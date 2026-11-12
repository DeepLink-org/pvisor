//! Opt-in ordinary OCI branching. Capture is explicit at the caller's initialized
//! request boundary; there is no image conversion, UKSM pool, or automatic retry.
use super::*;
use pvisor_vm::api::{
    MachineRestore, MachineSnapshot, SnapshotCapture, SnapshotControl, SnapshotState,
};
use std::os::unix::net::UnixListener;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(super) fn validate_config(c: &Create) -> anyhow::Result<()> {
    if !c.branchable {
        return Ok(());
    }
    ensure!(
        cfg!(all(target_os = "linux", target_arch = "x86_64")),
        "OCI branching requires Linux x86-64"
    );
    ensure!(
        c.managed_memory && c.memory_pool.is_none() && c.shared_kernel_base.is_none(),
        "OCI branches require managed memory without UKSM/pool/shared kernel"
    );
    ensure!(
        !c.network && c.ports.is_empty() && c.peers.is_none() && c.mounts.is_empty(),
        "OCI branches currently require --network none and no additional mounts"
    );
    ensure!(
        !c.interactive && c.tty.is_none() && !c.auto_remove,
        "OCI branches require detached noninteractive ownership without automatic removal"
    );
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    protocol: u32,
    source: managed::Target,
    baseline: String,
}

// Preparation is ordinary host filesystem work, not time spent with the VM
// frozen. Keep its bounded wait distinct from the existing capture deadline.
const PREPARE_TIMEOUT: Duration = Duration::from_secs(1800);
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(600);
const COMMIT_TIMEOUT: Duration = Duration::from_secs(5);
const CAPTURE_PROTOCOL: u32 = 2;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "phase", content = "baseline", rename_all = "snake_case")]
enum CaptureReply {
    Prepared(String),
    Captured(String),
}

fn receive_phase(
    stream: &mut UnixStream,
    timeout: Duration,
    expected: CaptureReply,
) -> anyhow::Result<()> {
    stream.set_read_timeout(Some(timeout))?;
    let reply: Result<CaptureReply, String> =
        crate::host_transport::read_host_frame_sync(stream)
            .with_context(|| format!("branch {expected:?} reply failed (budget {timeout:?})"))?;
    ensure!(
        reply.map_err(anyhow::Error::msg)? == expected,
        "branch phase/identity mismatch"
    );
    Ok(())
}

// A late preparation must never pause a VM on behalf of a client that already
// timed out or disconnected. Only a fresh generation-bound acknowledgement
// permits the capture closure to acquire the CPU/device barrier.
fn prepare_then_capture<T>(
    stream: &mut UnixStream,
    req: &Capture,
    prepare: impl FnOnce() -> anyhow::Result<T>,
    capture: impl FnOnce(T) -> anyhow::Result<String>,
) -> anyhow::Result<String> {
    ensure!(
        req.protocol == CAPTURE_PROTOCOL,
        "unsupported branch capture protocol"
    );
    let prepared = prepare()?;
    let reply: Result<CaptureReply, String> = Ok(CaptureReply::Prepared(req.baseline.clone()));
    crate::host_transport::write_host_frame_sync(stream, &reply)?;
    stream.set_read_timeout(Some(COMMIT_TIMEOUT))?;
    let commit: Capture = crate::host_transport::read_host_frame_sync(stream)
        .context("branch preparation finished but capture acknowledgement missing")?;
    ensure!(
        commit.protocol == CAPTURE_PROTOCOL
            && commit.source == req.source
            && commit.baseline == req.baseline,
        "stale branch capture acknowledgement"
    );
    capture(prepared)
}

fn capture_stage<T>(
    base: &Path,
    phase: &str,
    action: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let started_ms = crate::unix_now_ms();
    let started = std::time::Instant::now();
    let path = base.join(format!("capture-{phase}.json"));
    crate::util::write_private_json(
        &path,
        &serde_json::json!({
            "phase": phase, "status": "running", "started_ms": started_ms,
        }),
    )?;
    let result = action().with_context(|| format!("branch {phase} failed"));
    let receipt = crate::util::write_private_json(
        &path,
        &serde_json::json!({
            "phase": phase, "status": if result.is_ok() { "complete" } else { "failed" },
            "started_ms": started_ms, "finished_ms": crate::unix_now_ms(),
            "elapsed_ms": started.elapsed().as_millis(),
            "error": result.as_ref().err().map(|e| format!("{e:#}")),
        }),
    );
    let value = result?;
    receipt?;
    Ok(value)
}

// Per-generation, per-phase receipts survive a killed runner. No guest command
// is issued and timing never changes a readiness or control budget.
pub(super) struct StartupSpan {
    path: Option<PathBuf>,
    generation: String,
    phase: String,
    started_ms: u64,
    started: Instant,
    finished: bool,
}
impl StartupSpan {
    pub(super) fn begin<B>(
        store: &Store,
        instance: &Instance<B>,
        phase: &str,
    ) -> anyhow::Result<Self> {
        ensure!(
            phase
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-'),
            "invalid startup phase"
        );
        let mut span = Self {
            path: instance.branch_base.as_ref().map(|_| {
                store
                    .directory(&instance.id)
                    .join(format!("startup-{}-{phase}.json", instance.generation))
            }),
            generation: instance.generation.clone(),
            phase: phase.into(),
            started_ms: crate::unix_now_ms(),
            started: Instant::now(),
            finished: false,
        };
        span.write("running", None)?;
        Ok(span)
    }
    fn write(&mut self, status: &str, error: Option<&str>) -> anyhow::Result<()> {
        if let Some(path) = &self.path {
            crate::util::write_private_json(
                path,
                &serde_json::json!({
                    "generation": self.generation, "phase": self.phase, "status": status,
                    "pid": std::process::id(), "started_ms": self.started_ms,
                    "observed_ms": crate::unix_now_ms(), "elapsed_ms": self.started.elapsed().as_millis(),
                    "error": error,
                }),
            )?;
        }
        Ok(())
    }
    pub(super) fn finish(&mut self, error: Option<&str>) -> anyhow::Result<()> {
        if self.finished {
            return Ok(());
        }
        self.write(
            if error.is_some() {
                "failed"
            } else {
                "complete"
            },
            error,
        )?;
        self.finished = true;
        Ok(())
    }
}
pub(super) fn startup_stage<B, T>(
    store: &Store,
    instance: &Instance<B>,
    phase: &str,
    action: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let mut span = StartupSpan::begin(store, instance, phase)?;
    let result = action();
    let error = result.as_ref().err().map(|e| format!("{e:#}"));
    let receipt = span.finish(error.as_deref());
    let value = result?;
    receipt?;
    Ok(value)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Baseline {
    version: u32,
    source: Instance,
    build: String,
    boot: String,
    image: crate::image::cache::RetainedImage,
    control: PathBuf,
    ram_sha256: String,
    machine_sha256: String,
    upper: crate::environment_snapshot::TreeInventory,
    work: crate::environment_snapshot::TreeInventory,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    baseline: String,
    manifest_sha256: String,
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
fn base_path(root: &Path, id: &str) -> anyhow::Result<PathBuf> {
    ensure!(valid_id(id), "invalid branch baseline identity");
    Ok(root.join("branches").join(id))
}
fn endpoint(target: &managed::Target) -> anyhow::Result<PathBuf> {
    target.validate()?;
    Ok(crate::host_transport::host_authority_root()?
        .join(format!("branch-{}.sock", target.generation)))
}
// The runner exits the process; its Drop is not an endpoint retirement receipt.
// Only the owner calls this after reaping the corresponding VM generation.
pub(super) fn retire<B>(instance: &Instance<B>) -> anyhow::Result<()> {
    if !instance.config.branchable || instance.branch_base.is_some() {
        return Ok(());
    }
    retire_path(&endpoint(&managed::Target {
        container_id: instance.id.clone(),
        generation: instance.generation.clone(),
    })?)
}
fn retire_path(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::FileTypeExt;
    match fs::symlink_metadata(path) {
        Ok(m) => {
            ensure!(
                m.file_type().is_socket()
                    && m.uid() == unsafe { libc::geteuid() }
                    && m.mode() & 0o7777 == 0o600,
                "refusing replaced branch endpoint"
            );
            fs::remove_file(path)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
fn boot() -> anyhow::Result<String> {
    Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned())
}
fn hash(path: &Path) -> anyhow::Result<String> {
    crate::environment_snapshot::file_hash(path)
}

impl Store {
    /// Freeze an initialized, explicitly branchable OCI VM once, then create
    /// stopped independent branches from that same machine baseline. The caller
    /// must stop sending guest requests before this call and select a point before
    /// divergent rollout actions. No readiness inference, imports, model calls or
    /// tool retries occur here. All branches keep the OCI image and configuration;
    /// only names, writable filesystems, control endpoints and RAM deltas differ.
    /// Failed/partial artifacts are retained for diagnosis. Use `start` only after
    /// this returns; branch restarts and branching a branch are unsupported.
    pub fn branch(&self, selector: &str, names: &[String]) -> anyhow::Result<Vec<Instance>> {
        ensure!(
            !names.is_empty()
                && names.len() <= 16
                && names.iter().all(|n| valid_name(n))
                && names
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == names.len(),
            "supply 1..16 distinct valid branch names"
        );
        let source = self.resolve(selector)?;
        validate_config(&source.config)?;
        ensure!(
            source.config.branchable && source.state == "running" && self.active(&source)?,
            "branch source is not running/branchable"
        );
        ensure!(
            source.branch_base.is_none(),
            "branching a branch is unsupported"
        );
        let target = managed::Target {
            container_id: source.id.clone(),
            generation: source.generation.clone(),
        };
        let path = endpoint(&target)?;
        crate::host_transport::validate_instance_socket(&path)?;
        let mut stream = UnixStream::connect(&path)?;
        crate::host_transport::authorize_host_peer_sync(&stream)?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let id = uuid();
        let request = Capture {
            protocol: CAPTURE_PROTOCOL,
            source: target,
            baseline: id.clone(),
        };
        crate::host_transport::write_host_frame_sync(&mut stream, &request)?;
        receive_phase(
            &mut stream,
            PREPARE_TIMEOUT,
            CaptureReply::Prepared(id.clone()),
        )?;
        crate::host_transport::write_host_frame_sync(&mut stream, &request)?;
        receive_phase(
            &mut stream,
            CAPTURE_TIMEOUT,
            CaptureReply::Captured(id.clone()),
        )?;
        let base = base_path(&self.root, &id)?;
        let manifest = base.join("baseline.json");
        let saved: Baseline = serde_json::from_reader(File::open(&manifest)?)?;
        ensure!(
            saved.source.id == source.id && saved.source.generation == source.generation,
            "branch source generation changed"
        );
        let binding = Binding {
            baseline: id,
            manifest_sha256: hash(&manifest)?,
        };
        let _guard = self.lock()?;
        let existing = self.all()?;
        ensure!(
            names.iter().all(|n| !existing.iter().any(|i| &i.name == n)),
            "branch name already exists"
        );
        let mut instances = Vec::new();
        for name in names {
            let mut child = source.clone();
            child.branch_base = Some(binding.baseline.clone());
            child.id = format!("{}{}", uuid(), uuid());
            child.name = name.clone();
            child.config.name = Some(name.clone());
            child.generation.clear();
            child.state = "created".into();
            child.started_ms = 0;
            child.exit_code = None;
            child.created_ms = crate::unix_now_ms();
            let directory = self.directory(&child.id);
            private_directory(&directory)?;
            // No catalog entry is published until both verified copies and the
            // binding are complete, so another start cannot see a partial branch.
            for (part, inventory) in [("upper", &saved.upper), ("work", &saved.work)] {
                crate::environment_snapshot::verify_tree(&base.join(part), inventory)?;
                let copied = crate::environment_snapshot::copy_owned_tree(
                    &base.join(part),
                    &directory.join(part),
                )?;
                ensure!(&copied == inventory, "branch filesystem copy differs");
            }
            crate::util::write_private_json(&directory.join("branch.json"), &binding)?;
            self.save(&child)?;
            instances.push(child);
        }
        Ok(instances)
    }
}

fn capture_private_state(
    frozen: &mut pvisor_vm::api::FrozenMachine<'_>,
    ram: &File,
    directory: &Path,
    lower: &Path,
    base: &Path,
) -> anyhow::Result<(
    MachineSnapshot,
    crate::environment_snapshot::TreeInventory,
    crate::environment_snapshot::TreeInventory,
)> {
    let mut machine = frozen
        .capture_machine_state(ram)
        .map_err(anyhow::Error::msg)?;
    let upper = crate::environment_snapshot::copy_owned_tree(
        &directory.join("upper"),
        &base.join("upper"),
    )?;
    let work =
        crate::environment_snapshot::copy_owned_tree(&directory.join("work"), &base.join("work"))?;
    let copies = vec![
        (directory.join("upper"), base.join("upper")),
        (directory.join("work"), base.join("work")),
    ];
    ensure!(
        machine.rebind_filesystem_stage("/dev/root", &copies, &[lower.to_owned()])? == 1,
        "expected one branch root filesystem"
    );
    Ok((machine, upper, work))
}

pub(super) struct Endpoint {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    listener: Option<UnixListener>,
    root: PathBuf,
    directory: PathBuf,
    target: managed::Target,
    image: crate::image::cache::RetainedImage,
    control: PathBuf,
}
impl Endpoint {
    pub(super) fn bind<B>(
        store: &Store,
        instance: &Instance<B>,
        image: &mut ImageRoot,
    ) -> anyhow::Result<Option<Self>> {
        if !instance.config.branchable || instance.branch_base.is_some() {
            return Ok(None);
        }
        validate_config(&instance.config)?;
        let target = managed::Target {
            container_id: instance.id.clone(),
            generation: instance.generation.clone(),
        };
        let path = endpoint(&target)?;
        let listener = UnixListener::bind(&path)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Some(Self {
            path,
            stop: Arc::new(AtomicBool::new(false)),
            listener: Some(listener),
            root: store.root.clone(),
            directory: store.directory(&instance.id),
            target,
            image: image
                ._owner
                .as_mut()
                .context("branch source needs a pinned OCI image")?
                .retain_for_branch()?,
            control: socket(instance)?,
        }))
    }
    pub(super) fn start(&mut self, handle: pvisor_vm::api::VmmHandle) -> anyhow::Result<()> {
        let listener = self
            .listener
            .take()
            .context("branch endpoint already started")?;
        let stop = self.stop.clone();
        let root = self.root.clone();
        let directory = self.directory.clone();
        let target = self.target.clone();
        let image = self.image.clone();
        let lower = image.root().to_owned();
        let control = self.control.clone();
        std::thread::Builder::new()
            .name("oci-branch-capture".into())
            .spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(s) => s,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(20));
                            continue;
                        }
                        Err(_) => break,
                    };
                    let result = (|| -> anyhow::Result<String> {
                        crate::host_transport::authorize_host_peer_sync(&stream)?;
                        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                        let req: Capture =
                            crate::host_transport::read_host_frame_sync(&mut stream)?;
                        ensure!(req.source == target, "stale branch capture identity");
                        let base = base_path(&root, &req.baseline)?;
                        private_directory(&root.join("branches"))?;
                        ensure!(
                            !base.exists(),
                            "branch baseline already exists; do not retry capture"
                        );
                        private_directory(&base)?;
                        let source: Instance =
                            serde_json::from_reader(File::open(directory.join("instance.json"))?)?;
                        ensure!(
                            source.id == target.container_id
                                && source.generation == target.generation,
                            "branch source changed"
                        );
                        prepare_then_capture(
                            &mut stream,
                            &req,
                            || {
                                capture_stage(&base, "prepare-lower", || {
                                    // Preserve the authenticated OCI revision and lazy
                                    // projection inode, not a fully expanded image copy.
                                    image.verify()
                                })
                            },
                            |()| {
                                let current: Instance = serde_json::from_reader(File::open(
                                    directory.join("instance.json"),
                                )?)?;
                                ensure!(
                                    !stop.load(Ordering::Acquire)
                                        && current.id == target.container_id
                                        && current.generation == target.generation
                                        && current.state == "running",
                                    "branch source stopped or changed during preparation"
                                );
                                let ram = OpenOptions::new()
                                    .read(true)
                                    .write(true)
                                    .create_new(true)
                                    .mode(0o600)
                                    .open(base.join("ram"))?;
                                let (machine, upper, work) =
                                    capture_stage(&base, "frozen-state", || {
                                        handle
                                            .with_snapshot_quiesced(
                                                Duration::from_secs(30),
                                                |frozen| {
                                                    capture_private_state(
                                                        frozen, &ram, &directory, &lower, &base,
                                                    )
                                                    .map_err(|e| format!("{e:#}"))
                                                },
                                            )
                                            .map_err(anyhow::Error::msg)
                                    })?;
                                // Immutable snapshot files no longer depend on a paused source.
                                // Hashing/publication must not extend the CPU/device freeze.
                                capture_stage(&base, "publish", || {
                                    crate::util::write_private_json(
                                        &base.join("machine.json"),
                                        &machine,
                                    )?;
                                    let saved = Baseline {
                                        version: 2,
                                        source,
                                        build: hash(&std::env::current_exe()?)?,
                                        boot: boot()?,
                                        image: image.clone(),
                                        control: control.clone(),
                                        ram_sha256: hash(&base.join("ram"))?,
                                        machine_sha256: hash(&base.join("machine.json"))?,
                                        upper,
                                        work,
                                    };
                                    crate::util::write_private_json(
                                        &base.join("baseline.json"),
                                        &saved,
                                    )?;
                                    Ok(())
                                })?;
                                Ok(req.baseline.clone())
                            },
                        )
                    })()
                    .map(CaptureReply::Captured)
                    .map_err(|e| format!("{e:#}"));
                    let _ = crate::host_transport::write_host_frame_sync(&mut stream, &result);
                }
            })?;
        Ok(())
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = fs::remove_file(&self.path);
    }
}

// A launch-local result, never persisted or shared across processes/branches.
// Only the private runner can construct it, by hashing its own executable.
pub(super) struct VerifiedBuild(String);
fn verify_build(
    expected: &str,
    verified: Option<&VerifiedBuild>,
    compute: impl FnOnce() -> anyhow::Result<String>,
) -> anyhow::Result<VerifiedBuild> {
    let actual = match verified {
        Some(build) => build.0.clone(),
        None => compute()?,
    };
    ensure!(actual == expected, "branch build identity changed");
    Ok(VerifiedBuild(actual))
}

/// Reopen the same authenticated lazy OCI lower. Source exit must not remove
/// this projection; its saved inode identities belong to the machine snapshot.
pub(super) fn image_root(
    store: &Store,
    instance: &Instance,
) -> anyhow::Result<Option<(ImageRoot, VerifiedBuild)>> {
    let Some((_, saved, build)) = load_baseline(store, instance, "image", None)? else {
        return Ok(None);
    };
    let owner = startup_stage(store, instance, "image-open", || saved.image.open())?;
    Ok(Some((
        ImageRoot {
            root: owner.root().to_owned(),
            _owner: Some(owner),
        },
        build,
    )))
}

fn load_baseline<B>(
    store: &Store,
    instance: &Instance<B>,
    phase: &str,
    verified_build: Option<&VerifiedBuild>,
) -> anyhow::Result<Option<(PathBuf, Baseline, VerifiedBuild)>> {
    let directory = store.directory(&instance.id);
    let binding = directory.join("branch.json");
    let Some(expected_base) = &instance.branch_base else {
        ensure!(
            !binding.exists(),
            "unexpected branch binding on cold launch"
        );
        return Ok(None);
    };
    validate_config(&instance.config)?;
    ensure!(instance.config.branchable, "branch launch flag missing");
    let binding: Binding = serde_json::from_reader(File::open(binding)?)?;
    ensure!(
        &binding.baseline == expected_base,
        "branch baseline identity mismatch"
    );
    let base = base_path(&store.root, &binding.baseline)?;
    ensure!(
        hash(&base.join("baseline.json"))? == binding.manifest_sha256,
        "branch manifest changed"
    );
    let saved: Baseline = serde_json::from_reader(File::open(base.join("baseline.json"))?)?;
    ensure!(
        saved.version == 2 && saved.boot == boot()?,
        "branch host/version changed"
    );
    let build = startup_stage(
        store,
        instance,
        &format!(
            "{phase}-build-{}",
            if verified_build.is_some() {
                "binding"
            } else {
                "hash"
            }
        ),
        || {
            verify_build(&saved.build, verified_build, || {
                hash(&std::env::current_exe()?)
            })
        },
    )?;
    ensure!(
        saved.source.image.id == instance.image.id
            && saved.source.image.digest == instance.image.digest
            && saved.source.image.platform == instance.image.platform
            && saved.source.argv == instance.argv
            && saved.source.env == instance.env
            && saved.source.cwd == instance.cwd
            && saved.source.user == instance.user
            && saved.source.config.cpus == instance.config.cpus
            && saved.source.config.memory_mib == instance.config.memory_mib,
        "branch OCI execution configuration changed"
    );
    startup_stage(store, instance, &format!("{phase}-image-binding"), || {
        saved.image.verify()
    })?;
    Ok(Some((base, saved, build)))
}

pub(super) fn restore<B>(
    store: &Store,
    instance: &Instance<B>,
    lower: &Path,
    verified_build: Option<&VerifiedBuild>,
) -> anyhow::Result<Option<MachineRestore>> {
    let Some((base, saved, _)) = load_baseline(store, instance, "restore", verified_build)? else {
        return Ok(None);
    };
    ensure!(
        saved.image.root() == lower,
        "branch image projection changed"
    );
    let directory = store.directory(&instance.id);
    ensure!(
        startup_stage(store, instance, "ram-hash", || hash(&base.join("ram")))? == saved.ram_sha256
            && startup_stage(store, instance, "machine-hash", || hash(
                &base.join("machine.json")
            ))? == saved.machine_sha256,
        "branch baseline content changed"
    );
    for (part, inventory) in [("upper", &saved.upper), ("work", &saved.work)] {
        startup_stage(store, instance, &format!("verify-base-{part}"), || {
            crate::environment_snapshot::verify_tree(&base.join(part), inventory)
        })?;
        startup_stage(store, instance, &format!("verify-child-{part}"), || {
            crate::environment_snapshot::verify_tree(&directory.join(part), inventory)
        })?;
    }
    let mut state: MachineSnapshot = startup_stage(store, instance, "machine-decode", || {
        Ok(serde_json::from_reader(File::open(
            base.join("machine.json"),
        )?)?)
    })?;
    let copies = vec![
        (base.join("upper"), directory.join("upper")),
        (base.join("work"), directory.join("work")),
    ];
    ensure!(
        startup_stage(store, instance, "filesystem-rebind", || state
            .rebind_filesystem_stage("/dev/root", &copies, &[lower.to_owned()])
            .map_err(anyhow::Error::from))?
            == 1,
        "branch root binding missing"
    );
    state.rebind_vsock_listener(control::CONTROL_PORT, &saved.control, &socket(instance)?)?;
    Ok(Some(MachineRestore {
        state,
        ram_file: Arc::new(File::open(base.join("ram"))?),
        ram_extents: vec![],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_phase_preserves_failed_or_interrupted_state() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("phase.json");
        let mut span = StartupSpan {
            path: Some(path.clone()),
            generation: "a".repeat(32),
            phase: "ram-hash".into(),
            started_ms: crate::unix_now_ms(),
            started: Instant::now(),
            finished: false,
        };
        span.write("running", None).unwrap();
        let row: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(row["status"], "running");
        span.finish(Some("checksum mismatch")).unwrap();
        span.finish(None).unwrap();
        let row: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(row["status"], "failed");
        assert_eq!(row["error"], "checksum mismatch");
        assert!(row["elapsed_ms"].as_u64().is_some());
    }

    #[test]
    fn executable_hash_is_reused_only_within_explicit_launch_context() {
        let count = std::cell::Cell::new(0);
        let mut compute = || {
            count.set(count.get() + 1);
            Ok("build-a".into())
        };
        let first = verify_build("build-a", None, &mut compute).unwrap();
        verify_build("build-a", Some(&first), || {
            panic!("duplicate executable scan")
        })
        .unwrap();
        assert_eq!(count.get(), 1);
        // A fresh branch has no inherited capability; its executable is scanned.
        verify_build("build-a", None, &mut compute).unwrap();
        assert_eq!(count.get(), 2);
    }

    #[test]
    fn reused_build_hash_still_rejects_changed_baseline_identity() {
        let first = verify_build("build-a", None, || Ok("build-a".into())).unwrap();
        assert!(verify_build("build-b", Some(&first), || panic!("no extra hash")).is_err());
        assert!(verify_build("build-a", None, || Ok("build-b".into())).is_err());
        assert!(verify_build("build-a", None, || anyhow::bail!("read failed")).is_err());
    }

    #[test]
    fn ram_hash_includes_sparse_zeros_and_detects_modified_bytes() {
        use std::os::unix::fs::FileExt;
        let temp = tempfile::tempdir().unwrap();
        let sparse = temp.path().join("sparse");
        let dense = temp.path().join("dense");
        let size = 2 * 1024 * 1024 + 19;
        let file = File::create(&sparse).unwrap();
        file.set_len(size as u64).unwrap();
        fs::write(&dense, vec![0u8; size]).unwrap();
        let expected = hash(&dense).unwrap();
        assert_eq!(hash(&sparse).unwrap(), expected);
        file.write_all_at(b"changed", (1024 * 1024 + 3) as u64)
            .unwrap();
        assert_ne!(hash(&sparse).unwrap(), expected);
        file.write_all_at(&[0u8; 7], (1024 * 1024 + 3) as u64)
            .unwrap();
        assert_eq!(hash(&sparse).unwrap(), expected);
        file.set_len(size as u64 - 1).unwrap();
        assert_ne!(hash(&sparse).unwrap(), expected);
    }

    fn request() -> Capture {
        Capture {
            protocol: CAPTURE_PROTOCOL,
            source: managed::Target {
                container_id: "a".repeat(64),
                generation: "b".repeat(32),
            },
            baseline: "c".repeat(32),
        }
    }

    #[test]
    fn prepared_lower_is_verified_before_commit_and_survives_source_exit() {
        let d = tempfile::tempdir().unwrap();
        let source = d.path().join("source");
        let base = d.path().join("base");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file"), b"immutable image").unwrap();
        fs::hard_link(source.join("file"), source.join("alias")).unwrap();
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let req = request();
        let captured = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                prepare_then_capture(
                    &mut server,
                    &req,
                    || crate::environment_snapshot::copy_owned_tree(&source, &base),
                    |inventory| {
                        crate::environment_snapshot::verify_tree(&base, &inventory)?;
                        // The owned lower no longer needs the source projection.
                        fs::remove_dir_all(&source)?;
                        crate::environment_snapshot::verify_tree(&base, &inventory)?;
                        captured.store(true, Ordering::Release);
                        Ok(req.baseline.clone())
                    },
                )
            });
            receive_phase(
                &mut client,
                Duration::from_secs(5),
                CaptureReply::Prepared(req.baseline.clone()),
            )
            .unwrap();
            assert_eq!(fs::read(base.join("alias")).unwrap(), b"immutable image");
            assert!(!captured.load(Ordering::Acquire));
            crate::host_transport::write_host_frame_sync(&mut client, &req).unwrap();
            assert_eq!(worker.join().unwrap().unwrap(), req.baseline);
        });
        assert!(captured.load(Ordering::Acquire));
    }

    #[test]
    fn preparation_failure_never_enters_capture() {
        let (_client, mut server) = UnixStream::pair().unwrap();
        let result = prepare_then_capture(
            &mut server,
            &request(),
            || anyhow::bail!("copy verification failed"),
            |_: ()| -> anyhow::Result<String> { panic!("must not freeze after prepare failure") },
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("copy verification failed")
        );
    }

    #[test]
    fn disconnected_or_stale_client_never_enters_capture() {
        for invalid in 0..4 {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let req = request();
            std::thread::scope(|scope| {
                let worker = scope.spawn(|| {
                    prepare_then_capture(
                        &mut server,
                        &req,
                        || Ok(()),
                        |_| panic!("must not freeze without exact fresh acknowledgement"),
                    )
                });
                receive_phase(
                    &mut client,
                    Duration::from_secs(5),
                    CaptureReply::Prepared(req.baseline.clone()),
                )
                .unwrap();
                if invalid == 0 {
                    drop(client);
                } else {
                    let mut stale = request();
                    if invalid == 3 {
                        stale.protocol = 1;
                    } else if invalid == 1 {
                        stale.source.generation = "d".repeat(32);
                    } else {
                        stale.baseline = "e".repeat(32);
                    }
                    crate::host_transport::write_host_frame_sync(&mut client, &stale).unwrap();
                }
                assert!(worker.join().unwrap().is_err());
            });
        }
    }

    #[test]
    fn legacy_capture_protocol_is_rejected_before_preparation() {
        let legacy =
            serde_json::json!({ "source": request().source, "baseline": request().baseline });
        assert!(serde_json::from_value::<Capture>(legacy).is_err());
        let (_client, mut server) = UnixStream::pair().unwrap();
        let mut req = request();
        req.protocol = 1;
        assert!(
            prepare_then_capture(
                &mut server,
                &req,
                || -> anyhow::Result<()> { panic!("unsupported protocol must not prepare") },
                |_| { panic!("unsupported protocol must not capture") },
            )
            .is_err()
        );
    }

    #[test]
    fn client_preparation_timeout_cannot_start_late_capture() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let req = request();
        let (release, wait) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let worker = scope.spawn(move || {
                prepare_then_capture(
                    &mut server,
                    &req,
                    || {
                        wait.recv().unwrap();
                        Ok(())
                    },
                    |_| panic!("timed-out client must not trigger a late freeze"),
                )
            });
            let error = receive_phase(
                &mut client,
                Duration::from_millis(20),
                CaptureReply::Prepared(request().baseline),
            )
            .unwrap_err();
            assert!(error.to_string().contains("Prepared"));
            drop(client);
            release.send(()).unwrap();
            assert!(worker.join().unwrap().is_err());
        });
    }

    #[test]
    fn phase_reply_rejects_wrong_stage_or_identity_and_preserves_server_error() {
        let req = request();
        for reply in [
            Ok(CaptureReply::Captured(req.baseline.clone())),
            Ok(CaptureReply::Prepared("d".repeat(32))),
            Err("lower preparation failed".to_owned()),
        ] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            crate::host_transport::write_host_frame_sync(&mut server, &reply).unwrap();
            assert!(
                receive_phase(
                    &mut client,
                    Duration::from_secs(1),
                    CaptureReply::Prepared(req.baseline.clone())
                )
                .is_err()
            );
        }
    }

    #[test]
    fn capture_stage_retains_failure_and_does_not_publish_success() {
        let d = tempfile::tempdir().unwrap();
        let result = capture_stage(d.path(), "prepare-lower", || -> anyhow::Result<()> {
            anyhow::bail!("checksum mismatch")
        });
        assert!(format!("{:#}", result.unwrap_err()).contains("checksum mismatch"));
        let receipt: serde_json::Value = serde_json::from_reader(
            File::open(d.path().join("capture-prepare-lower.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(receipt["status"], "failed");
        assert!(receipt["elapsed_ms"].as_u64().is_some());
        assert!(!d.path().join("baseline.json").exists());
    }

    fn config() -> Create {
        serde_json::from_value(serde_json::json!({
            "name":null,"image":"fixture","command":[],"entrypoint":null,"env":{},
            "cwd":null,"user":null,"mounts":[],"ports":[],"cpus":2,"memory_mib":5120,
            "auto_remove":false,"interactive":false,"tty":null,"network":false,"peers":null,
            "managed_memory":true,"managed_memory_compression":true,"branchable":true
        }))
        .unwrap()
    }
    #[test]
    fn retired_branch_endpoint_is_private_and_does_not_follow_replacements() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("control.sock");
        retire_path(&p).unwrap();
        fs::write(&p, b"unrelated").unwrap();
        assert!(retire_path(&p).is_err());
        assert_eq!(fs::read(&p).unwrap(), b"unrelated");
        fs::remove_file(&p).unwrap();
        let listener = UnixListener::bind(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(retire_path(&p).is_err());
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
        retire_path(&p).unwrap();
        assert!(!p.exists());
        drop(listener);
    }

    #[test]
    fn copied_baseline_lower_outlives_source_and_preserves_links() {
        let d = tempfile::tempdir().unwrap();
        let source = d.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file"), b"OCI bytes").unwrap();
        fs::hard_link(source.join("file"), source.join("alias")).unwrap();
        let target = d.path().join("baseline");
        let inventory = crate::environment_snapshot::copy_owned_tree(&source, &target).unwrap();
        fs::remove_dir_all(source).unwrap();
        crate::environment_snapshot::verify_tree(&target, &inventory).unwrap();
        assert_eq!(fs::read(target.join("alias")).unwrap(), b"OCI bytes");
        fs::write(target.join("file"), b"tampered").unwrap();
        assert!(crate::environment_snapshot::verify_tree(&target, &inventory).is_err());
    }

    #[test]
    fn source_profile_is_explicit_and_never_silently_disables_network_or_pool() {
        let c = config();
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            validate_config(&c).unwrap();
        } else {
            assert!(validate_config(&c).is_err());
        }
        for n in 0..6 {
            let mut invalid = config();
            match n {
                0 => invalid.network = true,
                1 => invalid.memory_pool = Some("/pool".into()),
                2 => invalid.managed_memory = false,
                3 => invalid.interactive = true,
                4 => invalid.auto_remove = true,
                _ => invalid.tty = Some([24, 80]),
            }
            assert!(validate_config(&invalid).is_err());
            // Ordinary OCI behavior remains independent of this opt-in profile.
            invalid.branchable = false;
            validate_config(&invalid).unwrap();
        }
    }
    #[test]
    fn baseline_identity_never_selects_an_external_path() {
        for id in [
            "",
            "..",
            "/tmp/base",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/",
            "gggggggggggggggggggggggggggggggg",
        ] {
            assert!(base_path(Path::new("/private"), id).is_err());
        }
        assert_eq!(
            base_path(Path::new("/private"), &"a".repeat(32)).unwrap(),
            Path::new("/private/branches").join("a".repeat(32))
        );
    }
    #[test]
    fn new_profile_defaults_off_for_existing_oci_records() {
        let mut value = serde_json::to_value(config()).unwrap();
        value.as_object_mut().unwrap().remove("branchable");
        assert!(!serde_json::from_value::<Create>(value).unwrap().branchable);
    }
}
