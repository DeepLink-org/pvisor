//! Read-only, authenticated snapshot RAM. Cached FUSE reads are driven by host
//! page faults; MAP_PRIVATE guest mappings own all subsequent writes.
#[path = "ram_runtime.rs"]
mod ram_runtime;
use super::{EnvironmentManifest, RamBlocks, SnapshotStore, store::valid_id};
use crate::ram_backing::BLOCK_BYTES;
#[cfg(target_os = "macos")]
use anyhow::Context;
use anyhow::ensure;
use fuser::{
    BackgroundSession, FileAttr, FileType, Filesystem, KernelConfig, MountOption, ReplyAttr,
    ReplyData, ReplyEntry, ReplyOpen, Request,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{FileExt, OpenOptionsExt, PermissionsExt},
    },
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    time::{Duration, Instant, UNIX_EPOCH},
};

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Raw snapshot v3 authenticates each 64 KiB block before supplying any bytes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRamIndex {
    pub length: u64,
    pub sha256: Vec<String>,
}
impl RawRamIndex {
    pub(super) fn capture(file: &File) -> anyhow::Result<Self> {
        let mut index = Self {
            length: file.metadata()?.len(),
            sha256: Vec::new(),
        };
        let mut offset = 0;
        while offset < index.length {
            let mut bytes = vec![0; (index.length - offset).min(BLOCK_BYTES as u64) as usize];
            file.read_exact_at(&mut bytes, offset)?;
            index.sha256.push(hash(&bytes));
            offset += bytes.len() as u64;
        }
        index.validate()?;
        Ok(index)
    }
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.length > 0 && self.sha256.len() as u64 == self.length.div_ceil(BLOCK_BYTES as u64),
            "invalid raw RAM block inventory"
        );
        for id in &self.sha256 {
            valid_id(id)?;
        }
        Ok(())
    }
}

enum Backing {
    Raw {
        file: File,
        index: Option<RawRamIndex>,
    },
    Compressed(std::sync::Arc<super::blocks::PinnedRamBlocks>),
}

/// Owns the backing, including hard links retaining compressed blocks after
/// snapshot deletion. Construction reads metadata only, never RAM payloads.
pub struct SnapshotRamReader {
    backing: Backing,
    store_root: std::path::PathBuf,
    length: u64,
    // Small bounded cache avoids repeatedly decoding a block for 4 KiB faults.
    // The kernel page cache is the primary decoded cache.
    cache: lru::LruCache<usize, (Vec<u8>, Option<crate::cache_budget::Charge>)>,
}
impl SnapshotRamReader {
    pub(super) fn new(path: &Path, manifest: &EnvironmentManifest) -> anyhow::Result<Self> {
        Self::from_parts(
            path,
            manifest.ram_blocks.as_ref(),
            manifest.ram_index.as_ref(),
            &manifest.ram_sha256,
        )
    }
    fn from_parts(
        path: &Path,
        blocks: Option<&RamBlocks>,
        index: Option<&RawRamIndex>,
        ram_sha256: &str,
    ) -> anyhow::Result<Self> {
        let (backing, length) = if let Some(blocks) = blocks {
            blocks.validate()?;
            // Pin on the store's filesystem (hard links cannot cross devices).
            // The existing writer lease lets gc reap crashed readers while
            // preserving live ones after the published snapshot is deleted.
            let store = path
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| anyhow::anyhow!("missing snapshot store"))?;
            let references = SnapshotStore::new(store)?.begin()?;
            fs::create_dir(references.directory().join("ram-blocks"))?;
            let mut seen = HashSet::new();
            for block in &blocks.blocks {
                if seen.insert(&block.id) {
                    let pinned = references.directory().join("ram-blocks").join(&block.id);
                    fs::hard_link(path.join("ram-blocks").join(&block.id), &pinned)?;
                    let meta = fs::symlink_metadata(&pinned)?;
                    ensure!(
                        meta.is_file() && (46..=(45 + BLOCK_BYTES) as u64).contains(&meta.len()),
                        "invalid RAM content object"
                    );
                }
            }
            (
                Backing::Compressed(std::sync::Arc::new(super::blocks::PinnedRamBlocks {
                    blocks: blocks.clone(),
                    references,
                    sha256: ram_sha256.to_owned(),
                })),
                blocks.length,
            )
        } else {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path.join("ram.bin"))?;
            let meta = file.metadata()?;
            ensure!(meta.is_file() && meta.len() > 0, "invalid raw RAM backing");
            if let Some(index) = index {
                index.validate()?;
                ensure!(index.length == meta.len(), "RAM file size mismatch");
            }
            (
                Backing::Raw {
                    file,
                    index: index.cloned(),
                },
                meta.len(),
            )
        };
        Ok(Self {
            backing,
            store_root: path
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| anyhow::anyhow!("missing snapshot store"))?
                .canonicalize()?,
            length,
            cache: lru::LruCache::new(std::num::NonZeroUsize::new(4).unwrap()),
        })
    }
    pub(crate) fn compressed_base(&self) -> Option<std::sync::Arc<super::blocks::PinnedRamBlocks>> {
        match &self.backing {
            Backing::Compressed(base) => Some(base.clone()),
            Backing::Raw { .. } => None,
        }
    }
    pub fn len(&self) -> u64 {
        self.length
    }
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Reads across block boundaries and short tails, checking complete blocks
    /// even for partial requests. A corrupt block never supplies unchecked bytes.
    pub fn read_at(&mut self, offset: u64, output: &mut [u8]) -> anyhow::Result<usize> {
        let length = self.length.saturating_sub(offset).min(output.len() as u64) as usize;
        let mut copied = 0;
        while copied < length {
            let position = offset + copied as u64;
            let index = (position / BLOCK_BYTES as u64) as usize;
            let start = (position % BLOCK_BYTES as u64) as usize;
            let mut uncached = None;
            if !self.cache.contains(&index) {
                let bytes = match &self.backing {
                    Backing::Compressed(base) => base
                        .blocks
                        .read_block(&base.references.directory().join("ram-blocks"), index)?,
                    Backing::Raw { file, index: seal } => {
                        let block_offset = index as u64 * BLOCK_BYTES as u64;
                        let mut bytes =
                            vec![0; (self.length - block_offset).min(BLOCK_BYTES as u64) as usize];
                        file.read_exact_at(&mut bytes, block_offset)?;
                        if let Some(seal) = seal {
                            ensure!(
                                hash(&bytes) == seal.sha256[index],
                                "raw RAM block digest mismatch"
                            );
                        }
                        bytes
                    }
                };
                match crate::cache_budget::reserve_replacing(bytes.len(), || {
                    self.cache.pop_lru().is_some()
                }) {
                    Ok(charge) => {
                        self.cache.put(index, (bytes, charge));
                    }
                    Err(()) => {
                        uncached = Some(bytes);
                    }
                }
            }
            let bytes = match &uncached {
                Some(bytes) => bytes,
                None => &self.cache.get(&index).unwrap().0,
            };
            let count = (length - copied).min(bytes.len() - start);
            output[copied..copied + count].copy_from_slice(&bytes[start..start + count]);
            copied += count;
        }
        Ok(copied)
    }
}

/// Must outlive the VM and its RAM mappings. Dropping it unmounts the pager and
/// releases content pins; it must never be dropped at the guest-ready callback.
pub struct SnapshotRamMount {
    session: Option<BackgroundSession>,
    server: Option<(Child, ChildStdin)>,
    watchdog: Option<(Child, ChildStdin)>,
    directory: tempfile::TempDir,
}
impl SnapshotRamMount {
    pub(crate) fn ram_path(&self) -> std::path::PathBuf {
        self.directory.path().join("ram")
    }

    /// The ordinary executor owns its pager in the supervisor, whose binary
    /// enters internal modes before parsing CLI arguments.
    pub(crate) fn watch_native_owner_exit(&mut self, executable: &Path) -> io::Result<()> {
        use std::os::unix::process::CommandExt;
        if self.watchdog.is_some() {
            return Err(io::Error::other("RAM exit watchdog already installed"));
        }
        let mut child = Command::new(executable)
            .env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
            )
            .env("PVISOR_VM_RESTORE_RAM_WATCHDOG", self.directory.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0)
            .spawn()?;
        let pipe = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("missing RAM watchdog pipe"))?;
        self.watchdog = Some((child, pipe));
        Ok(())
    }

    /// Mount in a private host runtime directory, never in durable state.
    /// `directory` is retained for API compatibility and treated as an excluded
    /// environment/state root, not a mount parent; it need not exist.
    pub fn new(reader: SnapshotRamReader, directory: &Path) -> io::Result<(Self, File)> {
        Self::runtime(reader, &[directory])
    }

    pub(crate) fn runtime(
        reader: SnapshotRamReader,
        protected: &[&Path],
    ) -> io::Result<(Self, File)> {
        let mut roots = protected.to_vec();
        roots.push(&reader.store_root);
        let temporary = ram_runtime::temporary(&roots)?;
        Self::mount_at(reader, temporary)
    }

    fn mount_at(
        reader: SnapshotRamReader,
        temporary: tempfile::TempDir,
    ) -> io::Result<(Self, File)> {
        let options = [
            MountOption::FSName("pvisor-snapshot-ram".into()),
            MountOption::RO,
            MountOption::DefaultPermissions,
            MountOption::NoExec,
            MountOption::NoSuid,
            MountOption::NoDev,
            #[cfg(target_os = "macos")]
            MountOption::CUSTOM("backend=kernel".into()),
        ];
        #[cfg(target_os = "linux")]
        let session = BackgroundSession::new_interruptible(fuser::Session::new(
            RamFs { reader },
            temporary.path(),
            &options,
        )?)?;
        #[cfg(not(target_os = "linux"))]
        let session = fuser::spawn_mount2(RamFs { reader }, temporary.path(), &options)?;
        let mount = Self {
            session: Some(session),
            server: None,
            watchdog: None,
            directory: temporary,
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let file = loop {
            match File::open(mount.directory.path().join("ram")) {
                Ok(file) => break file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    if mount.session.as_ref().unwrap().guard.is_finished() {
                        return Err(io::Error::other(
                            "snapshot RAM FUSE session exited before mount became ready",
                        ));
                    }
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "snapshot RAM mount did not become ready",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error),
            }
        };
        Ok((mount, file))
    }

    /// Serve faults outside the VMM process. Its kernel teardown must never
    /// wait on a FUSE server that was killed along with its vCPUs.
    /// The public ram_mount directory argument is an excluded state root, not
    /// a staging or mount parent. Both spec and mount use private runtime storage.
    pub(super) fn external(
        object: &Path,
        manifest: &EnvironmentManifest,
        directory: &Path,
        executable: &Path,
    ) -> io::Result<(Self, File)> {
        use std::os::unix::{io::AsRawFd, process::CommandExt};
        let object = fs::canonicalize(object)?;
        let store = object
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| io::Error::other("missing snapshot store"))?;
        let temporary = ram_runtime::temporary(&[store, directory])?;
        // Keep the spec outside the mountpoint: mounting hides its contents,
        // and cleanup must only remove an empty mount directory. The staging
        // owner is released on readiness or on any startup failure.
        let staging = ram_runtime::temporary(&[store, directory])?;
        let mut spec = tempfile::NamedTempFile::new_in(staging.path())?;
        serde_json::to_writer(
            spec.as_file_mut(),
            &RamServerSpec {
                object: object.as_os_str().as_bytes().to_vec(),
                mount: temporary.path().as_os_str().as_bytes().to_vec(),
                blocks: manifest.ram_blocks.clone(),
                index: manifest.ram_index.clone(),
                ram_sha256: manifest.ram_sha256.clone(),
            },
        )?;
        spec.as_file_mut().flush()?;
        let mut child = Command::new(executable)
            .env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
            )
            .env("PVISOR_VM_RESTORE_RAM_SERVER", spec.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .process_group(0)
            .spawn()?;
        let pipe = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("missing RAM server pipe"))?;
        let mut output = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing RAM readiness pipe"))?;
        let mount = Self {
            session: None,
            server: Some((child, pipe)),
            watchdog: None,
            directory: temporary,
        };
        // The pipe is private and has one writer. Bound readiness without a
        // detached reader thread or an unbounded wait on a failed helper.
        let flags = unsafe { libc::fcntl(output.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            return Err(io::Error::last_os_error());
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut ready = Vec::new();
        loop {
            let mut bytes = [0; 16];
            match output.read(&mut bytes) {
                Ok(0) => {
                    return Err(io::Error::other(
                        "snapshot RAM server exited before readiness",
                    ));
                }
                Ok(count) => {
                    ready.extend_from_slice(&bytes[..count]);
                    if ready == b"ready\n" {
                        break;
                    }
                    if ready.len() >= 6 {
                        return Err(io::Error::other("invalid RAM server readiness"));
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "snapshot RAM server readiness timed out",
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // The caller's published-object lease remains held until the helper
        // has opened raw backing or acquired independent compressed pins.
        let file = File::open(mount.directory.path().join("ram"))?;
        Ok((mount, file))
    }

    /// A VMM can terminate with _exit or SIGKILL, bypassing Rust destructors.
    /// A separate process group watches this pipe and unmounts after EOF. It
    /// inherits no RAM descriptors and does not require FUSE allow_other.
    pub fn watch_runner_exit(&mut self, executable: &Path) -> io::Result<()> {
        use std::os::unix::process::CommandExt;
        if self.watchdog.is_some() || self.server.is_some() {
            return Err(io::Error::other("RAM cleanup owner already installed"));
        }
        let mut child = Command::new(executable)
            .env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
            )
            .env("PVISOR_VM_RESTORE_RAM_WATCHDOG", self.directory.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0)
            .spawn()?;
        let pipe = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("missing RAM watchdog pipe"))?;
        self.watchdog = Some((child, pipe));
        Ok(())
    }
}
impl Drop for SnapshotRamMount {
    fn drop(&mut self) {
        if let Some((mut child, pipe)) = self.server.take() {
            drop(pipe); // RAM users have gone; the independent server may detach.
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if !status.success() {
                            tracing::warn!(%status, "snapshot RAM server cleanup failed");
                            if let Err(error) = detach_mount(self.directory.path()) {
                                tracing::warn!(%error, "cannot detach failed snapshot RAM server");
                            }
                        }
                        break;
                    }
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    result => {
                        tracing::warn!(?result, "snapshot RAM server cleanup exceeded deadline");
                        // No VM mappings may outlive this owner. Force-detach
                        // only this private mount before terminating its server.
                        if let Err(error) = detach_mount(self.directory.path()) {
                            tracing::warn!(%error, "cannot detach snapshot RAM server");
                        }
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }

        if let Some((mut child, pipe)) = self.watchdog.take() {
            drop(pipe);
            let result = wait_for_exit(&mut child, Duration::from_secs(10));
            if !matches!(result, Ok(status) if status.success()) {
                tracing::warn!(?result, "snapshot RAM watchdog cleanup failed");
                if let Err(error) = detach_mount(self.directory.path()) {
                    tracing::warn!(%error, "cannot detach snapshot RAM after watchdog failure");
                }
            }
        }
        if let Some(session) = self.session.take()
            && let Err(error) = session.unmount()
        {
            tracing::warn!(%error, "cannot unmount snapshot RAM");
        }
    }
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> io::Result<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "RAM cleanup helper timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RamServerSpec {
    object: Vec<u8>,
    mount: Vec<u8>,
    blocks: Option<RamBlocks>,
    index: Option<RawRamIndex>,
    ram_sha256: String,
}
/// Private snapshot pager entry point; keep answering faults until its sole
/// owning runner closes the pipe, including during kernel process teardown.
pub(crate) fn serve_ram(spec: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;
    ram_runtime::private(
        spec.parent()
            .ok_or_else(|| anyhow::anyhow!("missing RAM spec parent"))?,
        &[],
    )?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(spec)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o077 == 0
            && metadata.len() <= 128 * 1024 * 1024,
        "invalid private RAM server specification"
    );
    let spec: RamServerSpec = serde_json::from_reader(file.take(128 * 1024 * 1024 + 1))?;
    let object = Path::new(OsStr::from_bytes(&spec.object));
    let mount = Path::new(OsStr::from_bytes(&spec.mount));
    ensure!(
        object.is_absolute()
            && mount.is_absolute()
            && mount
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("ram-mount-")),
        "RAM server requires absolute object and private ram-mount paths"
    );
    let metadata = fs::symlink_metadata(mount)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o077 == 0,
        "invalid private RAM server mountpoint"
    );
    let reader = SnapshotRamReader::from_parts(
        object,
        spec.blocks.as_ref(),
        spec.index.as_ref(),
        &spec.ram_sha256,
    )?;
    ram_runtime::private(mount, &[&reader.store_root])?;
    let options = [
        MountOption::FSName("pvisor-snapshot-ram".into()),
        MountOption::RO,
        MountOption::DefaultPermissions,
        MountOption::NoExec,
        MountOption::NoSuid,
        MountOption::NoDev,
        #[cfg(target_os = "macos")]
        MountOption::CUSTOM("backend=kernel".into()),
    ];
    #[cfg(target_os = "linux")]
    let session = BackgroundSession::new_interruptible(fuser::Session::new(
        RamFs { reader },
        mount,
        &options,
    )?)?;
    #[cfg(not(target_os = "linux"))]
    let session = fuser::spawn_mount2(RamFs { reader }, mount, &options)?;
    let result = (|| -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !mount.join("ram").try_exists()? {
            ensure!(
                !session.guard.is_finished(),
                "snapshot RAM server stopped before mount readiness"
            );
            ensure!(
                Instant::now() < deadline,
                "snapshot RAM mount readiness timed out"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        io::stdout().write_all(b"ready\n")?;
        io::stdout().flush()?;
        io::copy(&mut io::stdin().lock(), &mut io::sink())?;
        Ok(())
    })();
    // EOF is safe even for SIGKILL/_exit: the FUSE server remains alive while
    // the owning process releases mappings, so it cannot wait on itself.
    let detached = detach_mount(mount);
    let unmounted = session.unmount();
    result?;
    detached?;
    unmounted?;
    Ok(())
}

/// Internal CLI watchdog entry point. EOF means the sole owning runner has
/// exited or explicitly released its mount after all RAM users were dropped.
pub(crate) fn watch_mount(path: &Path) -> anyhow::Result<()> {
    ensure!(
        path.is_absolute()
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("ram-mount-")),
        "invalid RAM watchdog mountpoint"
    );
    io::copy(&mut io::stdin().lock(), &mut io::sink())?;
    detach_mount(path)
}
fn detach_mount(path: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let native = super::native_path(path)?;
        // Privileged deployments may mount directly without fusermount.
        let detached = unsafe { libc::umount2(native.as_ptr(), libc::MNT_DETACH) } == 0;
        let error = io::Error::last_os_error();
        if !detached
            && !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOENT))
            && linux_mount_present(path)?
        {
            let mut result = None;
            for helper in ["fusermount3", "fusermount"] {
                match Command::new(helper)
                    .args(["-u", "-z", "--"])
                    .arg(path)
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit())
                    .spawn()
                {
                    Ok(mut child) => {
                        result = Some(wait_for_exit(&mut child, Duration::from_secs(5))?);
                        break;
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            let status =
                result.ok_or_else(|| anyhow::anyhow!("FUSE unmount helper unavailable"))?;
            ensure!(status.success(), "snapshot RAM unmount failed: {status}");
        }
    }
    #[cfg(target_os = "macos")]
    {
        let native = super::native_path(path)?;
        // The sole runner has closed its ownership pipe, so no VM mappings
        // may still use this private read-only mount. Match fuser's Darwin
        // detach policy: ordinary umount can wait forever on a dead server.
        // SAFETY: native is a live NUL-terminated path; the OS checks ownership.
        let detached = unsafe { libc::unmount(native.as_ptr(), libc::MNT_FORCE) } == 0;
        let error = io::Error::last_os_error();
        if !detached && !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOENT)) {
            return Err(error).context("snapshot RAM detach failed");
        }
    }
    match fs::remove_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn linux_mount_present(path: &Path) -> io::Result<bool> {
    // Linux can return EPERM before checking whether this path is mounted.
    // Read the kernel table rather than treating an unmount helper's diagnostic
    // as proof that cleanup is safe. Encode paths as mountinfo does, including
    // whitespace, backslashes and non-UTF-8 names, without touching the mount.
    let mut target = Vec::new();
    for byte in path.as_os_str().as_bytes() {
        match byte {
            b' ' => target.extend_from_slice(b"\\040"),
            b'\t' => target.extend_from_slice(b"\\011"),
            b'\n' => target.extend_from_slice(b"\\012"),
            b'\\' => target.extend_from_slice(b"\\134"),
            byte => target.push(*byte),
        }
    }
    let mounts = fs::read("/proc/self/mountinfo")?;
    Ok(mounts.split(|byte| *byte == b'\n').any(|line| {
        line.split(|byte| *byte == b' ')
            .nth(4)
            .is_some_and(|mount| mount == target)
    }))
}

const ROOT: u64 = 1;
const RAM: u64 = 2;
struct RamFs {
    reader: SnapshotRamReader,
}
impl RamFs {
    fn attr(&self, ino: u64) -> FileAttr {
        FileAttr {
            ino,
            size: if ino == RAM { self.reader.len() } else { 0 },
            blocks: if ino == RAM {
                self.reader.len().div_ceil(512)
            } else {
                0
            },
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: if ino == RAM {
                FileType::RegularFile
            } else {
                FileType::Directory
            },
            perm: if ino == RAM { 0o400 } else { 0o500 },
            nlink: if ino == RAM { 1 } else { 2 },
            uid: unsafe { libc::geteuid() },
            gid: unsafe { libc::getegid() },
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }
}
impl Filesystem for RamFs {
    fn init(&mut self, _req: &Request<'_>, config: &mut KernelConfig) -> Result<(), i32> {
        // Fetch bounded blocks near the fault, rather than the entire snapshot.
        let _ = config.set_max_readahead(BLOCK_BYTES as u32);
        Ok(())
    }
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        if parent == ROOT && name == OsStr::new("ram") {
            reply.entry(&Duration::from_secs(3600), &self.attr(RAM), 0);
        } else {
            reply.error(libc::ENOENT);
        }
    }
    fn getattr(&mut self, _req: &Request<'_>, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        if [ROOT, RAM].contains(&ino) {
            reply.attr(&Duration::from_secs(3600), &self.attr(ino));
        } else {
            reply.error(libc::ENOENT);
        }
    }
    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        if ino != RAM {
            reply.error(libc::ENOENT);
        } else if flags & libc::O_ACCMODE != libc::O_RDONLY || flags & libc::O_TRUNC != 0 {
            reply.error(libc::EROFS);
        } else {
            reply.opened(0, 0);
        } // Cached I/O is required for mmap.
    }
    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        if ino != RAM || offset < 0 || size > 1024 * 1024 {
            reply.error(libc::EINVAL);
            return;
        }
        let mut bytes = vec![0; size as usize];
        match self.reader.read_at(offset as u64, &mut bytes) {
            Ok(count) => reply.data(&bytes[..count]),
            Err(error) => {
                tracing::error!(%error, offset, "snapshot RAM fault read failed");
                // A failed mmap fault must stop the VM, never supply zero-filled
                // or unchecked RAM. The kernel propagates EIO as a mapping fault.
                reply.error(libc::EIO);
            }
        }
    }
}
