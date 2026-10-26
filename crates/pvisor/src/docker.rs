//! Rootless, per-instance VM lifecycle for the Docker command profile.
//!
//! Storage is private to the current user. Each launch has a separate supervisor
//! and VMM subprocess; no global listener or privileged daemon is required.
#![deny(missing_docs)]
use anyhow::{Context, bail, ensure};
use fs2::FileExt;
use pvisor_guest::control::{self, Reply, Request, read_frame, write_frame};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::{
    fs::{MetadataExt, OpenOptionsExt},
    net::UnixStream,
    process::{CommandExt, ExitStatusExt},
};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub use crate::image::oci::PullProgress;

/// VM-isolated Dockerfile builds and OCI image publication.
pub mod build;
mod images;
/// Persistent Docker-style network management and IPAM.
pub mod network;
pub use network::{Network, NetworkCreate};

fn native_platform() -> String {
    let architecture = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    };
    format!("linux/{architecture}")
}

fn canonical_platform(platform: &str) -> anyhow::Result<&'static str> {
    match platform {
        "linux/amd64" => Ok("linux/amd64"),
        "linux/arm64" | "linux/arm64/v8" => Ok("linux/arm64"),
        _ => bail!("unsupported platform {platform}; expected linux/amd64 or linux/arm64"),
    }
}

fn require_native_platform(platform: &str) -> anyhow::Result<()> {
    let platform = canonical_platform(platform)?;
    let native = native_platform();
    ensure!(
        platform == native,
        "cannot run {platform} on a {native} VM: cross-architecture emulation is not implemented; --platform selects an image, not a CPU emulator"
    );
    Ok(())
}

/// Persistent image identity and launch defaults. Published records require a
/// V2 binding; the backing parameter also models private, unpublished build stages.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Image<Backing = CachedImage> {
    /// User-visible image reference, including its tag or digest selector.
    pub reference: String,
    /// OCI configuration digest, used as the Docker image ID.
    pub id: String,
    /// Selected platform-specific manifest digest.
    pub digest: String,
    /// Canonical OCI platform (`linux/amd64` or `linux/arm64`).
    pub platform: String,
    /// Immutable V2 binding for published images; private builds use a distinct backing type.
    pub cache: Backing,
    /// Image environment. Host environment is never implicitly inherited.
    pub env: BTreeMap<String, String>,
    /// Executable prefix from image configuration.
    pub entrypoint: Vec<String>,
    /// Default arguments from image configuration.
    pub cmd: Vec<String>,
    /// Absolute guest working directory.
    pub cwd: PathBuf,
    /// Guest user name or UID, optionally with a group.
    pub user: String,
    /// Image stop signal; validated before a launch is admitted.
    pub stop_signal: i32,
    /// Declared image volume destinations.
    pub volumes: Vec<PathBuf>,
    /// Catalog publication time in Unix milliseconds.
    pub created_ms: u64,
}

impl<Backing> Image<Backing> {
    fn map_cache<T>(self, map: impl FnOnce(Backing) -> T) -> Image<T> {
        Image {
            reference: self.reference,
            id: self.id,
            digest: self.digest,
            platform: self.platform,
            env: self.env,
            entrypoint: self.entrypoint,
            cmd: self.cmd,
            cwd: self.cwd,
            user: self.user,
            stop_signal: self.stop_signal,
            volumes: self.volumes,
            created_ms: self.created_ms,
            cache: map(self.cache),
        }
    }
}

/// A fixed cache revision; tags and later pulls cannot change its contents.
/// The host-only pool location must remain available while any image or stopped
/// container references it. Removing a catalog tag does not delete pool objects.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CachedImage {
    /// Absolute local pool directory, shared by images in this Docker store.
    pub location: PathBuf,
    /// Authenticated immutable `pvisor-v2` metadata revision handle.
    pub handle: String,
}

/// Owns a usable root directory and its lazy backend through file/VM access.
/// Dropping it removes the published image's temporary projection.
pub struct ImageRoot {
    root: PathBuf,
    _owner: Option<crate::image::cache::DirectImage>,
}
impl ImageRoot {
    /// Root accepted by OverlayCore/VM filesystem adapters. Pooled files must be
    /// accessed through those adapters; ordinary host reads see placeholders.
    pub fn rootfs(&self) -> &Path {
        &self.root
    }
}

/// One independently mounted host directory; sources are resolved at creation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mount {
    /// Existing canonical host directory, or a managed volume directory.
    pub source: PathBuf,
    /// Absolute guest destination, excluding guest system/control directories.
    pub target: PathBuf,
    /// Whether guest writes must fail at the mount boundary.
    pub read_only: bool,
    /// Named or anonymous managed volume; bind mounts use `None`.
    pub volume: Option<String>,
}

/// A published IPv4 TCP endpoint. Port zero requests an ephemeral host port.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Port {
    /// Host bind address. Docker-compatible omission means `0.0.0.0`.
    pub address: std::net::Ipv4Addr,
    /// Host port; resolved before the VM starts.
    pub host: u16,
    /// Guest service port, always nonzero.
    pub guest: u16,
}

/// Fully resolved launch request; validated before instance publication.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Create {
    /// Optional unique container name.
    pub name: Option<String>,
    /// Image reference or local image ID.
    pub image: String,
    /// Optional OCI platform assertion. Only the native Linux platform can run;
    /// unsupported or foreign platforms fail before pulling or publication.
    #[serde(default)]
    pub platform: Option<String>,
    /// CLI command overrides CMD; ENTRYPOINT is retained unless overridden.
    pub command: Vec<String>,
    /// `Some(empty)` explicitly clears the image entrypoint.
    pub entrypoint: Option<String>,
    /// Explicit environment overrides.
    pub env: BTreeMap<String, String>,
    /// Names explicitly unset by CLI environment forwarding.
    #[serde(default)]
    pub unset_env: Vec<String>,
    /// Optional absolute working-directory override.
    pub cwd: Option<PathBuf>,
    /// Optional guest identity override.
    pub user: Option<String>,
    /// Extra mounts.
    pub mounts: Vec<Mount>,
    /// Published TCP endpoints.
    pub ports: Vec<Port>,
    /// Guest vCPU count, not a host CPU quota.
    pub cpus: u8,
    /// Total guest RAM budget in MiB.
    pub memory_mib: u32,
    /// Optional same-user daemon pool socket, canonicalized at creation and
    /// reused on start. Linux negotiates physical/sealed storage; Apple Silicon
    /// uses experimental live-pool cold compression. An unavailable pool fails
    /// startup. On macOS pool loss terminates dependent VMs; no reconnect occurs.
    #[serde(default)]
    pub memory_pool: Option<PathBuf>,
    /// Optional private shared boot-kernel directory on Apple Silicon macOS.
    /// Equal firmware bytes map one read-only inode with private COW writes.
    /// Canonicalized at creation, retained across start, and excluded from guest
    /// rootfs/mounts. This shares initial kernel RAM, not application pages.
    /// May accompany memory_pool; its immutable pages are excluded from the
    /// cold pager. The directory/files must not be modified by the caller.
    #[serde(default)]
    pub shared_kernel_base: Option<PathBuf>,
    /// Automatically remove the instance and its anonymous volumes on exit.
    pub auto_remove: bool,
    /// Keep guest stdin open for input attachment.
    pub interactive: bool,
    /// Optional initial guest terminal dimensions, rows then columns.
    /// A terminal defaults TERM to xterm; image/explicit values and unsets win.
    pub tty: Option<[u16; 2]>,
    /// Install the current public IPv4 TCP/DNS egress profile.
    pub network: bool,
    /// Docker network selector. None retains the isolated SDK/build link;
    /// the CLI selects bridge by default. Resolved to an exact name at creation.
    #[serde(default)]
    pub network_name: Option<String>,
    /// Requested static IPv4; supported only on user-defined networks. Automatic
    /// allocation is retained in peers until container removal, including stops.
    #[serde(default)]
    pub ipv4_address: Option<std::net::Ipv4Addr>,
    /// Optional same-host packet network. Requires network=true; all members
    /// explicitly trust each other for IPv4 TCP, UDP and ICMP.
    #[serde(default)]
    pub peers: Option<crate::PeerNetworkSettings>,
}

/// Stable container identity plus the current launch generation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Instance<Backing = CachedImage> {
    /// Schema version; unknown versions refuse mutation.
    pub schema: u32,
    /// Full 64-hex container ID.
    pub id: String,
    /// Unique user-visible name.
    pub name: String,
    /// Image pinned at creation; tag updates never change this instance.
    pub image: Image<Backing>,
    /// Effective launch options.
    pub config: Create,
    /// Effective argv after entrypoint/CMD resolution.
    pub argv: Vec<String>,
    /// Effective launch environment.
    pub env: BTreeMap<String, String>,
    /// Effective guest working directory.
    pub cwd: PathBuf,
    /// Effective guest identity.
    pub user: String,
    /// `created`, `starting`, `running`, or `exited`.
    pub state: String,
    /// Unique launch generation; prevents stale control requests after restart.
    pub generation: String,
    /// Launch admission time, used only to reconcile abandoned startup records.
    #[serde(default)]
    pub started_ms: u64,
    /// Most recent process/VM exit status.
    pub exit_code: Option<i32>,
    /// Creation time in Unix milliseconds.
    pub created_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct Completion {
    generation: String,
    code: i32,
    #[serde(default)]
    error: Option<String>,
}

/// Open log descriptors surviving automatic container removal.
pub struct Output {
    stdout: File,
    stderr: File,
    result: PathBuf,
    directory: PathBuf,
}
impl Output {
    /// Drain output until this exact generation has completed, then return its
    /// exit code. Polling never interprets a previous generation as completion.
    pub fn follow(
        mut self,
        generation: &str,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> anyhow::Result<i32> {
        loop {
            io::copy(&mut self.stdout, stdout)?;
            io::copy(&mut self.stderr, stderr)?;
            stdout.flush()?;
            stderr.flush()?;
            if let Ok(file) = File::open(&self.result) {
                let result: Completion = serde_json::from_reader(file)?;
                if result.generation == generation {
                    io::copy(&mut self.stdout, stdout)?;
                    io::copy(&mut self.stderr, stderr)?;
                    return Ok(result.code);
                }
            }
            if !lease_active(&self.directory.join("owner.lock"))?
                && !lease_active(&self.directory.join("vm.lock"))?
            {
                io::copy(&mut self.stdout, stdout)?;
                io::copy(&mut self.stderr, stderr)?;
                return Ok(137);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// User-private catalog and storage facade. No VM runs in this object.
#[derive(Clone)]
pub struct Store {
    root: PathBuf,
    images: crate::image::store::LocalStore,
}

fn private_directory(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        let mut builder = fs::DirBuilder::new();
        use std::os::unix::fs::DirBuilderExt;
        builder.recursive(true).mode(0o700).create(path)?;
    }
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "container storage must be an owned private directory: {}",
        path.display()
    );
    Ok(())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}

fn uuid() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn signal(value: &str) -> anyhow::Result<i32> {
    if let Ok(value) = value.parse::<i32>() {
        ensure!((1..=64).contains(&value), "invalid signal");
        return Ok(value);
    }
    Ok(match value.strip_prefix("SIG").unwrap_or(value) {
        "TERM" | "" => 15,
        "KILL" => 9,
        "INT" => 2,
        "QUIT" => 3,
        "HUP" => 1,
        "USR1" => 10,
        "USR2" => 12,
        "WINCH" => 28,
        _ => bail!("unsupported image stop signal {value}"),
    })
}

impl Store {
    /// Open user-private container state and the common local image store.
    /// `PVISOR_DOCKER_HOME` selects container state; images use `PVISOR_IMAGE_STORE`,
    /// then that explicit Docker home's `images`, then the user data directory.
    /// Both roots must be owned private directories.
    pub fn discover() -> anyhow::Result<Self> {
        let root = std::env::var_os("PVISOR_DOCKER_HOME")
            .map(PathBuf::from)
            .unwrap_or(
                dirs::data_local_dir()
                    .context("no user data directory")?
                    .join("pvisor/docker"),
            );
        private_directory(&root)?;
        Self::open_with_images(root, crate::image::store::LocalStore::discover(None)?)
    }
    /// Open isolated storage for this UID, including images beneath `root/images`.
    /// Environment image defaults do not affect this explicit root. Symlink roots
    /// and shared permissions fail. Published records require V2 bindings.
    pub fn open(root: PathBuf) -> anyhow::Result<Self> {
        private_directory(&root)?;
        let images = crate::image::store::LocalStore::open(root.join("images"))?;
        Self::open_with_images(root, images)
    }

    fn open_with_images(
        root: PathBuf,
        images: crate::image::store::LocalStore,
    ) -> anyhow::Result<Self> {
        private_directory(&root)?;
        let root = root.canonicalize()?;
        for name in ["instances", "volumes", "images", "results"] {
            private_directory(&root.join(name))?;
        }
        let store = Self { root, images };
        store.initialize_networks()?;
        Ok(store)
    }
    fn lock(&self) -> anyhow::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.root.join("catalog.lock"))?;
        file.lock_exclusive()?;
        Ok(file)
    }
    fn catalog(&self) -> anyhow::Result<BTreeMap<String, Image>> {
        let path = self.root.join("images.json");
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        let records: Vec<Image> = serde_json::from_reader(File::open(path)?)?;
        let mut images = BTreeMap::new();
        for image in records {
            let key = image_reference_key(&image.reference)?;
            ensure!(
                images.insert(key.clone(), image).is_none(),
                "duplicate canonical image reference: {key}"
            );
        }
        Ok(images)
    }
    fn save_catalog(&self, images: &BTreeMap<String, Image>) -> anyhow::Result<()> {
        crate::util::write_private_json(
            &self.root.join("images.json"),
            &images.values().collect::<Vec<_>>(),
        )
    }
    fn local_image(&self, selector: &str) -> anyhow::Result<Option<Image>> {
        let _guard = self.lock()?;
        let images = self.catalog()?;
        if let Some(image) = images.get(&image_reference_key(selector)?) {
            return Ok(Some(image.clone()));
        }
        let mut candidates = images
            .values()
            .filter(|image| image_matches(&image.id, selector));
        let selected = candidates.next();
        ensure!(
            candidates.all(|image| selected.is_some_and(|first| first.id == image.id)),
            "ambiguous image ID: {selector}"
        );
        Ok(selected.cloned())
    }
    fn directory(&self, id: &str) -> PathBuf {
        self.root.join("instances").join(id)
    }
    fn save<Backing: Serialize>(&self, instance: &Instance<Backing>) -> anyhow::Result<()> {
        crate::util::write_private_json(
            &self.directory(&instance.id).join("instance.json"),
            instance,
        )
    }
    fn all(&self) -> anyhow::Result<Vec<Instance>> {
        let mut instances = Vec::new();
        for entry in fs::read_dir(self.root.join("instances"))? {
            let path = entry?.path().join("instance.json");
            if path.is_file() {
                let mut instance: Instance = serde_json::from_reader(File::open(path)?)?;
                ensure!(
                    instance.schema == 1
                        && instance.id.len() == 64
                        && instance.id.bytes().all(|c| c.is_ascii_hexdigit()),
                    "unknown container record"
                );
                if (instance.state == "running"
                    || instance.state == "starting"
                        && crate::unix_now_ms().saturating_sub(instance.started_ms) > 30_000)
                    && !self.active(&instance)?
                {
                    instance.state = "exited".into();
                    instance.exit_code = Some(137);
                }
                instances.push(instance);
            }
        }
        instances.sort_by_key(|i| std::cmp::Reverse(i.created_ms));
        Ok(instances)
    }
    fn active(&self, instance: &Instance) -> anyhow::Result<bool> {
        if !self.directory(&instance.id).exists() {
            return Ok(false);
        }
        Ok(
            lease_active(&self.directory(&instance.id).join("owner.lock"))?
                || lease_active(&self.directory(&instance.id).join("vm.lock"))?,
        )
    }
    /// List catalog images without contacting a registry.
    pub fn images(&self) -> anyhow::Result<Vec<Image>> {
        let _guard = self.lock()?;
        Ok(self.catalog()?.into_values().collect())
    }
    /// Resolve a native OCI image and refresh its reference. Publication is atomic.
    pub fn pull(&self, reference: &str) -> anyhow::Result<Image> {
        self.pull_with_progress(reference, &mut |_| {})
    }
    /// Pull with synchronous progress observations. The callback must return
    /// promptly and must not panic or reenter this store. Completion is reported
    /// by the result only; on failure the previous catalog entry is preserved.
    /// Terminal rendering belongs to the caller, and silent `pull` is unchanged.
    pub fn pull_with_progress(
        &self,
        reference: &str,
        progress: &mut dyn FnMut(PullProgress),
    ) -> anyhow::Result<Image> {
        self.pull_for_platform_with_progress(reference, &native_platform(), progress)
    }
    /// Pull `linux/amd64`, `linux/arm64`, or its `linux/arm64/v8` alias.
    /// A foreign image can be downloaded but cannot run without emulation,
    /// which this runtime does not provide. Invalid platforms fail before I/O.
    /// One platform is catalogued per reference: success replaces that reference
    /// atomically; existing instances retain their pinned images. Failure and
    /// callback contracts match `pull_with_progress`.
    pub fn pull_for_platform_with_progress(
        &self,
        reference: &str,
        platform: &str,
        progress: &mut dyn FnMut(PullProgress),
    ) -> anyhow::Result<Image> {
        let platform = canonical_platform(platform)?;
        let arch = platform.strip_prefix("linux/").unwrap();
        let reference = normalize_reference(reference);
        let reference = reference.as_str();
        progress(PullProgress::Waiting);
        let _pull = self.image_pull_lock(reference, arch)?;
        let staging = self.images.staging()?;
        let store = crate::image::oci::ImageStore::new(Some(staging.path().into()))?;
        let resolved = store.resolve_with_progress(reference, arch, progress)?;
        let cached =
            match crate::image::cache::published_local_image(&self.images.pool(), reference, arch)?
            {
                Some(crate::image::cache::Response::Prepared {
                    digest,
                    image_handle,
                    ..
                }) if digest == resolved.prepared.digest => Some(CachedImage {
                    location: self.images.pool(),
                    handle: image_handle,
                }),
                _ => None,
            };
        if cached.is_some() {
            resolved.report_cached(progress);
        } else {
            store.extract_resolved(&resolved, progress)?;
        }
        let id = resolved.config_digest;
        let prepared = resolved.prepared;
        ensure!(
            id.starts_with("sha256:")
                && id.len() == 71
                && id[7..].bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid image config digest"
        );
        let config_bytes = fs::read(staging.path().join("blobs/sha256").join(&id[7..]))?;
        let config: serde_json::Value = serde_json::from_slice(&config_bytes)?;
        let config = &config["config"];
        let cwd = config["WorkingDir"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("/");
        let image = Image {
            reference: reference.into(),
            id,
            digest: prepared.digest,
            platform: platform.into(),
            cache: (),
            env: prepared.env,
            entrypoint: prepared.entrypoint,
            cmd: prepared.cmd,
            cwd: cwd.into(),
            user: config["User"].as_str().unwrap_or("").into(),
            stop_signal: signal(config["StopSignal"].as_str().unwrap_or("SIGTERM"))?,
            volumes: config["Volumes"]
                .as_object()
                .map(|v| v.keys().map(PathBuf::from).collect())
                .unwrap_or_default(),
            created_ms: crate::unix_now_ms(),
        };
        let image = if let Some(cache) = cached {
            self.images.retain_config(&image.id, &config_bytes)?;
            image.map_cache(|()| cache)
        } else {
            self.pool_image(image, &prepared.rootfs, &store, &config_bytes)?
        };
        let _guard = self.lock()?;
        let mut images = self.catalog()?;
        images.insert(image_reference_key(reference)?, image.clone());
        self.save_catalog(&images)?;
        Ok(image)
    }
    /// Remove a local image reference. Images used by any instance are refused.
    /// Shared pool objects are not deleted here; existing
    /// containers retain their pinned revision even after forced tag removal.
    pub fn rmi(&self, selector: &str, force: bool) -> anyhow::Result<Vec<String>> {
        let _guard = self.lock()?;
        let mut images = self.catalog()?;
        let key = image_reference_key(selector)?;
        let by_reference = images.contains_key(&key);
        let selected: Vec<_> = images
            .iter()
            .filter(|(identity, image)| {
                if by_reference {
                    *identity == &key
                } else {
                    image_matches(&image.id, selector)
                }
            })
            .collect();
        ensure!(!selected.is_empty(), "No such image: {selector}");
        let ids: std::collections::BTreeSet<_> =
            selected.iter().map(|(_, image)| &image.id).collect();
        ensure!(
            by_reference || ids.len() == 1,
            "ambiguous image ID: {selector}"
        );
        let used = self
            .all()?
            .iter()
            .any(|instance| ids.contains(&instance.image.id));
        ensure!(force || !used, "image is used by an existing container");
        ensure!(
            by_reference || force || selected.len() == 1,
            "image has multiple tags; remove a tag or use --force"
        );
        let removed: Vec<_> = selected
            .iter()
            .map(|(_, image)| image.reference.clone())
            .collect();
        let keys: Vec<_> = selected.into_iter().map(|(key, _)| key.clone()).collect();
        for key in keys {
            images.remove(&key);
        }
        self.save_catalog(&images)?;
        Ok(removed)
    }
    /// List containers, reconciling vanished owners without signaling saved PIDs.
    pub fn instances(&self, all: bool) -> anyhow::Result<Vec<Instance>> {
        let _guard = self.lock()?;
        Ok(self
            .all()?
            .into_iter()
            .filter(|i| all || i.state == "running")
            .collect())
    }
    /// Resolve an exact name, full ID, or unambiguous ID prefix.
    pub fn resolve(&self, selector: &str) -> anyhow::Result<Instance> {
        let instances = self.all()?;
        if let Some(exact) = instances
            .iter()
            .find(|i| i.name == selector || i.id == selector)
        {
            return Ok(exact.clone());
        }
        let found: Vec<_> = instances
            .into_iter()
            .filter(|i| !selector.is_empty() && i.id.starts_with(selector))
            .collect();
        ensure!(
            found.len() == 1,
            "{} container: {selector}",
            if found.is_empty() {
                "No such"
            } else {
                "ambiguous"
            }
        );
        Ok(found.into_iter().next().unwrap())
    }
    /// Create a stopped instance. A missing image is pulled before publication.
    pub fn create(&self, config: Create) -> anyhow::Result<Instance> {
        self.create_with_progress(config, &mut |_| {})
    }
    /// Create an instance, observing preparation if its image must be pulled.
    /// Local images emit no events. Callback and failure contracts match
    /// `pull_with_progress`; no owner or VM is started by this operation.
    pub fn create_with_progress(
        &self,
        mut config: Create,
        progress: &mut dyn FnMut(PullProgress),
    ) -> anyhow::Result<Instance> {
        if let Some(platform) = &config.platform {
            require_native_platform(platform)?;
        }
        ensure!(
            config.cpus > 0 && config.memory_mib >= 16,
            "invalid VM CPU/memory budget"
        );
        ensure!(
            config.network || config.ports.is_empty(),
            "port publishing with --network none is unsupported"
        );
        if let Some(path) = &config.memory_pool {
            use std::os::unix::fs::FileTypeExt;
            ensure!(
                cfg!(any(
                    all(target_os = "macos", target_arch = "aarch64"),
                    all(target_os = "linux", target_arch = "x86_64")
                )),
                "memory pool requires Apple Silicon macOS or Linux x86-64"
            );
            let path = path.canonicalize().context("resolve memory pool socket")?;
            let parent = path.parent().context("memory pool parent missing")?;
            let owner = fs::symlink_metadata(parent)?;
            ensure!(
                owner.is_dir()
                    && owner.uid() == unsafe { libc::geteuid() }
                    && owner.mode() & 0o077 == 0,
                "memory pool directory must be private and owned by this user"
            );
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                metadata.file_type().is_socket() && metadata.uid() == unsafe { libc::geteuid() },
                "memory pool must be a same-user Unix socket"
            );
            config.memory_pool = Some(path);
        }
        if let Some(path) = &config.shared_kernel_base {
            ensure!(
                cfg!(all(target_os = "macos", target_arch = "aarch64")),
                "shared kernel baseline requires Apple Silicon macOS"
            );
            let owner = fs::symlink_metadata(path)?;
            ensure!(
                path.is_absolute()
                    && owner.is_dir()
                    && owner.uid() == unsafe { libc::geteuid() }
                    && owner.mode() & 0o077 == 0,
                "shared kernel directory must be absolute, owned and private"
            );
            config.shared_kernel_base = Some(path.canonicalize()?);
        }
        if let Some(name) = &config.name {
            ensure!(valid_name(name), "invalid container name");
        }
        if let Some(selector) = &config.network_name {
            ensure!(config.peers.is_none(), "conflicting network settings");
            self.inspect_network(selector)?;
        } else {
            ensure!(
                config.ipv4_address.is_none(),
                "--ip requires a user-defined network"
            );
        }
        let image = if let Some(image) = self.local_image(&config.image)? {
            image
        } else {
            self.pull_with_progress(&config.image, progress)?
        };
        require_native_platform(&image.platform)?;
        if config.memory_pool.is_some() || config.shared_kernel_base.is_some() {
            let image_root = self.open_image(&image)?;
            let rootfs = image_root.rootfs().canonicalize()?;
            if let Some(pool) = &config.memory_pool {
                let private = pool.parent().context("memory pool parent missing")?;
                ensure!(
                    !rootfs.starts_with(private) && !private.starts_with(&rootfs),
                    "image rootfs overlaps private memory pool authority"
                );
            }
            if let Some(base) = &config.shared_kernel_base {
                ensure!(
                    !rootfs.starts_with(base) && !base.starts_with(&rootfs),
                    "image rootfs overlaps shared kernel authority"
                );
            }
        }
        let argv = resolve_command(&image, &config);
        ensure!(
            !argv.is_empty() && !argv[0].is_empty(),
            "No command specified"
        );
        let cwd = config.cwd.clone().unwrap_or_else(|| image.cwd.clone());
        ensure!(cwd.is_absolute(), "working directory must be absolute");
        for mount in &config.mounts {
            ensure!(
                mount.source.is_absolute() && mount.source.is_dir(),
                "mount source must be an existing absolute directory"
            );
            validate_target(&mount.target)?;
            ensure!(
                !(mount.volume.is_some()
                    && mount.read_only
                    && fs::read_dir(&mount.source)?.next().is_none()),
                "initialize an empty managed volume with a writable mount before read-only use"
            );
            let source = mount.source.canonicalize()?;
            if let Some(base) = &config.shared_kernel_base {
                ensure!(
                    !source.starts_with(base) && !base.starts_with(&source),
                    "mount overlaps shared kernel authority"
                );
            }
            if let Some(pool) = &config.memory_pool {
                let private = pool.parent().context("memory pool parent missing")?;
                ensure!(
                    !source.starts_with(private) && !private.starts_with(&source),
                    "mount overlaps private memory pool authority"
                );
            }
            let authority = crate::host_transport::host_authority_root()?;
            let sockets = socket_directory()?;
            ensure!(
                !source.starts_with(&authority)
                    && !authority.starts_with(&source)
                    && !source.starts_with(&sockets)
                    && !sockets.starts_with(&source),
                "mount overlaps private control authority"
            );
            ensure!(
                !source.starts_with(&self.root) && !self.root.starts_with(&source)
                    || mount.volume.is_some() && source.starts_with(self.root.join("volumes")),
                "mount overlaps private container storage"
            );
            ensure!(
                !source.starts_with(&self.images.root) && !self.images.root.starts_with(&source),
                "mount overlaps private image storage"
            );
            ensure!(
                !source.starts_with(&image.cache.location)
                    && !image.cache.location.starts_with(&source),
                "mount overlaps pinned image storage"
            );
        }
        for port in &config.ports {
            ensure!(port.guest != 0, "guest port must be nonzero");
        }
        if let Some(peers) = &config.peers {
            peers.validate()?;
            ensure!(
                config.network,
                "peer networking conflicts with --network none"
            );
        }
        let _guard = self.lock()?;
        let id = format!("{}{}", uuid(), uuid());
        let name = config
            .name
            .clone()
            .unwrap_or_else(|| format!("pvisor_{}", &id[..12]));
        ensure!(
            !self.all()?.iter().any(|i| i.name == name),
            "container name already in use: {name}"
        );
        self.assign_network(&mut config, &name)?;
        let directory = self.directory(&id);
        private_directory(&directory)?;
        for path in ["upper", "work"] {
            private_directory(&directory.join(path))?;
        }
        for target in &image.volumes {
            if !config.mounts.iter().any(|m| &m.target == target) {
                validate_target(target)?;
                let name = uuid();
                let source = self.volume(&name)?;
                config.mounts.push(Mount {
                    source,
                    target: target.clone(),
                    read_only: false,
                    volume: Some(format!("anonymous:{name}")),
                });
            }
        }
        config
            .mounts
            .sort_by_key(|mount| mount.target.components().count());
        let mut targets = std::collections::BTreeSet::new();
        ensure!(
            config.mounts.iter().all(|m| targets.insert(&m.target)),
            "duplicate mount destination"
        );
        let mut env = image.env.clone();
        if config.tty.is_some() {
            env.entry("TERM".into()).or_insert_with(|| "xterm".into());
        }
        for key in &config.unset_env {
            env.remove(key);
        }
        env.extend(config.env.clone());
        env.entry("PATH".into()).or_insert_with(|| {
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into()
        });
        let user = config.user.clone().unwrap_or_else(|| image.user.clone());
        let instance = Instance {
            schema: 1,
            id,
            name,
            image,
            config,
            argv,
            env,
            cwd,
            user,
            state: "created".into(),
            generation: String::new(),
            started_ms: 0,
            exit_code: None,
            created_ms: crate::unix_now_ms(),
        };
        self.save(&instance)?;
        Ok(instance)
    }
    /// Open/create an owned managed volume beneath the private store.
    /// Guest-controlled modes are retained; volumes survive container removal.
    pub fn volume(&self, name: &str) -> anyhow::Result<PathBuf> {
        ensure!(valid_name(name), "invalid volume name");
        let path = self.root.join("volumes").join(name);
        use std::os::unix::fs::DirBuilderExt;
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        // Guest copy-up/chmod controls the volume mode; its parent stays private.
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_dir() && metadata.uid() == unsafe { libc::geteuid() },
            "managed volume must be an owned directory: {}",
            path.display()
        );
        Ok(path)
    }
    /// Launch a new generation; `start` never changes the pinned image or upper.
    /// Concurrent callers join an already-starting generation and observe its
    /// bounded readiness/completion result, without holding the catalog lock.
    pub fn start(&self, selector: &str) -> anyhow::Result<Instance> {
        let mut instance;
        {
            let _guard = self.lock()?;
            instance = self.resolve(selector)?;
            require_native_platform(&instance.image.platform)?;
            self.validate_network_attachment(&instance)?;
            if self.active(&instance)? && instance.state == "running" {
                return Ok(instance);
            }
            if instance.state != "starting" {
                ensure!(
                    !self.active(&instance)?,
                    "container is already running or starting"
                );
                instance.generation = uuid();
                instance.started_ms = crate::unix_now_ms();
                instance.state = "starting".into();
                instance.exit_code = None;
                self.save(&instance)?;
                let mut command = Command::new(std::env::current_exe()?);
                command
                    .arg("--pvisor-internal-docker-owner")
                    .arg(&self.root)
                    .arg(&instance.id)
                    .arg(&instance.generation)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::from(self.diagnostic(&instance)?));
                unsafe {
                    command.pre_exec(|| {
                        if libc::setsid() < 0 {
                            return Err(io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
                if let Err(error) = command.spawn() {
                    instance.state = "exited".into();
                    instance.exit_code = Some(125);
                    self.save(&instance)?;
                    return Err(error.into());
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        let ready = self
            .directory(&instance.id)
            .join(format!("ready-{}", instance.generation));
        loop {
            if ready.is_file() {
                return Ok(self.resolve(&instance.id).unwrap_or(instance));
            }
            if let Ok(result) = File::open(
                self.root
                    .join("results")
                    .join(format!("{}.json", instance.id)),
            ) {
                let result: Completion = serde_json::from_reader(result)?;
                if result.generation == instance.generation {
                    if let Some(error) = result.error {
                        bail!("container failed to start: {error}");
                    }
                    instance.state = "exited".into();
                    instance.exit_code = Some(result.code);
                    return Ok(self.resolve(&instance.id).unwrap_or(instance));
                }
            }
            let current = self.resolve(&instance.id)?;
            if current.state == "exited" {
                bail!(
                    "container failed to start: {}",
                    fs::read_to_string(self.directory(&instance.id).join("diagnostic.log"))
                        .unwrap_or_default()
                );
            }
            ensure!(
                Instant::now() < deadline,
                "container startup timed out; inspect logs before retrying"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn diagnostic(&self, instance: &Instance) -> anyhow::Result<File> {
        Ok(OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(self.directory(&instance.id).join("diagnostic.log"))?)
    }
    /// Gracefully stop the selected generation, escalating through its owner.
    /// No signal is sent to a persisted host PID.
    pub fn stop(
        &self,
        selector: &str,
        timeout: Duration,
        requested_signal: Option<&str>,
    ) -> anyhow::Result<()> {
        let instance = {
            let _guard = self.lock()?;
            self.resolve(selector)?
        };
        if !self.active(&instance)? {
            return Ok(());
        }
        let request = Request::Stop {
            signal: requested_signal
                .map(signal)
                .transpose()?
                .unwrap_or(instance.image.stop_signal),
        };
        let _ = self.request(&instance, &request);
        let deadline = Instant::now() + timeout;
        while self.active(&instance)? {
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        {
            // Auto-removal uses the same lock; never publish into a removed instance.
            let _guard = self.lock()?;
            if !self.active(&instance)? {
                return Ok(());
            }
            crate::util::write_private_json(
                &self.directory(&instance.id).join("force-stop.json"),
                &instance.generation,
            )?;
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.directory(&instance.id).exists() && self.active(&instance)? {
            ensure!(
                Instant::now() < deadline,
                "owner did not confirm VM termination"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }
    /// Delete a stopped container. Named volumes always survive; `volumes` also
    /// removes its anonymous volumes. Force termination must be requested first.
    pub fn remove(&self, selector: &str, volumes: bool) -> anyhow::Result<()> {
        let _guard = self.lock()?;
        let instance = self.resolve(selector)?;
        ensure!(
            !self.active(&instance)? && instance.state != "starting",
            "cannot remove a running container"
        );
        fs::remove_dir_all(self.directory(&instance.id))?;
        if volumes {
            for mount in &instance.config.mounts {
                if let Some(name) = mount
                    .volume
                    .as_deref()
                    .and_then(|n| n.strip_prefix("anonymous:"))
                    && !self
                        .all()?
                        .iter()
                        .any(|other| other.config.mounts.iter().any(|m| m.source == mount.source))
                {
                    fs::remove_dir_all(self.root.join("volumes").join(name))?;
                }
            }
        }
        Ok(())
    }
    /// Open one current-generation guest control connection.
    pub fn connect(&self, instance: &Instance) -> anyhow::Result<UnixStream> {
        ensure!(self.active(instance)?, "container is not running");
        let stream = UnixStream::connect(socket(instance)?)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        Ok(stream)
    }
    fn request(&self, instance: &Instance, request: &Request) -> anyhow::Result<Reply> {
        let mut stream = self.connect(instance)?;
        write_frame(&mut stream, request)?;
        let reply = read_frame(&mut stream)?;
        if let Reply::Error { message } = &reply {
            bail!("{message}");
        }
        Ok(reply)
    }
    /// Open output before starting a container, retaining descriptors through
    /// automatic removal. The completion receipt is outside guest-visible trees.
    pub fn output(&self, instance: &Instance) -> anyhow::Result<Output> {
        let open = |name| -> anyhow::Result<File> {
            Ok(OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(self.directory(&instance.id).join(name))?)
        };
        Ok(Output {
            stdout: open("stdout.log")?,
            stderr: open("stderr.log")?,
            directory: self.directory(&instance.id),
            result: self
                .root
                .join("results")
                .join(format!("{}.json", instance.id)),
        })
    }
    /// Stream the current or previous launch's stdout/stderr. The caller owns
    /// terminal rendering; following ends after owner teardown and a final drain.
    pub fn logs(
        &self,
        selector: &str,
        follow: bool,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> anyhow::Result<i32> {
        let instance = self.resolve(selector)?;
        let directory = self.directory(&instance.id);
        let mut out = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(directory.join("stdout.log"))?;
        let mut err = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(directory.join("stderr.log"))?;
        loop {
            io::copy(&mut out, stdout)?;
            io::copy(&mut err, stderr)?;
            stdout.flush()?;
            stderr.flush()?;
            let active = directory.exists() && self.active(&instance)?;
            if !follow || !active {
                io::copy(&mut out, stdout)?;
                io::copy(&mut err, stderr)?;
                return Ok(self
                    .resolve(&instance.id)
                    .ok()
                    .and_then(|i| i.exit_code)
                    .unwrap_or(0));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn lease_file(path: &Path) -> anyhow::Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?)
}
fn acquire_lease(path: &Path) -> anyhow::Result<File> {
    let file = lease_file(path)?;
    file.try_lock_exclusive()?;
    Ok(file)
}
fn lease_active(path: &Path) -> anyhow::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let file = match lease_file(path) {
        Ok(file) => file,
        Err(_error) if !path.exists() => return Ok(false),
        Err(error) => return Err(error),
    };
    match file.try_lock_exclusive() {
        Ok(()) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error.into()),
    }
}

fn image_reference_key(reference: &str) -> anyhow::Result<String> {
    crate::image::oci::cache_reference(reference).map(|(key, _)| key)
}

fn normalize_reference(reference: &str) -> String {
    if reference.contains('@')
        || reference
            .rsplit('/')
            .next()
            .is_some_and(|tail| tail.contains(':'))
    {
        reference.into()
    } else {
        format!("{reference}:latest")
    }
}

fn image_matches(id: &str, selector: &str) -> bool {
    let selector = selector.strip_prefix("sha256:").unwrap_or(selector);
    selector.len() >= 4
        && selector.bytes().all(|c| c.is_ascii_hexdigit())
        && id.trim_start_matches("sha256:").starts_with(selector)
}

fn resolve_command(image: &Image, config: &Create) -> Vec<String> {
    let mut argv = match &config.entrypoint {
        Some(s) if s.is_empty() => Vec::new(),
        Some(s) => vec![s.clone()],
        None => image.entrypoint.clone(),
    };
    if !config.command.is_empty() {
        argv.extend(config.command.clone());
    } else if config.entrypoint.is_none() {
        argv.extend(image.cmd.clone());
    }
    argv
}

fn validate_target(target: &Path) -> anyhow::Result<()> {
    ensure!(
        target.is_absolute()
            && target != Path::new("/")
            && !target
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "invalid mount destination"
    );
    for reserved in ["/proc", "/sys", "/dev", "/.pvisor-guest.json"] {
        ensure!(
            !target.starts_with(reserved),
            "mount covers a guest system path"
        );
    }
    Ok(())
}

fn socket_directory() -> anyhow::Result<PathBuf> {
    let directory =
        PathBuf::from("/tmp").join(format!("pvisor-docker-{}", unsafe { libc::geteuid() }));
    private_directory(&directory)?;
    Ok(directory.canonicalize()?)
}

fn socket<Backing>(instance: &Instance<Backing>) -> anyhow::Result<PathBuf> {
    ensure!(
        instance.generation.len() == 32
            && instance.generation.bytes().all(|c| c.is_ascii_hexdigit()),
        "invalid launch generation"
    );
    Ok(socket_directory()?.join(format!(
        "{}-{}.sock",
        &instance.id[..12],
        &instance.generation[..8]
    )))
}

/// Dispatch private self-exec modes before threads, Tokio, or normal CLI parsing.
/// Normal invocations return false. Private modes validate saved identities and
/// generations; build stages use a distinct unpublished backing type.
/// A VMM mode consumes its process on normal guest exit.
pub fn internal_if_requested() -> anyhow::Result<bool> {
    let args: Vec<_> = std::env::args_os().collect();
    let Some(mode) = args.get(1).and_then(|s| s.to_str()) else {
        return Ok(false);
    };
    if !matches!(
        mode,
        "--pvisor-internal-docker-owner"
            | "--pvisor-internal-docker-vm"
            | "--pvisor-internal-docker-build-vm"
    ) {
        return Ok(false);
    }
    ensure!(args.len() == 5, "invalid private container invocation");
    let store = Store::open(PathBuf::from(&args[2]))?;
    if mode == "--pvisor-internal-docker-build-vm" {
        build::run_internal(
            &store,
            args[3].to_str().context("invalid ID")?,
            args[4].to_str().context("invalid generation")?,
        )?;
        return Ok(true);
    }
    let instance = store.resolve(args[3].to_str().context("invalid ID")?)?;
    ensure!(
        instance.generation == args[4].to_str().context("invalid generation")?,
        "stale container generation"
    );
    if mode.ends_with("-owner") {
        owner(&store, instance)?;
    } else {
        let lower = store.open_image(&instance.image)?;
        run_vm(&store, instance, lower)?;
    }
    Ok(true)
}

struct ChildGuard(std::process::Child);
impl std::ops::Deref for ChildGuard {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn owner(store: &Store, mut instance: Instance) -> anyhow::Result<()> {
    let directory = store.directory(&instance.id);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("owner.lock"))?;
    lock.try_lock_exclusive()
        .context("container already has an owner")?;
    let result = (|| -> anyhow::Result<i32> {
        let mut listeners = Vec::new();
        for port in &mut instance.config.ports {
            let listener = std::net::TcpListener::bind((port.address, port.host))?;
            port.host = listener.local_addr()?.port();
            listeners.push((listener, port.guest));
        }
        {
            let _guard = store.lock()?;
            store.save(&instance)?;
        }
        let stdout = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(directory.join("stdout.log"))?;
        let stderr = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(directory.join("stderr.log"))?;
        let child = Command::new(std::env::current_exe()?)
            .arg("--pvisor-internal-docker-vm")
            .arg(&store.root)
            .arg(&instance.id)
            .arg(&instance.generation)
            .stdin(Stdio::piped())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()?;
        let mut child = ChildGuard(child);
        let endpoint = socket(&instance)?;
        let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for (listener, port) in listeners {
            let endpoint = endpoint.clone();
            let connections = connections.clone();
            std::thread::spawn(move || {
                for client in listener.incoming().flatten() {
                    use std::sync::atomic::Ordering;
                    if connections.fetch_add(1, Ordering::AcqRel) >= 32 {
                        connections.fetch_sub(1, Ordering::AcqRel);
                        continue;
                    }
                    let endpoint = endpoint.clone();
                    let connections = connections.clone();
                    std::thread::spawn(move || {
                        let _ = forward(client, &endpoint, port);
                        connections.fetch_sub(1, Ordering::AcqRel);
                    });
                }
            });
        }
        loop {
            if let Some(status) = child.try_wait()? {
                ensure!(
                    directory
                        .join(format!("boot-{}", instance.generation))
                        .is_file(),
                    "VM failed to start: {}",
                    fs::read_to_string(directory.join("stderr.log")).unwrap_or_default()
                );
                return Ok(status.code().unwrap_or(128 + status.signal().unwrap_or(9)));
            }
            if instance.state == "starting"
                && endpoint.exists()
                && let Ok(Reply::Ready) = store.request(&instance, &Request::Ping {})
            {
                instance.state = "running".into();
                let _guard = store.lock()?;
                store.save(&instance)?;
                fs::write(
                    directory.join(format!("ready-{}", instance.generation)),
                    b"ready",
                )?;
            }
            let force = directory.join("force-stop.json");
            if force.is_file()
                && serde_json::from_reader::<_, String>(File::open(&force)?)
                    .ok()
                    .as_deref()
                    == Some(&instance.generation)
            {
                child.kill()?;
                let _ = child.wait()?;
                return Ok(137);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    })();
    let (code, error) = match result {
        Ok(code) => (code, None),
        Err(error) => {
            writeln!(store.diagnostic(&instance)?, "{error:#}")?;
            (125, Some(format!("{error:#}")))
        }
    };
    let _guard = store.lock()?;
    crate::util::write_private_json(
        &store
            .root
            .join("results")
            .join(format!("{}.json", instance.id)),
        &Completion {
            generation: instance.generation.clone(),
            code,
            error,
        },
    )?;
    instance.state = "exited".into();
    instance.exit_code = Some(code);
    store.save(&instance)?;
    let _ = fs::remove_file(socket(&instance)?);
    let auto_remove = instance.config.auto_remove;
    drop(_guard);
    drop(lock);
    if auto_remove {
        store.remove(&instance.id, true)?;
    }
    // Detached owners have no frontend to reap them; the OS adopts them.
    std::process::exit(0)
}

fn forward(mut client: std::net::TcpStream, endpoint: &Path, port: u16) -> anyhow::Result<()> {
    let mut stream = UnixStream::connect(endpoint)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    write_frame(&mut stream, &Request::Connect { port })?;
    ensure!(
        matches!(read_frame::<Reply>(&mut stream)?, Reply::Ready),
        "guest connection refused"
    );
    stream.set_read_timeout(None)?;
    let mut input = client.try_clone()?;
    let mut writer = stream.try_clone()?;
    let upstream = std::thread::spawn(move || -> io::Result<()> {
        let result = (|| {
            let mut bytes = [0; 16384];
            loop {
                let size = input.read(&mut bytes)?;
                if size == 0 {
                    // An orderly half-close still allows the guest's reply.
                    return write_frame(&mut writer, &control::StdinFrame::Eof {});
                }
                write_frame(
                    &mut writer,
                    &control::StdinFrame::Data {
                        bytes: bytes[..size].to_vec(),
                    },
                )?;
            }
        })();
        if result.is_err() {
            // RST and failed input writes must wake the downstream reader now,
            // rather than occupying a publication slot until the guest times out.
            let _ = writer.shutdown(std::net::Shutdown::Both);
        }
        result
    });
    let result = (|| -> anyhow::Result<()> {
        loop {
            match read_frame::<Reply>(&mut stream)? {
                Reply::Output { stream: 1, bytes } => client.write_all(&bytes)?,
                Reply::Exit { .. } => return Ok(()),
                _ => bail!("invalid guest TCP tunnel response"),
            }
        }
    })();
    let _ = client.shutdown(std::net::Shutdown::Both);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = upstream.join();
    result
}

fn prepare_resolver(
    rootfs: &Path,
    resolver: &Path,
    gateway: std::net::Ipv4Addr,
) -> anyhow::Result<()> {
    use pvisor_overlay_core::{backend::UnixIdentity, sys};
    use std::ffi::OsStr;

    private_directory(resolver)?;
    private_directory(&resolver.join("etc"))?;
    fs::write(
        resolver.join("etc/resolv.conf"),
        format!("nameserver {gateway}\n").as_bytes(),
    )?;
    let key = OsStr::new("user.containers.override_stat");
    // Host storage stays 0700; its guest directories retain the image's metadata.
    // Otherwise this first lower makes guest / and /etc inaccessible after setuid.
    for relative in ["", "etc"] {
        let source = rootfs.join(relative);
        // Lazy local lowers project attributes before native metadata access.
        let prepared = pvisor_overlay_core::backend::prepare_metadata(&source);
        if let Err(error) = prepared
            && (relative != "etc" || error.kind() != io::ErrorKind::NotFound)
        {
            return Err(error.into());
        }
        let identity = match fs::symlink_metadata(&source) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_dir(),
                    "resolver parent is not an image directory: {}",
                    source.display()
                );
                if sys::list_xattrs(&source)?
                    .iter()
                    .any(|name| name == key.as_encoded_bytes())
                {
                    let value = sys::get_xattr(&source, key)?;
                    UnixIdentity::parse(&value)?;
                    value
                } else {
                    format!(
                        "{}:{}:0{:o}",
                        metadata.uid(),
                        metadata.gid(),
                        metadata.mode()
                    )
                    .into_bytes()
                }
            }
            Err(error) if relative == "etc" && error.kind() == io::ErrorKind::NotFound => {
                b"0:0:040755".to_vec()
            }
            Err(error) => return Err(error.into()),
        };
        sys::set_xattr(&resolver.join(relative), key, &identity, 0)?;
    }
    sys::set_xattr(&resolver.join("etc/resolv.conf"), key, b"0:0:0100644", 0)?;
    Ok(())
}

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
fn run_vm<Backing>(
    store: &Store,
    instance: Instance<Backing>,
    image_root: ImageRoot,
) -> anyhow::Result<()> {
    use pvisor_vm::api::{
        OverlayConfig, PermissionSemantics, RuntimeSupport, VmBuilder, VmConfiguration, VmPlatform,
        VmRuntime,
    };
    require_native_platform(&instance.image.platform)?;
    if std::env::var_os("PVISOR_KRUN_LOG").is_some() {
        VmPlatform::init_logging("debug");
    }
    let _lease = acquire_lease(&store.directory(&instance.id).join("vm.lock"))?;
    let endpoint = socket(&instance)?;
    let mut vm = VmBuilder::new(instance.config.cpus, instance.config.memory_mib)?;
    if VmPlatform::embedded_kernel().is_none() {
        vm.set_firmware_path(VmPlatform::resolve_firmware_path(None)?)?;
    }
    if let Some(directory) = &instance.config.shared_kernel_base {
        vm.shared_kernel_base(directory.clone())?;
    }
    let directory = store.directory(&instance.id);
    // HVF cold blocks require a private file backing so discarded ranges can
    // be punched out. Linux's pager instead requires private anonymous RAM.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if instance.config.memory_pool.is_some() {
        vm.ram_backing(tempfile::tempfile_in(&directory)?)?;
    }
    let mut lowers = vec![image_root.rootfs().to_owned()];
    if instance.config.network {
        let resolver = directory.join(format!("resolver-{}", instance.generation));
        prepare_resolver(
            image_root.rootfs(),
            &resolver,
            instance
                .config
                .peers
                .as_ref()
                .map_or(pvisor_overlaynet::vm::ROUTER_IPV4, |p| p.gateway),
        )?;
        lowers.insert(0, resolver);
    }
    vm.overlay(
        "/dev/root",
        OverlayConfig {
            lower_mutability: vec![pvisor_overlay_core::LayerMutability::Mutable; lowers.len()],
            lower_dirs: lowers,
            upper_dir: directory.join("upper"),
            work_dir: Some(directory.join("work")),
            preimage_dir: None,
            apply_target: None,
            baseline_lower: None,
            baseline_content_index: None,
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            semantics: PermissionSemantics::LinuxComplete,
        },
        0,
    )?;
    // A private pipe makes owner death terminate the VMM, without a global daemon.
    std::thread::spawn(|| {
        let mut byte = [0; 1];
        let _ = std::io::Read::read(&mut io::stdin(), &mut byte);
        unsafe {
            libc::_exit(137);
        }
    });
    let mut guest = pvisor_guest::GuestConfig {
        argv: instance.argv,
        env: instance.env,
        cwd: instance.cwd,
        stdio_ports: Some([false, true, true]),
        process: Some(pvisor_guest::ProcessConfig {
            user: Some(instance.user),
            // Docker's default bounding set is a host-side compatibility policy.
            capabilities: Some(vec![0, 1, 3, 4, 5, 6, 7, 8, 10, 13, 18, 27, 29, 31]),
            new_session: true,
            stdin: if instance.config.interactive {
                pvisor_guest::StdinMode::Pipe
            } else {
                pvisor_guest::StdinMode::Closed
            },
            tty: instance.config.tty,
            create_workdir: true,
        }),
        control: true,
        ..Default::default()
    };
    for (index, mount) in instance.config.mounts.iter().enumerate() {
        let tag = format!("pvisor-volume-{index}");
        if mount.read_only {
            vm.read_only_filesystem(&tag, &mount.source)?;
        } else {
            vm.filesystem(&tag, &mount.source, 0)?;
        }
        guest.mounts.push(pvisor_guest::Mount {
            tag,
            target: mount.target.clone(),
            read_only: mount.read_only,
            initialize: mount.volume.is_some(),
        });
    }
    let _network = if instance.config.network {
        use pvisor_overlaynet::{
            BandwidthRegistry, EgressContext, EgressRuntime,
            vm::{VmNetwork, VmNetworkConfig},
        };
        let policy = pvisor_core::network::NetworkPolicy::compile(&Default::default())?;
        let egress = if instance.config.peers.is_some() {
            EgressRuntime::for_peer_network(policy, BandwidthRegistry::default())?
        } else {
            EgressRuntime::with_bandwidth_registry(
                policy,
                std::sync::Arc::new(pvisor_core::PolicyControlController),
                BandwidthRegistry::default(),
            )
        };
        let mut config = VmNetworkConfig::new(
            egress,
            EgressContext {
                run_id: Some(instance.id.clone()),
                attempt_id: Some(instance.generation.clone()),
            },
        );
        config.peer_network = instance
            .config
            .peers
            .as_ref()
            .map(crate::PeerNetworkSettings::resolve)
            .transpose()?;
        let address = config
            .peer_network
            .as_ref()
            .map_or(pvisor_overlaynet::vm::GUEST_IPV4, |peer| peer.address);
        let gateway = config
            .peer_network
            .as_ref()
            .map_or(pvisor_overlaynet::vm::ROUTER_IPV4, |p| p.gateway);
        let (network, stream) = VmNetwork::start(config)?;
        vm.network(stream, pvisor_overlaynet::vm::guest_mac(address))?;
        guest.network = Some(pvisor_guest::NetworkConfig {
            address: address.octets(),
            gateway: gateway.octets(),
        });
        Some(network)
    } else {
        None
    };
    vm.virtual_file(
        "/dev/root",
        pvisor_guest::CONFIG_PATH,
        serde_json::to_vec(&guest)?,
        0o400,
        true,
    )?;
    vm.vsock_port(control::CONTROL_PORT, endpoint, true)?;
    vm.task_stdio(-1, 1, 2)?;
    let ready = directory.join(format!("boot-{}", instance.generation));
    vm.run(move |handle| {
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ))]
        {
            crate::executor::vm::start_diagnostics(&handle)?;
            if let Some(pool) = &instance.config.memory_pool {
                crate::executor::vm::start_with_pool(
                    handle,
                    pool,
                    std::env::var_os("PVISOR_EXPERIMENTAL_MEMORY_METRICS").is_some(),
                )?;
            }
        }
        fs::write(ready, b"ready")
    })?;
    Ok(())
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
fn run_vm<Backing>(_: &Store, _: Instance<Backing>, _: ImageRoot) -> anyhow::Result<()> {
    bail!("container VMs require Apple Silicon macOS or Linux x86_64")
}

/// Shared guest protocol for CLI stream adapters.
pub use pvisor_guest::control::{
    Reply as GuestReply, Request as GuestRequest, StdinFrame as GuestStdin,
    read_frame as read_guest_frame, write_frame as write_guest_frame,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_published_tcp_reset_cancels_the_guest_tunnel() {
        use std::os::fd::AsRawFd;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("guest.sock");
        let guest = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = guest.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            assert!(matches!(
                read_frame::<Request>(&mut stream).unwrap(),
                Request::Connect { port: 8080 }
            ));
            write_frame(&mut stream, &Reply::Ready).unwrap();
            assert!(matches!(
                read_frame::<control::StdinFrame>(&mut stream).unwrap(),
                control::StdinFrame::Data { .. }
            ));
            sent.send(()).unwrap();
            let error = read_frame::<control::StdinFrame>(&mut stream).unwrap_err();
            // Release a broken implementation's downstream reader before asserting.
            let _ = write_frame(&mut stream, &Reply::Exit { code: 0 });
            error.kind()
        });
        let forwarding = std::thread::spawn(move || forward(accepted, &path, 8080));
        client.write_all(b"GET /unfinished HTTP/1.1\r\n").unwrap();
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        let linger = libc::linger {
            l_onoff: 1,
            l_linger: 0,
        };
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    client.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_LINGER,
                    (&linger as *const libc::linger).cast(),
                    std::mem::size_of_val(&linger) as _,
                )
            },
            0
        );
        drop(client);
        let kind = server.join().unwrap();
        let _ = forwarding.join().unwrap();
        assert!(
            matches!(
                kind,
                io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset
            ),
            "reset did not cancel tunnel: {kind:?}"
        );
    }

    #[test]
    fn resolver_preserves_guest_directory_metadata_without_exposing_host_storage() {
        use pvisor_overlay_core::{backend::UnixIdentity, sys};
        use std::ffi::OsStr;
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let rootfs = temporary.path().join("rootfs");
        fs::create_dir(&rootfs).unwrap();
        fs::set_permissions(&rootfs, fs::Permissions::from_mode(0o755)).unwrap();
        let key = OsStr::new("user.containers.override_stat");
        for custom in [false, true] {
            if custom {
                fs::create_dir(rootfs.join("etc")).unwrap();
                sys::set_xattr(&rootfs, key, b"123:456:040750", 0).unwrap();
                sys::set_xattr(&rootfs.join("etc"), key, b"234:567:040710", 0).unwrap();
            }
            let resolver = temporary.path().join(format!("resolver-{custom}"));
            prepare_resolver(&rootfs, &resolver, pvisor_overlaynet::vm::ROUTER_IPV4).unwrap();
            let root = UnixIdentity::parse(&sys::get_xattr(&resolver, key).unwrap()).unwrap();
            let etc =
                UnixIdentity::parse(&sys::get_xattr(&resolver.join("etc"), key).unwrap()).unwrap();
            if custom {
                assert_eq!((root.uid, root.gid, root.mode), (123, 456, 0o40750));
                assert_eq!((etc.uid, etc.gid, etc.mode), (234, 567, 0o40710));
            } else {
                let native = fs::symlink_metadata(&rootfs).unwrap();
                assert_eq!(
                    (root.uid, root.gid, root.mode),
                    (native.uid(), native.gid(), native.mode())
                );
                assert_eq!((etc.uid, etc.gid, etc.mode), (0, 0, 0o40755));
            }
            for path in [&resolver, &resolver.join("etc")] {
                assert_eq!(fs::symlink_metadata(path).unwrap().mode() & 0o777, 0o700);
            }
            assert_eq!(
                fs::read(resolver.join("etc/resolv.conf")).unwrap(),
                b"nameserver 192.0.2.1\n"
            );
            let file = UnixIdentity::parse(
                &sys::get_xattr(&resolver.join("etc/resolv.conf"), key).unwrap(),
            )
            .unwrap();
            assert_eq!((file.uid, file.gid, file.mode), (0, 0, 0o100644));
        }
    }

    fn fixture() -> (tempfile::TempDir, Store, Image) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("docker")).unwrap();
        let context = directory.path().join("context");
        fs::create_dir(&context).unwrap();
        fs::write(context.join("Dockerfile"), "FROM scratch\nENV IMAGE_ENV=image\nENTRYPOINT [\"/entry\"]\nCMD [\"default\"]\nWORKDIR /work\nUSER 1000:1000\n").unwrap();
        let image = build::build(
            &store,
            &build::BuildOptions {
                context: context.clone(),
                file: context.join("Dockerfile"),
                tags: vec!["fixture:latest".into()],
                build_args: BTreeMap::new(),
                target: None,
                platform: None,
                pull: false,
                cpus: 1,
                memory_mib: 32,
                network: false,
                timeout: Duration::from_secs(30),
                quiet: true,
                output: None,
            },
        )
        .unwrap();
        (directory, store, image)
    }
    fn request() -> Create {
        Create {
            name: Some("test".into()),
            image: "fixture:latest".into(),
            platform: None,
            command: Vec::new(),
            entrypoint: None,
            env: BTreeMap::new(),
            unset_env: Vec::new(),
            cwd: None,
            user: None,
            mounts: Vec::new(),
            ports: Vec::new(),
            cpus: 1,
            memory_mib: 128,
            memory_pool: None,
            shared_kernel_base: None,
            auto_remove: false,
            interactive: false,
            tty: None,
            network: false,
            network_name: None,
            ipv4_address: None,
            peers: None,
        }
    }
    #[test]
    fn docker_network_addresses_are_reserved_across_stops_and_released_on_removal() {
        let (_directory, store, _) = fixture();
        let net = store
            .create_network(NetworkCreate {
                name: "dev".into(),
                subnet: Some("172.22.0.0/24".into()),
                gateway: None,
            })
            .unwrap();
        let config = |name: &str, ip: Option<&str>| {
            let mut cfg = request();
            cfg.name = Some(name.into());
            cfg.network = true;
            cfg.network_name = Some("dev".into());
            cfg.ipv4_address = ip.map(|ip| ip.parse().unwrap());
            cfg
        };
        let a = store.create(config("web", None)).unwrap();
        assert!(store.create(config("WEB", None)).is_err());
        let b = store.create(config("client", None)).unwrap();
        assert_eq!(
            a.config.peers.as_ref().unwrap().address,
            "172.22.0.2".parse::<std::net::Ipv4Addr>().unwrap()
        );
        assert_ne!(
            a.config.peers.as_ref().unwrap().address,
            b.config.peers.as_ref().unwrap().address
        );
        assert!(
            store
                .create(config("duplicate", Some("172.22.0.2")))
                .is_err()
        );
        assert!(store.create(config("outside", Some("172.23.0.2"))).is_err());
        assert!(store.create(config("router", Some("172.22.0.1"))).is_err());
        assert!(store.remove_network("dev", true).is_err());
        assert_eq!(store.network_instances(&net.id).unwrap().len(), 2);
        store.remove(&a.id, false).unwrap();
        let c = store.create(config("replacement", None)).unwrap();
        assert_eq!(
            c.config.peers.as_ref().unwrap().address,
            "172.22.0.2".parse::<std::net::Ipv4Addr>().unwrap()
        );
        store.remove(&b.id, false).unwrap();
        store.remove(&c.id, false).unwrap();
        store.remove_network("dev", false).unwrap();
    }

    #[test]
    fn docker_network_concurrent_creation_assigns_unique_addresses() {
        let (_directory, store, _) = fixture();
        store
            .create_network(NetworkCreate {
                name: "dev".into(),
                subnet: None,
                gateway: None,
            })
            .unwrap();
        let workers: Vec<_> = (0..8)
            .map(|i| {
                let store = store.clone();
                std::thread::spawn(move || {
                    let mut cfg = request();
                    cfg.name = Some(format!("worker-{i}"));
                    cfg.network = true;
                    cfg.network_name = Some("dev".into());
                    store.create(cfg).unwrap().config.peers.unwrap().address
                })
            })
            .collect();
        let addresses: std::collections::BTreeSet<_> =
            workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert_eq!(addresses.len(), 8);
    }

    #[test]
    fn docker_memory_pool_is_persisted_private_and_optional_in_requests() {
        use std::os::unix::fs::PermissionsExt;
        let (directory, store, _) = fixture();
        let private = directory.path().join("pool");
        fs::create_dir(&private).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = private.join("pool.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let mut config = request();
        config.memory_pool = Some(socket.clone());
        let mut exposed = config.clone();
        exposed.mounts.push(Mount {
            source: directory.path().canonicalize().unwrap(),
            target: "/host".into(),
            read_only: false,
            volume: None,
        });
        assert!(
            store
                .create(exposed)
                .unwrap_err()
                .to_string()
                .contains("memory pool authority")
        );
        fs::set_permissions(&private, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            store
                .create(config.clone())
                .unwrap_err()
                .to_string()
                .contains("private")
        );
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        let instance = store.create(config).unwrap();
        let expected = Some(socket.canonicalize().unwrap());
        assert_eq!(instance.config.memory_pool, expected);
        assert_eq!(
            store.resolve(&instance.id).unwrap().config.memory_pool,
            expected
        );
        let mut legacy = serde_json::to_value(&instance.config).unwrap();
        legacy.as_object_mut().unwrap().remove("memory_pool");
        legacy.as_object_mut().unwrap().remove("shared_kernel_base");
        let restored: Create = serde_json::from_value(legacy).unwrap();
        assert!(restored.shared_kernel_base.is_none());
        assert!(restored.memory_pool.is_none());
    }
    #[test]
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn docker_kernel_base_persists_and_cannot_be_exposed_to_guest() {
        use std::os::unix::fs::PermissionsExt;
        let (directory, store, _) = fixture();
        let bases = directory.path().join("bases");
        fs::create_dir(&bases).unwrap();
        fs::set_permissions(&bases, fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = request();
        config.shared_kernel_base = Some(bases.clone());
        let mut exposed = config.clone();
        exposed.mounts.push(Mount {
            source: directory.path().canonicalize().unwrap(),
            target: "/host".into(),
            read_only: false,
            volume: None,
        });
        assert!(
            store
                .create(exposed)
                .unwrap_err()
                .to_string()
                .contains("shared kernel authority")
        );
        fs::set_permissions(&bases, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            store
                .create(config.clone())
                .unwrap_err()
                .to_string()
                .contains("private")
        );
        fs::set_permissions(&bases, fs::Permissions::from_mode(0o700)).unwrap();
        let instance = store.create(config).unwrap();
        assert_eq!(
            store
                .resolve(&instance.id)
                .unwrap()
                .config
                .shared_kernel_base,
            Some(bases.canonicalize().unwrap())
        );
    }
    #[test]
    fn docker_platform_selection_requires_explicit_platform_in_records() {
        assert_eq!(canonical_platform("linux/arm64/v8").unwrap(), "linux/arm64");
        let (_temporary, store, image) = fixture();
        let mut legacy = serde_json::to_value(&image).unwrap();
        legacy.as_object_mut().unwrap().remove("platform");
        assert!(serde_json::from_value::<Image>(legacy).is_err());
        for invalid in [
            "",
            "amd64",
            "windows/amd64",
            "linux/arm/v7",
            "linux/arm64/v9",
        ] {
            let error = store
                .pull_for_platform_with_progress("unused", invalid, &mut |_| {
                    panic!("invalid platform reached image preparation")
                })
                .unwrap_err();
            assert!(error.to_string().contains("unsupported platform"));
        }
        assert_eq!(store.images().unwrap()[0].id, image.id);
        let mut config = request();
        config.platform = Some(native_platform());
        assert_eq!(
            store.create(config).unwrap().image.platform,
            native_platform()
        );
    }
    #[test]
    fn docker_foreign_platforms_fail_before_pull_creation_or_start() {
        let (_temporary, store, mut image) = fixture();
        let foreign = if native_platform() == "linux/arm64" {
            "linux/amd64"
        } else {
            "linux/arm64"
        };
        let mut config = request();
        config.image = "missing-image".into();
        config.platform = Some(foreign.into());
        let error = store
            .create_with_progress(config, &mut |_| panic!("foreign run tried to pull"))
            .unwrap_err();
        assert!(error.to_string().contains("cross-architecture emulation"));
        assert!(store.instances(true).unwrap().is_empty());

        image.platform = foreign.into();
        store
            .save_catalog(&BTreeMap::from([(
                image_reference_key(&image.reference).unwrap(),
                image.clone(),
            )]))
            .unwrap();
        assert!(
            store
                .create(request())
                .unwrap_err()
                .to_string()
                .contains(foreign)
        );
        assert!(store.instances(true).unwrap().is_empty());

        image.platform = native_platform();
        store
            .save_catalog(&BTreeMap::from([(
                image_reference_key(&image.reference).unwrap(),
                image,
            )]))
            .unwrap();
        let mut instance = store.create(request()).unwrap();
        instance.image.platform = foreign.into();
        store.save(&instance).unwrap();
        let error = store.start(&instance.id).unwrap_err();
        assert!(error.to_string().contains("cross-architecture emulation"));
        let unchanged = store.resolve(&instance.id).unwrap();
        assert_eq!(unchanged.generation, instance.generation);
        assert_eq!(unchanged.state, instance.state);
    }
    #[test]
    fn docker_tty_defaults_preserve_image_explicit_and_unset_environment() {
        for (tty, base, explicit, unset, expected) in [
            (true, None, None, false, Some("xterm")),
            (true, Some("image-term"), None, false, Some("image-term")),
            (true, Some("image-term"), Some(""), false, Some("")),
            (true, Some("image-term"), None, true, None),
            (false, None, None, false, None),
        ] {
            let (_temporary, store, mut image) = fixture();
            if let Some(value) = base {
                image.env.insert("TERM".into(), value.into());
                store
                    .save_catalog(&BTreeMap::from([(
                        image_reference_key(&image.reference).unwrap(),
                        image,
                    )]))
                    .unwrap();
            }
            let mut config = request();
            config.tty = tty.then_some([24, 80]);
            if let Some(value) = explicit {
                config.env.insert("TERM".into(), value.into());
            }
            if unset {
                config.unset_env.push("TERM".into());
            }
            assert_eq!(
                store
                    .create(config)
                    .unwrap()
                    .env
                    .get("TERM")
                    .map(String::as_str),
                expected
            );
        }
    }
    #[test]
    fn docker_creation_pins_defaults_and_command_overrides() {
        let (_temporary, store, image) = fixture();
        let instance = store.create(request()).unwrap();
        assert_eq!(instance.argv, ["/entry", "default"]);
        assert_eq!(instance.cwd, Path::new("/work"));
        assert_eq!(instance.user, "1000:1000");
        assert_eq!(instance.image.id, image.id);
        assert!(store.instances(false).unwrap().is_empty());
        assert_eq!(store.resolve(&instance.id[..12]).unwrap().id, instance.id);
        assert!(store.create(request()).is_err());
        let mut config = request();
        config.name = Some("override".into());
        config.command = vec!["argument".into()];
        assert_eq!(
            store.create(config.clone()).unwrap().argv,
            ["/entry", "argument"]
        );
        config.name = Some("entrypoint".into());
        config.entrypoint = Some("/other".into());
        config.command.clear();
        assert_eq!(store.create(config).unwrap().argv, ["/other"]);
        let mut config = request();
        config.name = Some("unset".into());
        config.unset_env = vec!["IMAGE_ENV".into()];
        assert!(!store.create(config).unwrap().env.contains_key("IMAGE_ENV"));
    }
    #[test]
    fn docker_start_joins_starting_generation_and_reports_its_failure() {
        let (_temporary, store, _) = fixture();
        let mut instance = store.create(request()).unwrap();
        instance.state = "starting".into();
        instance.started_ms = crate::unix_now_ms();
        store.save(&instance).unwrap();
        crate::util::write_private_json(
            &store
                .root
                .join("results")
                .join(format!("{}.json", instance.id)),
            &Completion {
                generation: instance.generation.clone(),
                code: 125,
                error: Some("original launch failed".into()),
            },
        )
        .unwrap();
        let error = store.start(&instance.id).unwrap_err();
        assert!(error.to_string().contains("original launch failed"));
        assert_eq!(
            store.resolve(&instance.id).unwrap().generation,
            instance.generation
        );
    }
    #[test]
    fn docker_volume_reopen_preserves_guest_modes_and_rejects_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (_temporary, store, _) = fixture();
        let volume = store.volume("data").unwrap();
        fs::write(volume.join("keep"), b"retained").unwrap();
        fs::set_permissions(&volume, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(store.volume("data").unwrap(), volume);
        assert_eq!(fs::metadata(&volume).unwrap().mode() & 0o777, 0o755);
        assert_eq!(fs::read(volume.join("keep")).unwrap(), b"retained");
        assert_eq!(
            fs::metadata(volume.parent().unwrap()).unwrap().mode() & 0o777,
            0o700
        );
        symlink(&volume, volume.parent().unwrap().join("link")).unwrap();
        assert!(store.volume("link").is_err());
        fs::write(volume.parent().unwrap().join("file"), b"retained").unwrap();
        assert!(store.volume("file").is_err());
    }

    #[test]
    fn docker_remove_refuses_live_owners_and_retains_named_volumes() {
        let (_temporary, store, _) = fixture();
        let mut config = request();
        let named = store.volume("data").unwrap();
        fs::write(named.join("keep"), b"database").unwrap();
        config.mounts.push(Mount {
            source: named.clone(),
            target: "/data".into(),
            read_only: false,
            volume: Some("data".into()),
        });
        let instance = store.create(config).unwrap();
        let vm = acquire_lease(&store.directory(&instance.id).join("vm.lock")).unwrap();
        assert!(store.remove("test", true).is_err());
        drop(vm);
        assert!(store.rmi("fixture:latest", false).is_err());
        store.remove("test", true).unwrap();
        assert_eq!(fs::read(named.join("keep")).unwrap(), b"database");
        store.rmi("fixture:latest", false).unwrap();
        assert!(store.images().unwrap().is_empty());
    }
    #[test]
    fn docker_stale_generation_cannot_consume_an_old_completion() {
        let (_temporary, store, _) = fixture();
        let instance = store.create(request()).unwrap();
        let mut output = store.output(&instance).unwrap();
        fs::write(
            store.directory(&instance.id).join("stdout.log"),
            b"last output",
        )
        .unwrap();
        crate::util::write_private_json(
            &output.result,
            &Completion {
                generation: "old".into(),
                code: 0,
                error: None,
            },
        )
        .unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        // Missing owners terminate the wait, but never reuse an old success.
        assert_eq!(output.follow("new", &mut stdout, &mut stderr).unwrap(), 137);
        assert_eq!(stdout, b"last output");
        output = store.output(&instance).unwrap();
        crate::util::write_private_json(
            &output.result,
            &Completion {
                generation: "new".into(),
                code: 7,
                error: None,
            },
        )
        .unwrap();
        assert_eq!(
            output
                .follow("new", &mut Vec::new(), &mut Vec::new())
                .unwrap(),
            7
        );
    }
    #[test]
    fn docker_storage_and_mounts_reject_authority_aliases() {
        let (temporary, store, _) = fixture();
        let alias = temporary.path().join("alias");
        std::os::unix::fs::symlink(&store.root, &alias).unwrap();
        assert!(Store::open(alias).is_err());
        let mut config = request();
        config.mounts.push(Mount {
            source: store.root.clone(),
            target: "/host".into(),
            read_only: false,
            volume: None,
        });
        assert!(store.create(config).is_err());
        let mut config = request();
        config.cwd = Some("relative".into());
        assert!(store.create(config).is_err());
        let mut config = request();
        config.name = Some("../escape".into());
        assert!(store.create(config).is_err());
        assert!(validate_target(Path::new("/proc/other")).is_err());
        let mut instance = store.create(request()).unwrap();
        instance.generation = uuid();
        assert!(
            socket(&instance)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
                .len()
                < 100
        );
    }
}
