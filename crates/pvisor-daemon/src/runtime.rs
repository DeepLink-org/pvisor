//! VM-only sandbox runtime. No host executor, OCI command or network pull fallback.
//!
//! Integration: add `pvisor` and `pvisor-core` dependencies. Before starting Tokio,
//! the executable MUST call `pvisor::run_krun_internal_if_requested()`. Dispatch
//! the hidden `native-supervisor --sandbox-dir ABSOLUTE_PATH` command to
//! `run_native_supervisor`. Normal daemon shutdown leaves supervisors running.
//!
//! Prepared images are trusted local `<images_dir>/<key>.json` manifests (see
//! PreparedImage). Bootstrap supervises real execd, egress and workload, and
//! provides byte-transparent AF_VSOCK listeners on CID 3, ports 44772/18080.
//! Those listeners bridge to the real services, not synthetic health handlers.
//! The bootstrap receives workload argv without shell interpolation. Its rootfs
//! is an immutable, independently provisioned Linux tree, never the host root.
//!
//! Private state and same-UID socket authentication fence ownership, not hostile
//! code running as the host UID/root. Loopback publications require real service
//! authentication on multi-user hosts. Network policy remains an upper-layer
//! unsupported option; native OverlayNet supplies the VM's outbound network.
//!
//! A required delegated cgroup v2 caps the entire supervisor/VM process tree.
//! Pause/resume uses RunHandle's acknowledged vCPU controls, not cgroup freeze,
//! checkpoint/restore, or rebuilding a VM. Loss of IPC never means Missing.
//! Crash cleanup uses an inode-bound cgroup.kill, never a persisted PID. Durable
//! deletion intent and the supervisor's exclusive lock fence late child launch.
//! A pre-exec callback enters the bound cgroup before supervisor/Tokio allocations.
//! Empty-group proof is durably tombstoned before cgroup/storage reclamation;
//! tombstones retain ownership/binding only, not image/env/control secrets.
//! Endpoint lookup authenticates live supervisor state on every request, without
//! service probes or full cgroup-limit reconciliation. Create, Inspect and resume
//! retain those checks; connection failures belong to the API adapter. Native
//! observation.json caches are neither written nor used as liveness proof.

use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex as StdMutex, Weak},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use fs2::FileExt;
use pvisor::host_transport::{
    authorize_host_peer, read_host_frame as receive_frame, write_host_frame as send_frame,
};
use pvisor_core::host_protocol::{
    AGENTCTL_HOST_MAX_FRAME_BYTES, AGENTCTL_HOST_VERSION, AgentCtlHostError, AgentCtlHostErrorCode,
    AgentCtlHostRequest, AgentCtlHostResponse, AgentCtlTarget, HostAttemptCommand,
    HostSupervisorAuth, HostSupervisorCommand as Operation, HostSupervisorRequest,
    HostSupervisorResult, HostSupervisorState,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::{
    net::{TcpListener, UnixListener, UnixStream},
    process::Command,
    sync::{Mutex, OwnedMutexGuard, Semaphore},
    task::JoinSet,
    time::{sleep, timeout},
};

const EXECD_PORT: u16 = 44772;
const EGRESS_PORT: u16 = 18080;
const IPC_LIMIT: usize = AGENTCTL_HOST_MAX_FRAME_BYTES;
const HEALTH_LIMIT: usize = 16 * 1024;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
const READY_TIMEOUT: Duration = Duration::from_secs(90);
const DELETE_TIMEOUT: Duration = Duration::from_secs(30);
const MIB: u64 = 1024 * 1024;

#[async_trait::async_trait]
pub trait Runtime: Send + Sync {
    async fn preflight(&self) -> Result<()> {
        Ok(())
    }
    async fn create(&self, spec: &RuntimeSpec) -> Result<()>;
    async fn inspect(&self, id: &str) -> Result<RuntimeState>;
    /// Return the confirmed live state, not merely command acceptance.
    /// An error may follow an applied control; retain ownership and reconcile.
    async fn pause(&self, id: &str) -> Result<RuntimeState>;
    /// Confirm Running only after runtime readiness and enforcement checks.
    /// An error does not prove that the VM remained paused.
    async fn resume(&self, id: &str) -> Result<RuntimeState>;
    async fn delete(&self, id: &str) -> Result<()>;
    async fn endpoint(&self, id: &str, port: u16) -> Result<String>;
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSpec {
    pub id: String,
    pub image: String,
    pub entrypoint: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Hard aggregate CPU quota in thousandths of one CPU, not shares/vCPUs.
    pub cpu_millis: u64,
    /// Hard cgroup memory limit, including supervisor/VMM overhead; no swap.
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeState {
    Running,
    Paused,
    Stopped,
    Missing,
}

/// Downcastable create failure. Unknown cleanup must retain admission/intent.
#[derive(Debug)]
pub struct CreateError {
    requires_reconciliation: bool,
    message: String,
}
impl CreateError {
    pub fn requires_reconciliation(&self) -> bool {
        self.requires_reconciliation
    }
    pub fn new(message: impl Into<String>, requires_reconciliation: bool) -> Self {
        Self {
            requires_reconciliation,
            message: message.into(),
        }
    }
}
impl fmt::Display for CreateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for CreateError {}

/// All paths must be absolute and persist unchanged across daemon restarts.
#[derive(Clone)]
pub struct NativeRuntimeConfig {
    /// The daemon's private registry/runtime state directory. Construct inside
    /// Daemon::open's factory while Store holds its exclusive daemon.lock.
    pub state_dir: PathBuf,
    pub owner: String,
    /// Existing delegated cgroup v2 directory with cpu/memory/pids enabled.
    pub cgroup_root: PathBuf,
    pub images_dir: PathBuf,
    /// Trusted daemon executable implementing the hidden command and VM reentry.
    pub executable: PathBuf,
}

/// Local manifest schema. `image` is a key, NOT a registry reference or path.
/// Entrypoint is a long-lived guest bootstrap, followed by requested workload
/// argv (or cmd when the request is empty). No executable runs on the host.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedImage {
    pub rootfs: PathBuf,
    pub entrypoint: Vec<String>,
    #[serde(default)]
    pub cmd: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub library_dir: Option<PathBuf>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    version: u32,
    owner: String,
    token: String,
    generation: String,
    spec: RuntimeSpec,
    image: PreparedImage,
    cgroup: PathBuf,
    cgroup_device: u64,
    cgroup_inode: u64,
    boot_id: String,
}

/// Durable proof published only with exclusive ownership and an empty native
/// cgroup (or a changed kernel boot). Contains no workload/control secrets.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tombstone {
    version: u32,
    owner: String,
    id: String,
    generation: String,
    cgroup: PathBuf,
    cgroup_device: u64,
    cgroup_inode: u64,
    boot_id: String,
}
impl Tombstone {
    fn from_identity(identity: &Identity) -> Self {
        Self {
            version: 1,
            owner: identity.owner.clone(),
            id: identity.spec.id.clone(),
            generation: identity.generation.clone(),
            cgroup: identity.cgroup.clone(),
            cgroup_device: identity.cgroup_device,
            cgroup_inode: identity.cgroup_inode,
            boot_id: identity.boot_id.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunRecord {
    generation: String,
    run_id: String,
    attempt_id: String,
}

type Request = AgentCtlHostRequest<HostSupervisorRequest>;
type Response = AgentCtlHostResponse<HostSupervisorResult>;

// Local adapter preserves RuntimeState's existing persisted representation.
struct Reply {
    state: Option<RuntimeState>,
    endpoint: Option<String>,
}
impl From<RuntimeState> for HostSupervisorState {
    fn from(state: RuntimeState) -> Self {
        match state {
            RuntimeState::Running => Self::Running,
            RuntimeState::Paused => Self::Paused,
            RuntimeState::Stopped => Self::Stopped,
            RuntimeState::Missing => Self::Missing,
        }
    }
}
impl From<HostSupervisorState> for RuntimeState {
    fn from(state: HostSupervisorState) -> Self {
        match state {
            HostSupervisorState::Running => Self::Running,
            HostSupervisorState::Paused => Self::Paused,
            HostSupervisorState::Stopped => Self::Stopped,
            HostSupervisorState::Missing => Self::Missing,
        }
    }
}

fn supervisor_request(identity: &Identity, attempt_id: &str, operation: Operation) -> Request {
    Request {
        version: AGENTCTL_HOST_VERSION,
        request_id: uuid::Uuid::new_v4().to_string(),
        target: Some(AgentCtlTarget {
            job_id: identity.spec.id.clone(),
            attempt_id: Some(attempt_id.into()),
            generation: Some(identity.generation.clone()),
        }),
        command: HostSupervisorRequest {
            auth: HostSupervisorAuth {
                owner: identity.owner.clone(),
                token: identity.token.clone(),
            },
            operation,
        },
    }
}

fn validate_reply(identity: &Identity, request: &Request, response: Response) -> Result<Reply> {
    response.validate(&request.request_id)?;
    let result = response.result?;
    ensure!(
        result.owner == identity.owner && Some(&result.target) == request.target.as_ref(),
        "supervisor identity mismatch"
    );
    Ok(Reply {
        state: Some(result.state.into()),
        endpoint: result.endpoint,
    })
}

pub struct NativeRuntime {
    config: NativeRuntimeConfig,
    operations: SandboxLocks,
}

impl NativeRuntime {
    /// Called under the Store's already-held exclusive lock. An occupied registry
    /// without the native marker may belong to Podman; never adopt it as Missing.
    /// A v2 header activates per-sandbox records and always requires the marker.
    pub fn new(config: NativeRuntimeConfig) -> Result<Self> {
        ensure!(
            cfg!(all(target_os = "linux", target_arch = "x86_64")),
            "native daemon requires Linux x86-64 and delegated cgroup v2"
        );
        validate_owner(&config.owner)?;
        for path in [
            &config.state_dir,
            &config.cgroup_root,
            &config.images_dir,
            &config.executable,
        ] {
            ensure!(path.is_absolute(), "native runtime paths must be absolute");
        }
        private_directory(&config.state_dir)?;
        let marker = config.state_dir.join("owner.json");
        if !marker.try_exists()? {
            reject_occupied_unclaimed_registry(&config.state_dir, &config.owner)?;
        }
        trusted_directory(&config.images_dir)?;
        trusted_file(&config.executable, false)?;
        ensure!(
            config.executable.metadata()?.mode() & 0o111 != 0,
            "daemon is not executable"
        );
        if marker.try_exists()? {
            let owner: String = read_json(&marker)?;
            ensure!(
                owner == config.owner,
                "runtime state belongs to another owner"
            );
        } else {
            publish_new(&marker, &config.owner)?;
        }
        Ok(Self {
            config,
            operations: SandboxLocks::default(),
        })
    }

    fn directory(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        Ok(self.config.state_dir.join(id))
    }

    fn identity(&self, id: &str) -> Result<Option<Identity>> {
        let directory = self.directory(id)?;
        match fs::symlink_metadata(&directory) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            other => {
                other?;
            }
        }
        trusted_private_directory(&directory)?;
        if self.tombstone(id)?.is_some() {
            return Ok(None);
        }
        let identity_path = directory.join("identity.json");
        let preparation_path = directory.join("preparing.json");
        let identity: Identity = if identity_path.try_exists()? {
            read_json(&identity_path)?
        } else if preparation_path.try_exists()? {
            read_json(&preparation_path)?
        } else {
            // Preparation is durably published BEFORE creating any cgroup or
            // launching a child. A directory alone cannot own native execution.
            ensure!(
                !directory.join("started").try_exists()?
                    && !directory.join("run.json").try_exists()?,
                "native identity was lost; retain sandbox and reconcile"
            );
            return Ok(None);
        };
        validate_identity(&identity, &directory)?;
        ensure!(
            identity.owner == self.config.owner && identity.spec.id == id,
            "sandbox ownership mismatch"
        );
        ensure!(
            identity.cgroup.parent() == Some(self.config.cgroup_root.as_path()),
            "sandbox cgroup root changed; restore the original runtime configuration"
        );
        Ok(Some(identity))
    }

    fn tombstone(&self, id: &str) -> Result<Option<Tombstone>> {
        let directory = self.directory(id)?;
        let path = directory.join("tombstone.json");
        if !path.try_exists()? {
            return Ok(None);
        }
        trusted_private_directory(&directory)?;
        let tombstone: Tombstone = read_json(&path)?;
        validate_tombstone(&tombstone, &directory)?;
        ensure!(
            tombstone.owner == self.config.owner && tombstone.id == id,
            "tombstone ownership mismatch"
        );
        ensure!(
            tombstone.cgroup.parent() == Some(self.config.cgroup_root.as_path()),
            "tombstone cgroup root changed"
        );
        for name in ["identity.json", "preparing.json"] {
            let path = directory.join(name);
            if path.try_exists()? {
                let identity: Identity = read_json(&path)?;
                validate_identity(&identity, &directory)?;
                ensure!(
                    identity.owner == tombstone.owner
                        && identity.spec.id == tombstone.id
                        && identity.generation == tombstone.generation
                        && identity.cgroup == tombstone.cgroup
                        && identity.boot_id == tombstone.boot_id,
                    "tombstone does not match surviving identity"
                );
                ensure!(
                    identity.cgroup_inode == 0
                        || (identity.cgroup_inode == tombstone.cgroup_inode
                            && identity.cgroup_device == tombstone.cgroup_device),
                    "tombstone kernel binding mismatch"
                );
            }
        }
        Ok(Some(tombstone))
    }

    fn reclaim(&self, directory: &Path, tombstone: &Tombstone) -> Result<()> {
        // Caller holds owner.lock. Persisted empty-group proof makes retry after
        // rmdir safe, without treating unexpected live missing groups as absent.
        reclaim_cgroup(tombstone)?;
        mark(directory, "deleting")?;
        mark(directory, "deleted")?;
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            if matches!(
                entry.file_name().to_str(),
                Some("owner.lock" | "deleting" | "deleted" | "started" | "tombstone.json")
            ) {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                fs::remove_dir_all(entry.path())?;
            } else {
                fs::remove_file(entry.path())?;
            }
        }
        File::open(directory)?.sync_all()?;
        Ok(())
    }

    async fn finish_tombstone(&self, id: &str, tombstone: &Tombstone) -> Result<()> {
        let directory = self.directory(id)?;
        let lock = lock_file(&directory)?;
        lock.try_lock_exclusive()
            .context("deleted sandbox still has an owner")?;
        self.reclaim(&directory, tombstone)
    }

    async fn request(&self, identity: &Identity, operation: Operation) -> Result<Reply> {
        let directory = self.directory(&identity.spec.id)?;
        timeout(CONTROL_TIMEOUT, async {
            let mut stream = UnixStream::connect(directory.join("control.sock"))
                .await
                .context("supervisor control unavailable; retain sandbox and reconcile")?;
            authorize_host_peer(&stream)?;
            // Never resolve an unspecified target to the current instance.
            // run.json is published once and binds the same Attempt across restart.
            let record: RunRecord = read_json(&directory.join("run.json"))?;
            ensure!(
                record.generation == identity.generation,
                "native run generation mismatch"
            );
            let request = supervisor_request(identity, &record.attempt_id, operation);
            request.validate()?;
            send_frame(&mut stream, &request).await?;
            let response: Response = receive_frame(&mut stream).await?;
            validate_reply(identity, &request, response)
        })
        .await
        .context("supervisor control timed out; retain sandbox and reconcile")?
    }

    fn group(&self, identity: &Identity) -> Result<Option<Group>> {
        Group::open(identity)
    }

    async fn cleanup(&self, identity: &Identity) -> Result<()> {
        if let Some(tombstone) = self.tombstone(&identity.spec.id)? {
            ensure!(
                tombstone.generation == identity.generation,
                "cleanup generation mismatch"
            );
            return self.finish_tombstone(&identity.spec.id, &tombstone).await;
        }
        let directory = self.directory(&identity.spec.id)?;
        mark(&directory, "deleting")?;
        // The lock is not inherited by the VMM (CLOEXEC). A late supervisor
        // must take it and observe deletion intent before joining the cgroup.
        let lock = lock_file(&directory)?;
        timeout(DELETE_TIMEOUT, async {
            loop {
                let locked = lock.try_lock_exclusive().is_ok();
                if let Some(group) = self.group(identity)? {
                    if group.populated()? {
                        ensure!(
                            identity.cgroup_inode != 0,
                            "unbound cgroup is populated; refusing kill"
                        );
                        group.write("cgroup.kill", "1")?;
                    }
                    if locked && !group.populated()? {
                        let tombstone = Tombstone::from_identity(identity);
                        publish_new(&directory.join("tombstone.json"), &tombstone)?;
                        self.reclaim(&directory, &tombstone)?;
                        return Ok(());
                    }
                } else if locked {
                    let tombstone = Tombstone::from_identity(identity);
                    publish_new(&directory.join("tombstone.json"), &tombstone)?;
                    self.reclaim(&directory, &tombstone)?;
                    return Ok(());
                }
                if locked {
                    FileExt::unlock(&lock)?;
                }
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .context("native deletion is uncertain; keep reservation and retry")?
    }

    /// No child has been spawned, but a published binding still requires durable
    /// native absence proof. In particular, never rmdir a committed cgroup first
    /// and leave its identity behind if storage reclamation fails or we crash.
    async fn rollback_preparation(&self, id: &str, group_created: bool) -> Result<()> {
        if let Some(tombstone) = self.tombstone(id)? {
            return self.finish_tombstone(id, &tombstone).await;
        }
        if let Some(identity) = self.identity(id)? {
            return self.cleanup(&identity).await;
        }
        // No preparation record means creation never crossed the durable intent
        // gate. Only this pre-cgroup case may reclaim the directory directly.
        ensure!(
            !group_created,
            "preparation identity unavailable; retain native ownership"
        );
        fs::remove_dir_all(self.directory(id)?)?;
        File::open(&self.config.state_dir)?.sync_all()?;
        Ok(())
    }

    async fn observe(&self, id: &str) -> Result<RuntimeState> {
        if let Some(tombstone) = self.tombstone(id)? {
            self.finish_tombstone(id, &tombstone).await?;
            return Ok(RuntimeState::Missing);
        }
        let Some(identity) = self.identity(id)? else {
            return Ok(RuntimeState::Missing);
        };
        let directory = self.directory(id)?;
        if directory.join("deleted").try_exists()? {
            // Upgrade an older deletion marker only after proving native absence.
            self.cleanup(&identity).await?;
            return Ok(RuntimeState::Missing);
        }
        if directory.join("deleting").try_exists()? {
            bail!("native deletion pending; retain reservation and retry delete");
        }
        let reply = self.request(&identity, Operation::Inspect).await?;
        reply.state.context("supervisor omitted native state")
    }
}

#[async_trait::async_trait]
impl Runtime for NativeRuntime {
    async fn preflight(&self) -> Result<()> {
        ensure!(
            cfg!(all(target_os = "linux", target_arch = "x86_64")),
            "unsupported native VM platform"
        );
        trusted_directory(&self.config.cgroup_root)?;
        verify_cgroup_filesystem(&File::open(&self.config.cgroup_root)?)?;
        let controllers =
            fs::read_to_string(self.config.cgroup_root.join("cgroup.subtree_control"))?;
        for required in ["cpu", "memory", "pids"] {
            ensure!(
                controllers.split_whitespace().any(|c| c == required),
                "delegated cgroup v2 requires enabled {required} controller"
            );
        }
        // Exercise real controller writes without launching project code/VMs.
        let path = self
            .config
            .cgroup_root
            .join(format!("pvisor-preflight-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).context("cgroup root is not delegated/writable")?;
        let result = (|| {
            install_limits(&path, 1000, 64 * MIB)?;
            ensure!(
                path.join("cgroup.kill").is_file(),
                "cgroup.kill is unavailable"
            );
            Ok(())
        })();
        let cleanup = fs::remove_dir(&path);
        result?;
        cleanup?;
        // KVM is a character device, not a regular executable/record.
        let kvm = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/kvm")
            .context("native VM requires usable /dev/kvm")?;
        ensure!(
            unsafe { libc::ioctl(kvm.as_raw_fd(), 0xAE00) } == 12,
            "unsupported KVM API version"
        );
        Ok(())
    }

    async fn create(&self, spec: &RuntimeSpec) -> Result<()> {
        validate_spec(spec)?;
        let _lease = self.operations.lock(&spec.id).await?;
        let directory = self.directory(&spec.id)?;
        ensure!(
            !directory.try_exists()?,
            "sandbox ID was already used; never relaunch/reuse it"
        );
        let image: PreparedImage =
            read_json(&self.config.images_dir.join(format!("{}.json", spec.image)))?;
        validate_image(&image, &self.config.state_dir)?;
        let generation = uuid::Uuid::new_v4().to_string();
        let group_path = self
            .config
            .cgroup_root
            .join(format!("pvisor-{}-{generation}", spec.id));
        // No await from durable directory creation through immutable identity
        // publication: cancellation cannot strand an unbound launched child.
        fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let mut group_created = false;
        let prepared = (|| -> Result<Identity> {
            File::open(&self.config.state_dir)?.sync_all()?;
            let mut identity = Identity {
                version: 1,
                owner: self.config.owner.clone(),
                token: random_token(),
                generation,
                spec: spec.clone(),
                image,
                cgroup: group_path.clone(),
                cgroup_device: 0,
                cgroup_inode: 0,
                boot_id: current_boot_id()?,
            };
            make_run_spec(&identity)?;
            publish_new(&directory.join("preparing.json"), &identity)?;
            fs::create_dir(&group_path)?;
            group_created = true;
            install_limits(&group_path, spec.cpu_millis, spec.memory_bytes)?;
            let metadata = fs::symlink_metadata(&group_path)?;
            identity.cgroup_device = metadata.dev();
            identity.cgroup_inode = metadata.ino();
            publish_new(&directory.join("identity.json"), &identity)?;
            // Native temporary overlays/specs must be reclaimable after a hard
            // supervisor kill, not stranded in a global /tmp directory.
            private_directory(&directory.join("run"))?;
            private_directory(&directory.join("run/tmp"))?;
            let _ = lock_file(&directory)?;
            Ok(identity)
        })();
        let identity = match prepared {
            Ok(identity) => identity,
            Err(_) => {
                let cleanup = self.rollback_preparation(&spec.id, group_created).await;
                return Err(CreateError::new(
                    if cleanup.is_ok() {
                        "native preparation failed; absence confirmed and storage reclaimed"
                    } else {
                        "native preparation failed; rollback uncertain, retain sandbox ID and reconcile"
                    },
                    cleanup.is_err(),
                ).into());
            }
        };
        let result = async {
            // Detached owner: daemon death/disconnect must not kill this child.
            // No workload secrets in argv or host environment.
            let mut command = Command::new(&self.config.executable);
            command
                .env_clear()
                .env("TMPDIR", directory.join("run/tmp"))
                .args(["native-supervisor", "--sandbox-dir"])
                .arg(&directory)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(false);
            let group = self
                .group(&identity)?
                .context("owned launch cgroup disappeared")?;
            group.verify_limits(&identity.spec)?;
            let procs = group.file("cgroup.procs", true)?;
            let owner_lock = lock_file(&directory)?;
            let directory_fd = File::open(&directory)?;
            configure_cgroup_child(&mut command, procs, owner_lock, directory_fd);
            let spawned = command.spawn();
            // Drop the parent's copies of the callback FDs even on exec failure.
            drop(command);
            let mut child = spawned.context("launch native supervisor inside owned cgroup")?;
            // Tokio reaps the exact child on normal completion; never use a saved PID.
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            timeout(READY_TIMEOUT, async {
                loop {
                    match self.request(&identity, Operation::Inspect).await {
                        Ok(reply) if reply.state == Some(RuntimeState::Running) => return Ok(()),
                        Ok(reply) if reply.state == Some(RuntimeState::Stopped) => {
                            bail!("native run stopped during startup")
                        }
                        _ => {}
                    }
                    sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .context("native VM/service readiness timed out")?
        }
        .await;
        if result.is_err() {
            let cleanup = self.cleanup(&identity).await;
            return Err(CreateError::new(
                if cleanup.is_ok() {
                    "native creation failed; VM confirmed absent"
                } else {
                    "native creation failed; cleanup uncertain, retain sandbox ID and reconcile"
                },
                cleanup.is_err(),
            )
            .into());
        }
        Ok(())
    }

    async fn inspect(&self, id: &str) -> Result<RuntimeState> {
        let _lease = self.operations.lock(id).await?;
        self.observe(id).await
    }
    async fn pause(&self, id: &str) -> Result<RuntimeState> {
        let _lease = self.operations.lock(id).await?;
        let identity = self.identity(id)?.context("sandbox is Missing")?;
        ensure_not_deleting(&self.directory(id)?)?;
        let state = self
            .request(&identity, Operation::Pause)
            .await?
            .state
            .context("supervisor omitted pause state")?;
        ensure!(
            state == RuntimeState::Paused,
            "native pause was not confirmed"
        );
        Ok(state)
    }
    async fn resume(&self, id: &str) -> Result<RuntimeState> {
        let _lease = self.operations.lock(id).await?;
        let identity = self.identity(id)?.context("sandbox is Missing")?;
        ensure_not_deleting(&self.directory(id)?)?;
        let state = self
            .request(&identity, Operation::Resume)
            .await?
            .state
            .context("supervisor omitted resume state")?;
        ensure!(
            state == RuntimeState::Running,
            "native resume/readiness was not confirmed"
        );
        Ok(state)
    }
    async fn delete(&self, id: &str) -> Result<()> {
        let _lease = self.operations.lock(id).await?;
        if let Some(tombstone) = self.tombstone(id)? {
            return self.finish_tombstone(id, &tombstone).await;
        }
        let Some(identity) = self.identity(id)? else {
            return Ok(());
        };
        mark(&self.directory(id)?, "deleting")?;
        // A rejected/lost native acknowledgement never authorizes release.
        let _ = self.request(&identity, Operation::Terminate).await;
        self.cleanup(&identity).await
    }
    async fn endpoint(&self, id: &str, port: u16) -> Result<String> {
        validate_port(port)?;
        let _lease = self.operations.lock(id).await?;
        let identity = self.identity(id)?.context("sandbox is Missing")?;
        ensure_not_deleting(&self.directory(id)?)?;
        let reply = self
            .request(&identity, Operation::Endpoint { port })
            .await?;
        ensure!(
            reply.state == Some(RuntimeState::Running),
            "sandbox is not running"
        );
        let endpoint = reply.endpoint.context("supervisor omitted endpoint")?;
        validate_endpoint(&endpoint)?;
        Ok(endpoint)
    }
}

/// Hidden daemon subcommand entrypoint. Do not run native VM reentry here:
/// `run_krun_internal_if_requested` belongs before Tokio/main argument parsing.
/// This owner survives daemon shutdown; only authenticated delete cancels it.
/// The runtime's spawn callback has already placed the child in its bound cgroup
/// before exec. Direct invocation outside that cgroup fails closed; this async
/// entrypoint never migrates pre-existing Tokio memory charges.
pub async fn run_native_supervisor(sandbox_dir: &Path) -> Result<()> {
    ensure!(
        cfg!(all(target_os = "linux", target_arch = "x86_64")),
        "unsupported native VM platform"
    );
    ensure!(
        sandbox_dir.is_absolute(),
        "supervisor directory must be absolute"
    );
    trusted_private_directory(sandbox_dir)?;
    let identity: Identity = read_json(&sandbox_dir.join("identity.json"))?;
    validate_identity(&identity, sandbox_dir)?;
    validate_launch_resources(&identity, sandbox_dir)?;
    ensure!(
        identity.cgroup_inode != 0,
        "supervisor requires committed cgroup binding"
    );
    let lock = lock_file(sandbox_dir)?;
    lock.try_lock_exclusive()
        .context("sandbox already has a supervisor")?;
    // Never resurrect a stopped/crashed/restarted sandbox with the same ID.
    ensure!(
        !sandbox_dir.join("started").try_exists()?,
        "sandbox was already started"
    );
    ensure_not_deleting(sandbox_dir)?;
    let group = Group::open(&identity)?.context("owned cgroup disappeared")?;
    ensure!(
        group.contains_current_process()?,
        "supervisor was not placed in its cgroup before exec"
    );
    group.verify_limits(&identity.spec)?;
    mark(sandbox_dir, "started")?;
    // Membership was installed before exec, so supervisor/Tokio allocations
    // and every subsequently spawned native runner are charged to this budget.
    let control_path = sandbox_dir.join("control.sock");
    ensure!(
        control_path.as_os_str().len() < 104,
        "control socket path too long"
    );
    let listener = UnixListener::bind(&control_path)?;
    fs::set_permissions(&control_path, fs::Permissions::from_mode(0o600))?;

    let mut publications = JoinSet::new();
    let mut endpoints = BTreeMap::new();
    let mut vsock_ports = BTreeMap::new();
    let ports_dir = sandbox_dir.join("ports");
    private_directory(&ports_dir)?;
    for port in [EXECD_PORT, EGRESS_PORT] {
        let path = ports_dir.join(format!("{port}.sock"));
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        endpoints.insert(port, format!("http://{}", listener.local_addr()?));
        vsock_ports.insert(u32::from(port), path.clone());
        publications.spawn(bridge(listener, path));
    }
    let run_dir = sandbox_dir.join("run");
    private_directory(&run_dir)?;
    let settings = pvisor::VmSettings {
        ram_backing: Some(run_dir.join("live-ram")),
        rootfs: Some(identity.image.rootfs.clone()),
        rootfs_immutable: true,
        library_dir: identity.image.library_dir.clone(),
        memory_mib: u32::try_from(identity.spec.memory_bytes / MIB)?,
        cpus: u16::try_from(identity.spec.cpu_millis.div_ceil(1000).min(8))?,
        ..Default::default()
    };
    let executor = pvisor::VmExecutor::new(settings)?.with_vsock_ports(vsock_ports)?;
    let visor = supervisor_builder()
        .storage(run_dir)
        .network(pvisor::NetworkDriverConfig::default())
        .executors(vec![Arc::new(executor)])
        .build();
    let mut run = make_run_spec(&identity)?;
    run.runtime.max_output_bytes = 64 * 1024;
    let handle = pvisor::job_service::RuntimeJobService::start(&visor, run)
        .await
        .context("native VM run admission failed")?;
    publish_new(
        &sandbox_dir.join("run.json"),
        &RunRecord {
            generation: identity.generation.clone(),
            run_id: handle.run_id().to_string(),
            attempt_id: handle.attempt_id().to_string(),
        },
    )?;
    let service = handle.service();
    // Prove acknowledged live controls before advertising any readiness. This
    // pauses/resumes the SAME attempt briefly; no snapshot/rebuild is involved.
    let controls_ready = timeout(READY_TIMEOUT, async {
        wait_control_ready(&service).await?;
        service
            .dispatch(HostAttemptCommand::Operation {
                kind: pvisor_core::OperationKind::RunPause,
            })
            .await?;
        ensure!(
            native_state(&service)? == RuntimeState::Paused,
            "startup pause was not confirmed"
        );
        service
            .dispatch(HostAttemptCommand::Operation {
                kind: pvisor_core::OperationKind::RunResume,
            })
            .await?;
        ensure!(
            native_state(&service)? == RuntimeState::Running,
            "startup resume was not confirmed"
        );
        Ok::<(), anyhow::Error>(())
    })
    .await;
    if !matches!(controls_ready, Ok(Ok(()))) {
        service.dispatch(HostAttemptCommand::Terminate).await?;
        let _ = timeout(DELETE_TIMEOUT, handle.wait()).await;
        bail!("native live VM control verification failed");
    }
    let client = health_client()?;
    // Handle remains in this durable owner. Native transitions are serialized
    // independently of daemon lifetimes, even across IPC client disconnects.
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (mut stream, _) = accepted?;
                if authorize_host_peer(&stream).is_err() { continue; }
                let request = timeout(Duration::from_secs(5), receive_frame::<Request>(&mut stream)).await;
                let Ok(Ok(request)) = request else { continue; };
                let admission = admit_supervisor_request(&identity, &handle.attempt_id().to_string(), &request);
                if let Err(error) = admission {
                    let reply = Response { version: AGENTCTL_HOST_VERSION,
                        request_id: request.request_id, result: Err(error) };
                    let _ = timeout(Duration::from_secs(5), send_frame(&mut stream, &reply)).await;
                    continue;
                }
                let deleted = matches!(request.command.operation, Operation::Terminate);
                let result = execute_control(&service, &identity, sandbox_dir,
                    &client, &endpoints, request.command.operation).await;
                let mut reply = Response {
                    version: AGENTCTL_HOST_VERSION, request_id: request.request_id,
                    result: result.map(|(state, endpoint)| HostSupervisorResult {
                        owner: identity.owner.clone(), target: request.target.expect("validated target"),
                        state: state.into(), endpoint,
                    }).map_err(|_| AgentCtlHostError::new(AgentCtlHostErrorCode::Unavailable,
                        "native control or real service readiness failed; reconcile")),
                };
                // Never return image env, argv, guest output or native paths.
                if deleted && reply.result.is_ok() {
                    // Wait for native runner teardown, NOT merely terminal status.
                    // On uncertainty the parent uses the inode-bound cgroup kill.
                    let waited = timeout(DELETE_TIMEOUT, handle.wait()).await;
                    if !matches!(waited, Ok(Ok(_))) {
                        reply.result = Err(AgentCtlHostError::new(AgentCtlHostErrorCode::Unavailable,
                            "native termination uncertain; reconcile"));
                    }
                    publications.abort_all();
                    while publications.join_next().await.is_some() {}
                    let _ = timeout(Duration::from_secs(5), send_frame(&mut stream, &reply)).await;
                    return Ok(());
                }
                let _ = timeout(Duration::from_secs(5), send_frame(&mut stream, &reply)).await;
            }
            // Publication task death cannot leave a pretend healthy sandbox.
            _ = publications.join_next() => {
                service.dispatch(HostAttemptCommand::Terminate).await?;
                let _ = timeout(DELETE_TIMEOUT, handle.wait()).await;
                bail!("native publication failed");
            }
        }
    }
}

async fn wait_control_ready(service: &pvisor::AttemptService) -> Result<()> {
    loop {
        let status = service.dispatch(HostAttemptCommand::Status).await?.status;
        ensure!(
            status.attempt.executor.kind == pvisor_core::ExecutorKind::VirtualMachine,
            "run is not a VM"
        );
        ensure!(
            !status.state.is_terminal() && status.state != pvisor_core::RunState::Cancelling,
            "attempt ended before control readiness"
        );
        if matches!(
            status.state,
            pvisor_core::RunState::Running | pvisor_core::RunState::Suspended
        ) {
            return Ok(());
        }
        // AttemptService exposes live status, not a readiness subscription. The
        // startup owner supplies READY_TIMEOUT and still proves native pause/resume.
        sleep(Duration::from_millis(10)).await;
    }
}

fn supervisor_builder() -> pvisor::PVisorBuilder {
    // The daemon owns the authenticated supervisor endpoint and its publication.
    pvisor::PVisor::builder().instance_control(false)
}

async fn execute_control(
    service: &pvisor::AttemptService,
    identity: &Identity,
    directory: &Path,
    client: &reqwest::Client,
    endpoints: &BTreeMap<u16, String>,
    operation: Operation,
) -> Result<(RuntimeState, Option<String>)> {
    if matches!(operation, Operation::Terminate) {
        mark(directory, "deleting")?;
        timeout(
            CONTROL_TIMEOUT,
            service.dispatch(HostAttemptCommand::Terminate),
        )
        .await??;
        // Cancellation is not reaping proof; the caller still awaits RunHandle::wait.
        return Ok((RuntimeState::Stopped, None));
    }
    ensure_not_deleting(directory)?;
    let command = match operation {
        Operation::Pause => HostAttemptCommand::Operation {
            kind: pvisor_core::OperationKind::RunPause,
        },
        Operation::Resume => HostAttemptCommand::Operation {
            kind: pvisor_core::OperationKind::RunResume,
        },
        _ => HostAttemptCommand::Status,
    };
    let result = timeout(CONTROL_TIMEOUT, service.dispatch(command)).await??;
    let state = native_status(result.status)?;
    observe_control(
        directory,
        client,
        endpoints,
        operation,
        || Ok(state),
        || {
            Group::open(identity)?
                .context("owned cgroup disappeared")?
                .verify_limits(&identity.spec)
        },
    )
    .await
}

// The supervisor authenticates each request before this path. Endpoint resolves
// only from the current AttemptService, never a durable observation; readiness and
// hard-limit reconciliation belong to Inspect and lifecycle controls.
async fn observe_control(
    directory: &Path,
    client: &reqwest::Client,
    endpoints: &BTreeMap<u16, String>,
    operation: Operation,
    live_state: impl FnOnce() -> Result<RuntimeState>,
    verify_limits: impl FnOnce() -> Result<()>,
) -> Result<(RuntimeState, Option<String>)> {
    ensure_not_deleting(directory)?;
    let state = live_state()?;
    let endpoint = if let Operation::Endpoint { port } = operation {
        validate_port(port)?;
        ensure!(state == RuntimeState::Running, "VM is not running");
        Some(
            endpoints
                .get(&port)
                .context("port is not published")?
                .clone(),
        )
    } else {
        if state == RuntimeState::Running {
            probe_services(client, endpoints).await?;
        }
        verify_limits()?;
        None
    };
    ensure_not_deleting(directory)?;
    Ok((state, endpoint))
}

fn native_state(service: &pvisor::AttemptService) -> Result<RuntimeState> {
    native_status(service.status())
}

fn native_status(status: pvisor_core::RunStatus) -> Result<RuntimeState> {
    use pvisor_core::{ExecutorKind, RunState};
    ensure!(
        status.attempt.executor.kind == ExecutorKind::VirtualMachine,
        "run is not a VM"
    );
    match status.state {
        RunState::Running => Ok(RuntimeState::Running),
        RunState::Suspended => Ok(RuntimeState::Paused),
        state if state.is_terminal() => Ok(RuntimeState::Stopped),
        _ => bail!("native run is transitioning"),
    }
}

fn make_run_spec(identity: &Identity) -> Result<pvisor_core::RunSpec> {
    let image = &identity.image;
    let mut argv = image.entrypoint.clone();
    argv.extend(
        if identity.spec.entrypoint.is_empty() {
            &image.cmd
        } else {
            &identity.spec.entrypoint
        }
        .iter()
        .cloned(),
    );
    let mut env = image.env.clone();
    env.extend(identity.spec.env.clone());
    validate_argv_env(&argv, &env)?;
    ensure!(!argv.is_empty(), "prepared bootstrap is missing");
    let mut run = pvisor_core::RunSpec::process(
        format!("run-{}", identity.generation),
        "opensandbox",
        argv.remove(0),
    );
    let pvisor_core::RunInvocation::Process(process) = &mut run.invocation;
    process.args = argv;
    process.env = env;
    process.inherit_env = false;
    process.cwd = Some("/".into());
    process.stdin = pvisor_core::StdioMode::Null;
    process.stdout = pvisor_core::StdioMode::Capture;
    process.stderr = pvisor_core::StdioMode::Capture;
    // RAM is rounded DOWN; the enclosing cgroup also caps host overhead.
    run.runtime.resource_limits.memory_bytes = Some(identity.spec.memory_bytes / MIB * MIB);
    Ok(run)
}

async fn bridge(listener: TcpListener, path: PathBuf) -> Result<()> {
    let slots = Arc::new(Semaphore::new(128));
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (mut tcp, _) = accepted?;
                let Ok(permit) = slots.clone().try_acquire_owned() else { continue; };
                let path = path.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    if let Ok(Ok(mut guest)) = timeout(Duration::from_secs(5), UnixStream::connect(path)).await {
                        let _ = tokio::io::copy_bidirectional(&mut tcp, &mut guest).await;
                    }
                });
            }
            _ = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

fn health_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()?)
}
async fn probe_services(client: &reqwest::Client, endpoints: &BTreeMap<u16, String>) -> Result<()> {
    let execd = endpoints
        .get(&EXECD_PORT)
        .context("execd is not published")?;
    let egress = endpoints
        .get(&EGRESS_PORT)
        .context("egress is not published")?;
    probe(client, &format!("{execd}/ping")).await?;
    let ready = probe(client, &format!("{execd}/ready")).await?;
    validate_ready(&ready)?;
    probe(client, &format!("{egress}/healthz")).await?;
    Ok(())
}
fn validate_ready(bytes: &[u8]) -> Result<()> {
    let json: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| anyhow!("invalid real execd readiness response"))?;
    ensure!(
        json.get("initialized").and_then(serde_json::Value::as_bool) == Some(true),
        "real execd is not initialized"
    );
    Ok(())
}
async fn probe(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let mut response = client.get(url).send().await?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "real service health failed"
    );
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            body.len() + chunk.len() <= HEALTH_LIMIT,
            "service health response too large"
        );
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// A directory FD pins the exact kernel cgroup; openat avoids a replaced path
/// redirecting cleanup to a different sandbox. No process identifier is stored.
struct Group(File);
impl Group {
    fn open(identity: &Identity) -> Result<Option<Self>> {
        Self::open_binding(
            &identity.cgroup,
            identity.cgroup_device,
            identity.cgroup_inode,
            &identity.boot_id,
        )
    }
    fn open_binding(path: &Path, device: u64, inode: u64, boot_id: &str) -> Result<Option<Self>> {
        // A VMM cannot survive a kernel boot. On the same boot a missing path
        // could be a renamed/live cgroup, so disappearance is NOT deletion proof.
        if boot_id != current_boot_id()? {
            return Ok(None);
        }
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && inode == 0 => {
                return Ok(None);
            }
            Err(e) => {
                return Err(anyhow!(
                    "owned cgroup unavailable; native absence is unproven: {e}"
                ));
            }
        };
        let metadata = file.metadata()?;
        ensure!(
            inode == 0 || (metadata.dev() == device && metadata.ino() == inode),
            "owned cgroup identity changed; refusing control/kill"
        );
        verify_cgroup_filesystem(&file)?;
        Ok(Some(Self(file)))
    }
    fn file(&self, name: &str, write: bool) -> Result<File> {
        let name = std::ffi::CString::new(name)?;
        let flags = if write {
            libc::O_WRONLY
        } else {
            libc::O_RDONLY
        };
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    fn write(&self, name: &str, value: &str) -> Result<()> {
        self.file(name, true)?.write_all(value.as_bytes())?;
        Ok(())
    }
    fn read(&self, name: &str) -> Result<String> {
        let mut value = String::new();
        self.file(name, false)?
            .take(4096)
            .read_to_string(&mut value)?;
        Ok(value)
    }
    fn populated(&self) -> Result<bool> {
        parse_populated(&self.read("cgroup.events")?)
    }
    fn contains_current_process(&self) -> Result<bool> {
        let mut procs = String::new();
        self.file("cgroup.procs", false)?
            .take(IPC_LIMIT as u64)
            .read_to_string(&mut procs)?;
        let pid = std::process::id().to_string();
        Ok(procs.lines().any(|line| line == pid))
    }
    fn verify_limits(&self, spec: &RuntimeSpec) -> Result<()> {
        let cpu = self.read("cpu.max")?;
        let parts = cpu.split_whitespace().collect::<Vec<_>>();
        let quota = cpu_quota(spec.cpu_millis)?.to_string();
        ensure!(
            parts == [quota.as_str(), "100000"],
            "CPU quota changed or unavailable"
        );
        ensure!(
            self.read("memory.max")?.trim() == spec.memory_bytes.to_string(),
            "memory cap changed"
        );
        ensure!(
            self.read("memory.swap.max")?.trim() == "0",
            "swap cap changed"
        );
        ensure!(self.read("pids.max")?.trim() == "512", "PID cap changed");
        ensure!(
            self.read("memory.oom.group")?.trim() == "1",
            "OOM group control changed"
        );
        Ok(())
    }
}
/// Install only async-signal-safe syscalls in the fork child. All files and
/// callback state are allocated/opened by the parent after binding validation.
fn configure_cgroup_child(command: &mut Command, procs: File, owner_lock: File, directory: File) {
    unsafe {
        command.pre_exec(move || {
            enter_cgroup_before_exec(
                procs.as_raw_fd(),
                owner_lock.as_raw_fd(),
                directory.as_raw_fd(),
            )
        });
    }
}
fn enter_cgroup_before_exec(
    procs: libc::c_int,
    owner_lock: libc::c_int,
    directory: libc::c_int,
) -> std::io::Result<()> {
    if unsafe { libc::flock(owner_lock, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let result = (|| {
        for name in [c"deleting", c"deleted", c"tombstone.json", c"started"] {
            let fd = unsafe {
                libc::openat(
                    directory,
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if fd >= 0 {
                unsafe {
                    libc::close(fd);
                }
                return Err(std::io::Error::from_raw_os_error(libc::ECANCELED));
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ENOENT) {
                return Err(error);
            }
        }
        // cgroup.procs accepts 0 as the calling process; no PID formatting,
        // allocation, Rust locking or path lookup is needed after fork.
        loop {
            let written = unsafe { libc::write(procs, b"0".as_ptr().cast(), 1) };
            if written == 1 {
                return Ok(());
            }
            if written < 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(error);
            }
            return Err(std::io::Error::from_raw_os_error(libc::EIO));
        }
    })();
    let unlocked = unsafe { libc::flock(owner_lock, libc::LOCK_UN) };
    if unlocked != 0 && result.is_ok() {
        return Err(std::io::Error::last_os_error());
    }
    result
}

fn reclaim_cgroup(tombstone: &Tombstone) -> Result<()> {
    // A reboot has destroyed the old kernel object. Never touch a new boot's
    // unrelated cgroup at the same path.
    if tombstone.boot_id != current_boot_id()? {
        return Ok(());
    }
    match fs::symlink_metadata(&tombstone.cgroup) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        other => {
            other?;
        }
    }
    let group = Group::open_binding(
        &tombstone.cgroup,
        tombstone.cgroup_device,
        tombstone.cgroup_inode,
        &tombstone.boot_id,
    )?
    .context("reclaim cgroup disappeared")?;
    ensure!(
        !group.populated()?,
        "tombstoned cgroup is populated; refusing reclamation"
    );
    // Empty unbound preparation groups are safe to remove, never to kill.
    fs::remove_dir(&tombstone.cgroup).context("reclaim empty owned cgroup")?;
    Ok(())
}

fn current_boot_id() -> Result<String> {
    let source = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let value = source.trim();
    let uuid = uuid::Uuid::parse_str(value)?;
    ensure!(
        !uuid.is_nil() && uuid.to_string() == value,
        "invalid kernel boot identity"
    );
    Ok(value.to_owned())
}
fn verify_cgroup_filesystem(file: &File) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
        ensure!(
            unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } == 0,
            "cannot identify cgroup filesystem"
        );
        ensure!(
            unsafe { filesystem.assume_init() }.f_type as u64 == 0x63677270,
            "native controls require a real cgroup v2 filesystem"
        );
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        bail!("native controls require Linux cgroup v2")
    }
}
fn cpu_quota(millis: u64) -> Result<u64> {
    ensure!(millis > 0, "CPU quota must be positive");
    millis.checked_mul(100).context("CPU quota overflow")
}
fn install_limits(path: &Path, cpu: u64, memory: u64) -> Result<()> {
    fs::write(path.join("cpu.max"), format!("{} 100000", cpu_quota(cpu)?))?;
    fs::write(path.join("memory.max"), memory.to_string())?;
    fs::write(path.join("memory.swap.max"), "0")?;
    fs::write(path.join("memory.oom.group"), "1")?;
    fs::write(path.join("pids.max"), "512")?;
    ensure!(
        path.join("cgroup.kill").is_file(),
        "native cgroup.kill is unavailable"
    );
    Ok(())
}
fn parse_populated(source: &str) -> Result<bool> {
    let values = source
        .lines()
        .filter_map(|line| line.strip_prefix("populated "))
        .collect::<Vec<_>>();
    match values.as_slice() {
        ["0"] => Ok(false),
        ["1"] => Ok(true),
        _ => bail!("invalid cgroup populated observation"),
    }
}

pub fn validate_spec(spec: &RuntimeSpec) -> Result<()> {
    validate_id(&spec.id)?;
    ensure!(
        !spec.image.is_empty()
            && spec.image.len() <= 128
            && spec
                .image
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            && !spec.image.starts_with('.'),
        "image must be a local prepared-image key"
    );
    cpu_quota(spec.cpu_millis)?;
    ensure!(
        spec.cpu_millis >= 10,
        "native CPU quota must be at least 10 millicores (kernel minimum)"
    );
    ensure!(
        spec.cpu_millis <= 8000,
        "native profile supports at most eight CPU quotas"
    );
    ensure!(
        spec.memory_bytes >= 6 * MIB
            && spec.memory_bytes / MIB <= u64::from(u32::MAX)
            && spec.memory_bytes <= i64::MAX as u64,
        "native memory limit is not representable"
    );
    validate_argv_env(&spec.entrypoint, &spec.env)?;
    Ok(())
}
fn validate_argv_env(argv: &[String], env: &BTreeMap<String, String>) -> Result<()> {
    ensure!(
        argv.first().is_none_or(|s| !s.is_empty()),
        "empty guest executable"
    );
    let mut size = 0usize;
    for arg in argv {
        ensure!(!arg.contains('\0'), "guest argv contains NUL");
        size = size
            .checked_add(arg.len() + 1)
            .context("argv size overflow")?;
    }
    for (key, value) in env {
        ensure!(
            !key.is_empty()
                && key.bytes().enumerate().all(|(i, b)| b == b'_'
                    || b.is_ascii_alphabetic()
                    || (i > 0 && b.is_ascii_digit())),
            "invalid guest environment key"
        );
        ensure!(
            !key.starts_with("PVISOR_") && !key.starts_with("AGENTCTL_"),
            "reserved native control environment key"
        );
        ensure!(!value.contains('\0'), "guest environment contains NUL");
        size = size
            .checked_add(key.len() + value.len() + 2)
            .context("environment size overflow")?;
    }
    ensure!(size <= 64 * 1024, "guest argv/environment exceeds 64 KiB");
    Ok(())
}
fn validate_id(id: &str) -> Result<()> {
    let value = id
        .strip_prefix("sb-")
        .context("sandbox ID must start with sb-")?;
    let uuid = uuid::Uuid::parse_str(value).context("invalid sandbox UUID")?;
    ensure!(
        !uuid.is_nil() && uuid.to_string() == value,
        "sandbox UUID must be canonical lowercase and nonnil"
    );
    Ok(())
}
fn validate_owner(owner: &str) -> Result<()> {
    ensure!(
        !owner.is_empty() && owner.len() <= 256 && !owner.chars().any(char::is_control),
        "invalid native owner"
    );
    Ok(())
}
fn validate_image_shape(image: &PreparedImage, state: &Path) -> Result<()> {
    ensure!(
        image.rootfs.is_absolute()
            && image.rootfs != Path::new("/")
            && !image.rootfs.components().any(|part| matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            ))
            && !state.starts_with(&image.rootfs)
            && !image.rootfs.starts_with(state),
        "prepared rootfs must be absolute and independent of host root and daemon state"
    );
    ensure!(
        !image.entrypoint.is_empty() && image.entrypoint[0].starts_with('/'),
        "prepared guest bootstrap must be absolute"
    );
    validate_argv_env(&image.entrypoint, &image.env)?;
    validate_argv_env(&image.cmd, &BTreeMap::new())?;
    if let Some(path) = &image.library_dir {
        ensure!(path.is_absolute(), "firmware directory must be absolute");
    }
    Ok(())
}
fn validate_image(image: &PreparedImage, state: &Path) -> Result<()> {
    validate_image_shape(image, state)?;
    ensure!(
        image.rootfs.is_absolute(),
        "prepared rootfs must be absolute"
    );
    trusted_directory(&image.rootfs)?;
    let root = image.rootfs.canonicalize()?;
    let state = state.canonicalize()?;
    ensure!(
        root != Path::new("/") && !state.starts_with(&root) && !root.starts_with(&state),
        "prepared rootfs must be independent of host root and daemon state"
    );
    ensure!(
        !image.entrypoint.is_empty() && image.entrypoint[0].starts_with('/'),
        "prepared guest bootstrap must be absolute"
    );
    validate_argv_env(&image.entrypoint, &image.env)?;
    validate_argv_env(&image.cmd, &BTreeMap::new())?;
    if let Some(path) = &image.library_dir {
        ensure!(path.is_absolute(), "firmware directory must be absolute");
        trusted_directory(path)?;
    }
    Ok(())
}
fn validate_identity(identity: &Identity, directory: &Path) -> Result<()> {
    ensure!(
        identity.version == 1,
        "unsupported supervisor identity version"
    );
    validate_owner(&identity.owner)?;
    validate_id(&identity.spec.id)?;
    ensure!(
        directory.file_name().and_then(|s| s.to_str()) == Some(identity.spec.id.as_str()),
        "sandbox directory/ID mismatch"
    );
    ensure!(
        uuid::Uuid::parse_str(&identity.generation)?.to_string() == identity.generation,
        "invalid generation"
    );
    ensure!(
        identity.token.len() == 64 && identity.token.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid control token"
    );
    ensure!(
        identity.cgroup.is_absolute()
            && identity.cgroup.file_name().and_then(|s| s.to_str())
                == Some(format!("pvisor-{}-{}", identity.spec.id, identity.generation).as_str()),
        "invalid cgroup binding"
    );
    ensure!(
        (identity.cgroup_inode == 0) == (identity.cgroup_device == 0),
        "incomplete cgroup binding"
    );
    ensure!(
        uuid::Uuid::parse_str(&identity.boot_id)?.to_string() == identity.boot_id,
        "invalid boot binding"
    );
    // Only ownership/binding is relevant to cleanup. Resource configuration,
    // guest argv/env, rootfs and firmware are exclusively launch validation.
    Ok(())
}
fn validate_tombstone(tombstone: &Tombstone, directory: &Path) -> Result<()> {
    ensure!(tombstone.version == 1, "unsupported tombstone version");
    validate_owner(&tombstone.owner)?;
    validate_id(&tombstone.id)?;
    ensure!(
        directory.file_name().and_then(|s| s.to_str()) == Some(tombstone.id.as_str()),
        "tombstone directory/ID mismatch"
    );
    ensure!(
        uuid::Uuid::parse_str(&tombstone.generation)?.to_string() == tombstone.generation,
        "invalid tombstone generation"
    );
    ensure!(
        uuid::Uuid::parse_str(&tombstone.boot_id)?.to_string() == tombstone.boot_id,
        "invalid tombstone boot binding"
    );
    ensure!(
        tombstone.cgroup.is_absolute()
            && tombstone.cgroup.file_name().and_then(|s| s.to_str())
                == Some(format!("pvisor-{}-{}", tombstone.id, tombstone.generation).as_str()),
        "invalid tombstone cgroup binding"
    );
    ensure!(
        (tombstone.cgroup_inode == 0) == (tombstone.cgroup_device == 0),
        "incomplete tombstone cgroup binding"
    );
    Ok(())
}
fn validate_launch_resources(identity: &Identity, directory: &Path) -> Result<()> {
    validate_spec(&identity.spec)?;
    validate_image(
        &identity.image,
        directory.parent().context("runtime root missing")?,
    )
}
fn validate_port(port: u16) -> Result<()> {
    ensure!(
        matches!(port, EXECD_PORT | EGRESS_PORT),
        "only execd/egress service ports are supported"
    );
    Ok(())
}
fn validate_endpoint(endpoint: &str) -> Result<()> {
    let url = url::Url::parse(endpoint)?;
    ensure!(
        url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.port().is_some_and(|p| p != 0)
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid native loopback endpoint"
    );
    Ok(())
}
fn ensure_not_deleting(directory: &Path) -> Result<()> {
    ensure!(
        !directory.join("deleting").try_exists()? && !directory.join("deleted").try_exists()?,
        "sandbox deletion is pending or complete"
    );
    Ok(())
}
fn admit_supervisor_request(
    identity: &Identity,
    attempt_id: &str,
    request: &Request,
) -> Result<(), AgentCtlHostError> {
    request.validate()?;
    if identity.owner == request.command.auth.owner
        && constant_time_equal(&identity.token, &request.command.auth.token)
        && request.target.as_ref().is_some_and(|target| {
            target.job_id == identity.spec.id
                && target.generation.as_deref() == Some(identity.generation.as_str())
                && target.attempt_id.as_deref() == Some(attempt_id)
        })
    {
        Ok(())
    } else {
        Err(AgentCtlHostError::new(
            AgentCtlHostErrorCode::Unauthorized,
            "supervisor authority mismatch",
        ))
    }
}
fn constant_time_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}
fn random_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Store has already locked and validated this registry. Re-read only the
/// backend admission envelope, bounded by the Store's 16 MiB limit. Do not
/// acquire daemon.lock again (that would conflict with the caller's ownership).
fn reject_occupied_unclaimed_registry(directory: &Path, owner: &str) -> Result<()> {
    #[derive(Deserialize)]
    struct Envelope {
        version: u32,
        owner: String,
        sandboxes: BTreeMap<String, serde::de::IgnoredAny>,
    }
    const REGISTRY_LIMIT: u64 = 16 * 1024 * 1024;
    let path = directory.join("sandboxes.json");
    let file = match trusted_file(&path, true) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(());
        }
        Err(error) => {
            return Err(
                error.context("cannot verify registry backend; refusing native state adoption")
            );
        }
    };
    ensure!(
        file.metadata()?.len() <= REGISTRY_LIMIT,
        "registry exceeds native adoption limit"
    );
    let mut bytes = Vec::new();
    file.take(REGISTRY_LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= REGISTRY_LIMIT,
        "registry exceeds native adoption limit"
    );
    let registry: Envelope = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow!("invalid registry; refusing native state adoption"))?;
    // An empty v2 map is only a header: active records may still own native
    // execution. Only an empty v1 registry can establish a new native marker;
    // any records directory beside v1 is an unactivated migration, not live delta.
    ensure!(
        registry.version != 2,
        "per-record sandbox registry has no native owner.json marker; native ownership is unresolved even with an empty header; restore the original ownership state, do not delete state or reservations"
    );
    ensure!(
        registry.version == 1 && registry.owner == owner,
        "registry ownership/version mismatch; refusing native state adoption"
    );
    ensure!(
        registry.sandboxes.is_empty(),
        "occupied sandbox registry has no native owner.json marker and may own live Podman containers; use a fresh state directory, or clean up every sandbox with the old Podman daemon before switching backends; do not delete state or reservations"
    );
    Ok(())
}

fn trusted_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && (metadata.uid() == unsafe { libc::geteuid() } || metadata.uid() == 0)
            && metadata.mode() & 0o022 == 0,
        "directory is not trusted/private"
    );
    // Reject symlink traversal in ancestors as well as the final component.
    ensure!(
        path.is_absolute() && path.canonicalize()? == path,
        "directory path must be canonical without symlinks"
    );
    Ok(())
}
fn trusted_private_directory(path: &Path) -> Result<()> {
    trusted_directory(path)?;
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0,
        "sandbox state directory must be owned and mode 0700"
    );
    Ok(())
}
fn private_directory(path: &Path) -> Result<()> {
    if !path.try_exists()? {
        fs::DirBuilder::new().mode(0o700).create(path)?;
        File::open(path.parent().context("state parent missing")?)?.sync_all()?;
    }
    trusted_private_directory(path)
}
fn trusted_file(path: &Path, private: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    let forbidden_mode = if private { 0o077 } else { 0o022 };
    ensure!(
        metadata.is_file()
            && (metadata.uid() == unsafe { libc::geteuid() } || (!private && metadata.uid() == 0))
            && metadata.mode() & forbidden_mode == 0,
        "file is not trusted/private"
    );
    Ok(file)
}
fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let file = trusted_file(path, true)?;
    ensure!(
        file.metadata()?.len() <= IPC_LIMIT as u64,
        "native record exceeds size limit"
    );
    let mut bytes = Vec::new();
    file.take(IPC_LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= IPC_LIMIT, "native record exceeds size limit");
    serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid native record"))
}
/// Publish immutable records without replacement. hard_link makes the final
/// name atomic while preserving create-new semantics; fsync both file and dir.
fn publish_new<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context("record parent missing")?;
    let temporary = parent.join(format!(".publish-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let bytes = serde_json::to_vec(value)?;
        ensure!(bytes.len() <= IPC_LIMIT, "native record exceeds size limit");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::hard_link(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result
}

fn mark(directory: &Path, name: &str) -> Result<()> {
    let path = directory.join(name);
    if path.try_exists()? {
        let _ = trusted_file(&path, true)?;
        return Ok(());
    }
    publish_new(&path, &true)
}
fn lock_file(directory: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory.join("owner.lock"))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "invalid supervisor ownership lock"
    );
    Ok(file)
}

type LockEntries = StdMutex<BTreeMap<String, Weak<SandboxKey>>>;
#[derive(Default)]
struct SandboxLocks {
    entries: Arc<LockEntries>,
}
struct SandboxKey {
    id: String,
    lock: Arc<Mutex<()>>,
    entries: Weak<LockEntries>,
}
struct SandboxLease {
    key: Arc<SandboxKey>,
    guard: Option<OwnedMutexGuard<()>>,
}
impl SandboxLocks {
    async fn lock(&self, id: &str) -> Result<SandboxLease> {
        validate_id(id)?;
        let key = {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            match entries.get(id).and_then(Weak::upgrade) {
                Some(key) => key,
                None => {
                    let key = Arc::new(SandboxKey {
                        id: id.into(),
                        lock: Arc::new(Mutex::new(())),
                        entries: Arc::downgrade(&self.entries),
                    });
                    entries.insert(id.into(), Arc::downgrade(&key));
                    key
                }
            }
        };
        let mut lease = SandboxLease { key, guard: None };
        lease.guard = Some(lease.key.lock.clone().lock_owned().await);
        Ok(lease)
    }
}
impl Drop for SandboxKey {
    fn drop(&mut self) {
        if let Some(entries) = self.entries.upgrade() {
            let mut entries = entries.lock().unwrap_or_else(|e| e.into_inner());
            if entries
                .get(&self.id)
                .is_some_and(|entry| std::ptr::eq(entry.as_ptr(), self))
            {
                entries.remove(&self.id);
            }
        }
    }
}
impl Drop for SandboxLease {
    fn drop(&mut self) {
        drop(self.guard.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pvisor::host_transport::{encode_host_frame, read_host_frame_sync, write_host_frame_sync};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // A VM-labelled process has no native control channel. It exercises shared
    // admission/status and fail-closed dispatch, not VM enforcement or readiness.
    struct VmLabelledProcess;
    #[async_trait::async_trait]
    impl pvisor::RunExecutor for VmLabelledProcess {
        fn descriptor(&self) -> pvisor_core::ExecutorPlan {
            let mut plan = pvisor::ProcessExecutor::default().descriptor();
            plan.kind = pvisor_core::ExecutorKind::VirtualMachine;
            plan.isolation = pvisor_core::IsolationKind::VirtualMachine;
            plan
        }
        fn supports(&self, invocation: &pvisor_core::RunInvocation) -> bool {
            pvisor::ProcessExecutor::default().supports(invocation)
        }
        async fn execute(&self, session: &pvisor::Session) -> pvisor::ExecutorOutput {
            pvisor::ProcessExecutor::default().execute(session).await
        }
    }

    #[tokio::test]
    async fn supervisor_builder_disables_auto_endpoint_and_rejects_custom_socket() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("unexpected.sock");
        let visor = supervisor_builder().control_socket(&socket).build();
        let result = pvisor::job_service::RuntimeJobService::start(
            &visor,
            pvisor_core::RunSpec::process("socket-conflict", "test", "/bin/true"),
        )
        .await;
        assert!(matches!(result, Err(pvisor::PVisorError::InvalidSpec(_))));
        assert!(!socket.exists());
    }

    #[tokio::test]
    async fn shared_job_start_and_attempt_dispatch_do_not_fabricate_vm_controls() {
        let temp = tempfile::tempdir().unwrap();
        let visor = supervisor_builder()
            .storage(temp.path().join("run"))
            .network(pvisor::NetworkDriverConfig::new(
                pvisor::OverlayNetMode::Off,
                Default::default(),
            ))
            .executors(vec![Arc::new(VmLabelledProcess)])
            .build();
        let mut run = pvisor_core::RunSpec::process("shared-attempt", "test", "/bin/sleep");
        let pvisor_core::RunInvocation::Process(process) = &mut run.invocation;
        process.args = vec!["60".into()];
        let handle = pvisor::job_service::RuntimeJobService::start(&visor, run)
            .await
            .unwrap();
        let service = handle.service();
        let ready = timeout(CONTROL_TIMEOUT, wait_control_ready(&service)).await;
        if !matches!(ready, Ok(Ok(()))) {
            service
                .dispatch(HostAttemptCommand::Terminate)
                .await
                .unwrap();
            let _ = timeout(DELETE_TIMEOUT, handle.wait()).await;
            panic!("mock executor did not reach running state");
        }
        assert!(
            handle.control_socket().is_err(),
            "unexpected auto VM endpoint"
        );
        let status = service.dispatch(HostAttemptCommand::Status).await.unwrap();
        assert_eq!(status.status.attempt.attempt_id, *handle.attempt_id());
        assert_eq!(native_status(status.status).unwrap(), RuntimeState::Running);
        assert_eq!(native_state(&service).unwrap(), RuntimeState::Running);

        let (identity, _) = identity_fixture(temp.path());
        let error = execute_control(
            &service,
            &identity,
            temp.path(),
            &health_client().unwrap(),
            &BTreeMap::new(),
            Operation::Pause,
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<AgentCtlHostError>().unwrap().code,
            AgentCtlHostErrorCode::Unavailable
        );
        // The same adapter must still fence admission after durable delete intent.
        mark(temp.path(), "deleting").unwrap();
        assert!(
            execute_control(
                &service,
                &identity,
                temp.path(),
                &health_client().unwrap(),
                &BTreeMap::new(),
                Operation::Inspect,
            )
            .await
            .is_err()
        );
        service
            .dispatch(HostAttemptCommand::Terminate)
            .await
            .unwrap();
        timeout(DELETE_TIMEOUT, handle.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(service.status().state.is_terminal());
    }

    const ID: &str = "sb-12345678-1234-4234-8234-123456789abc";
    fn spec() -> RuntimeSpec {
        RuntimeSpec {
            id: ID.into(),
            image: "prepared-v1".into(),
            entrypoint: vec!["/bin/work".into(), "a b;$HOME".into()],
            env: BTreeMap::new(),
            cpu_millis: 500,
            memory_bytes: 64 * MIB,
        }
    }
    fn identity_fixture(temp: &Path) -> (Identity, PathBuf) {
        let state = temp.join("state");
        private_directory(&state).unwrap();
        let directory = state.join(ID);
        private_directory(&directory).unwrap();
        let rootfs = temp.join("rootfs");
        private_directory(&rootfs).unwrap();
        let generation = "22345678-1234-4234-8234-123456789abc".to_owned();
        let identity = Identity {
            version: 1,
            owner: "node-owner".into(),
            token: "a".repeat(64),
            generation: generation.clone(),
            spec: spec(),
            image: PreparedImage {
                rootfs,
                entrypoint: vec!["/guest/bootstrap".into(), "--".into()],
                cmd: vec!["/guest/default".into()],
                env: BTreeMap::from([("PATH".into(), "/guest/bin".into())]),
                library_dir: None,
            },
            cgroup: temp
                .join("groups")
                .join(format!("pvisor-{ID}-{generation}")),
            cgroup_device: 1,
            cgroup_inode: 1,
            boot_id: "32345678-1234-4234-8234-123456789abc".into(),
        };
        (identity, directory)
    }
    fn runtime_fixture(identity: &Identity, directory: &Path) -> NativeRuntime {
        NativeRuntime {
            config: NativeRuntimeConfig {
                state_dir: directory.parent().unwrap().to_owned(),
                owner: identity.owner.clone(),
                cgroup_root: identity.cgroup.parent().unwrap().to_owned(),
                images_dir: directory.parent().unwrap().to_owned(),
                executable: directory.join("unused"),
            },
            operations: SandboxLocks::default(),
        }
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn constructor_fixture(temp: &Path) -> NativeRuntimeConfig {
        let state = temp.join("daemon");
        private_directory(&state).unwrap();
        let images = temp.join("images");
        private_directory(&images).unwrap();
        let executable = temp.join("daemon-executable");
        fs::write(&executable, b"not executed by constructor tests").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        NativeRuntimeConfig {
            state_dir: state,
            owner: "22345678-1234-4234-8234-123456789abc".into(),
            cgroup_root: temp.join("groups"),
            images_dir: images,
            executable,
        }
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn occupied_legacy_registry_is_rejected_under_existing_store_lock_without_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let config = constructor_fixture(temp.path());
        let registry_path = config.state_dir.join("sandboxes.json");
        let registry = serde_json::json!({"version": 1, "owner": config.owner, "sandboxes": {
            (ID): {
                "sandbox": {"id": ID, "status": {"state": "Running"},
                    "createdAt": "2026-10-06T00:00:00Z", "image": {"uri": "old-podman-image:local"},
                    "entrypoint": ["/bin/work"], "metadata": {}},
                "env": {"GUEST_SECRET": "legacy-sensitive-value"},
                "endpoint_token": "legacy-secret-0123456789-0123456789",
                "cpu_millis": 500, "memory_bytes": 67108864
            }
        }});
        publish_new(&registry_path, &registry).unwrap();
        let bytes = fs::read(&registry_path).unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(config.state_dir.join("daemon.lock"))
            .unwrap();
        lock.try_lock_exclusive().unwrap();
        let error = NativeRuntime::new(config.clone())
            .err()
            .expect("legacy state must not be adopted");
        let message = error.to_string();
        assert!(message.contains("Podman"));
        assert!(message.contains("fresh state directory"));
        assert!(!message.contains("legacy-secret"));
        assert!(!config.state_dir.join("owner.json").exists());
        assert!(!config.state_dir.join(ID).exists());
        assert_eq!(fs::read(&registry_path).unwrap(), bytes);
        // The rejected constructor neither reacquires nor releases Store's lock.
        let another = OpenOptions::new()
            .read(true)
            .write(true)
            .open(config.state_dir.join("daemon.lock"))
            .unwrap();
        assert!(another.try_lock_exclusive().is_err());
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn empty_registry_can_be_claimed_but_native_occupied_restart_keeps_its_marker() {
        let temp = tempfile::tempdir().unwrap();
        let config = constructor_fixture(temp.path());
        let path = config.state_dir.join("sandboxes.json");
        publish_new(
            &path,
            &serde_json::json!({"version": 1, "owner": config.owner, "sandboxes": {}}),
        )
        .unwrap();
        NativeRuntime::new(config.clone()).unwrap();
        assert_eq!(
            read_json::<String>(&config.state_dir.join("owner.json")).unwrap(),
            config.owner
        );
        fs::write(&path, serde_json::to_vec(&serde_json::json!({"version": 1, "owner": config.owner, "sandboxes": {(ID): {"native": true}}})).unwrap()).unwrap();
        let bytes = fs::read(&path).unwrap();
        NativeRuntime::new(config.clone()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let mut foreign = config;
        foreign.owner = "other-node".into();
        assert!(NativeRuntime::new(foreign).is_err());
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn per_record_header_without_native_marker_never_claims_or_mutates_state() {
        for with_record in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let config = constructor_fixture(temp.path());
            let path = config.state_dir.join("sandboxes.json");
            publish_new(
                &path,
                &serde_json::json!({"version": 2, "owner": config.owner, "sandboxes": {}}),
            )
            .unwrap();
            let records = config.state_dir.join("records");
            private_directory(&records).unwrap();
            let record = records.join(format!("{ID}.json"));
            if with_record {
                publish_new(&record, &serde_json::json!({"native": true})).unwrap();
            }
            let header_bytes = fs::read(&path).unwrap();
            let record_bytes = with_record.then(|| fs::read(&record).unwrap());
            let error = NativeRuntime::new(config.clone()).err().unwrap();
            assert!(error.to_string().contains("no native owner.json marker"));
            assert!(!config.state_dir.join("owner.json").exists());
            assert!(!config.state_dir.join(ID).exists());
            assert_eq!(fs::read(&path).unwrap(), header_bytes);
            assert_eq!(record.exists(), with_record);
            if let Some(bytes) = record_bytes {
                assert_eq!(fs::read(&record).unwrap(), bytes);
            }
            // Existing native ownership still permits a v2 restart.
            publish_new(&config.state_dir.join("owner.json"), &config.owner).unwrap();
            NativeRuntime::new(config).unwrap();
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn malformed_unclaimed_registry_does_not_create_a_native_marker() {
        for json in [
            serde_json::json!({"version": 1, "owner": "other-node", "sandboxes": {}}),
            serde_json::json!({"version": 2, "owner": "22345678-1234-4234-8234-123456789abc", "sandboxes": {}}),
            serde_json::json!({"version": 1, "owner": "22345678-1234-4234-8234-123456789abc", "sandboxes": null}),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let config = constructor_fixture(temp.path());
            publish_new(&config.state_dir.join("sandboxes.json"), &json).unwrap();
            assert!(NativeRuntime::new(config.clone()).is_err());
            assert!(!config.state_dir.join("owner.json").exists());
        }
    }
    #[test]
    fn launch_resources_can_disappear_without_invalidating_cleanup_binding() {
        let temp = tempfile::tempdir().unwrap();
        let (mut identity, directory) = identity_fixture(temp.path());
        let firmware = temp.path().join("firmware");
        private_directory(&firmware).unwrap();
        identity.image.library_dir = Some(firmware.clone());
        publish_new(&directory.join("identity.json"), &identity).unwrap();
        assert!(validate_launch_resources(&identity, &directory).is_ok());
        fs::remove_dir_all(&identity.image.rootfs).unwrap();
        fs::remove_dir_all(&firmware).unwrap();
        assert!(validate_launch_resources(&identity, &directory).is_err());
        assert!(validate_identity(&identity, &directory).is_ok());
        assert!(
            runtime_fixture(&identity, &directory)
                .identity(ID)
                .unwrap()
                .is_some()
        );
    }
    #[test]
    fn preexec_join_is_fenced_by_the_owner_lock_and_all_launch_markers() {
        let temp = tempfile::tempdir().unwrap();
        let (_, directory) = identity_fixture(temp.path());
        let procs_path = temp.path().join("mock-procs");
        let procs = OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .open(&procs_path)
            .unwrap();
        let directory_fd = File::open(&directory).unwrap();
        let busy = lock_file(&directory).unwrap();
        busy.try_lock_exclusive().unwrap();
        let child_lock = lock_file(&directory).unwrap();
        assert!(
            enter_cgroup_before_exec(
                procs.as_raw_fd(),
                child_lock.as_raw_fd(),
                directory_fd.as_raw_fd()
            )
            .is_err()
        );
        drop(busy);
        assert!(fs::read(&procs_path).unwrap().is_empty());
        for name in ["deleting", "deleted", "tombstone.json", "started"] {
            mark(&directory, name).unwrap();
            assert!(
                enter_cgroup_before_exec(
                    procs.as_raw_fd(),
                    child_lock.as_raw_fd(),
                    directory_fd.as_raw_fd()
                )
                .is_err()
            );
            assert!(fs::read(&procs_path).unwrap().is_empty());
            // Test cases are independent; production never removes launch fences.
            fs::remove_file(directory.join(name)).unwrap();
        }
        // A rejected callback must release the lock, not strand cleanup.
        busy_lock_check(&directory);
    }
    fn busy_lock_check(directory: &Path) {
        let lock = lock_file(directory).unwrap();
        lock.try_lock_exclusive().unwrap();
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn child_join_write_is_visible_to_the_exec_image() {
        let temp = tempfile::tempdir().unwrap();
        let (_, directory) = identity_fixture(temp.path());
        let path = temp.path().join("mock-procs");
        let procs = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        // Ordinary file tests syscall ordering only, not native enforcement.
        let mut command = Command::new("/bin/cat");
        command.arg(&path).stdout(Stdio::piped());
        configure_cgroup_child(
            &mut command,
            procs,
            lock_file(&directory).unwrap(),
            File::open(&directory).unwrap(),
        );
        let output = command.output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"0");
        drop(command);
        busy_lock_check(&directory);
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn committed_prelaunch_failure_uses_durable_cleanup_not_raw_directory_removal() {
        let temp = tempfile::tempdir().unwrap();
        let (identity, directory) = identity_fixture(temp.path());
        assert_ne!(identity.boot_id, current_boot_id().unwrap());
        publish_new(&directory.join("identity.json"), &identity).unwrap();
        let run = directory.join("run");
        private_directory(&run).unwrap();
        fs::write(run.join("tmp"), b"blocks post-commit run/tmp preparation").unwrap();
        assert!(private_directory(&run.join("tmp")).is_err());
        let runtime = runtime_fixture(&identity, &directory);
        runtime.rollback_preparation(ID, true).await.unwrap();
        // Old-boot absence is authoritative, but even before any child launch
        // the committed binding must leave a durable proof and ID-use fence.
        let proof: Tombstone = read_json(&directory.join("tombstone.json")).unwrap();
        assert_eq!(proof.generation, identity.generation);
        assert_eq!(proof.cgroup_inode, identity.cgroup_inode);
        assert!(directory.join("deleting").is_file());
        assert!(directory.join("deleted").is_file());
        assert!(!directory.join("identity.json").exists());
        assert!(!run.exists());
        assert_eq!(runtime.inspect(ID).await.unwrap(), RuntimeState::Missing);
        assert!(runtime.create(&spec()).await.is_err());
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn committed_prelaunch_rollback_without_native_proof_preserves_binding_and_group() {
        let temp = tempfile::tempdir().unwrap();
        let (mut identity, directory) = identity_fixture(temp.path());
        identity.boot_id = current_boot_id().unwrap();
        fs::create_dir_all(&identity.cgroup).unwrap();
        let metadata = identity.cgroup.metadata().unwrap();
        identity.cgroup_device = metadata.dev();
        identity.cgroup_inode = metadata.ino();
        publish_new(&directory.join("identity.json"), &identity).unwrap();
        fs::write(directory.join("run"), b"blocks post-commit run preparation").unwrap();
        assert!(private_directory(&directory.join("run")).is_err());
        let runtime = runtime_fixture(&identity, &directory);
        // An ordinary empty directory cannot prove native absence. Old raw
        // rollback would incorrectly rmdir it and discard the committed record.
        assert!(runtime.rollback_preparation(ID, true).await.is_err());
        assert!(identity.cgroup.is_dir());
        assert!(directory.join("identity.json").is_file());
        assert!(!directory.join("tombstone.json").exists());
        assert!(runtime.inspect(ID).await.is_err());
        // Same-boot disappearance without proof remains uncertain on retry.
        fs::remove_dir(&identity.cgroup).unwrap();
        assert!(runtime.rollback_preparation(ID, true).await.is_err());
        assert!(directory.join("identity.json").is_file());
        assert!(!directory.join("tombstone.json").exists());
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "requires PVISOR_TEST_CGROUP_ROOT: a real delegated cgroup v2 directory"]
    async fn native_committed_prelaunch_rollback_tombstones_before_rmdir_and_recovers_storage_retry()
     {
        let temp = tempfile::tempdir().unwrap();
        let (mut identity, directory) = identity_fixture(temp.path());
        let root = PathBuf::from(
            std::env::var_os("PVISOR_TEST_CGROUP_ROOT")
                .expect("explicit delegated cgroup root required"),
        );
        trusted_directory(&root).unwrap();
        verify_cgroup_filesystem(&File::open(&root).unwrap()).unwrap();
        identity.generation = uuid::Uuid::new_v4().to_string();
        identity.cgroup = root.join(format!("pvisor-{ID}-{}", identity.generation));
        fs::create_dir(&identity.cgroup).unwrap();
        identity.boot_id = current_boot_id().unwrap();
        let metadata = identity.cgroup.metadata().unwrap();
        identity.cgroup_device = metadata.dev();
        identity.cgroup_inode = metadata.ino();
        publish_new(&directory.join("identity.json"), &identity).unwrap();
        let run = directory.join("run");
        private_directory(&run).unwrap();
        fs::write(run.join("tmp"), b"post-commit prelaunch failure").unwrap();
        assert!(private_directory(&run.join("tmp")).is_err());
        let runtime = runtime_fixture(&identity, &directory);
        runtime.rollback_preparation(ID, true).await.unwrap();
        assert!(!identity.cgroup.exists());
        assert!(directory.join("tombstone.json").is_file());
        assert!(!directory.join("identity.json").exists());
        // Reproduce the persisted crash window after rmdir but before removal
        // of the secret record: retry must use proof, not missing-group inference.
        publish_new(&directory.join("identity.json"), &identity).unwrap();
        assert!(Group::open(&identity).is_err());
        runtime_fixture(&identity, &directory)
            .rollback_preparation(ID, true)
            .await
            .unwrap();
        assert!(!directory.join("identity.json").exists());
        assert_eq!(runtime.inspect(ID).await.unwrap(), RuntimeState::Missing);
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn tombstone_resumes_reclamation_after_cgroup_rmdir_without_image_or_identity() {
        let temp = tempfile::tempdir().unwrap();
        let (mut identity, directory) = identity_fixture(temp.path());
        identity.boot_id = current_boot_id().unwrap();
        // Simulates crash after empty-group proof and successful rmdir, before
        // storage reclamation. Without that proof, same-boot absence stays unknown.
        assert!(Group::open(&identity).is_err());
        publish_new(&directory.join("identity.json"), &identity).unwrap();
        publish_new(&directory.join("preparing.json"), &identity).unwrap();
        let run = directory.join("run");
        private_directory(&run).unwrap();
        fs::write(run.join("bulky-ram"), vec![0u8; 128 * 1024]).unwrap();
        mark(&directory, "started").unwrap();
        mark(&directory, "deleting").unwrap();
        publish_new(
            &directory.join("tombstone.json"),
            &Tombstone::from_identity(&identity),
        )
        .unwrap();
        fs::remove_dir_all(&identity.image.rootfs).unwrap();
        let external = temp.path().join("must-survive");
        private_directory(&external).unwrap();
        fs::write(external.join("keep"), b"outside sandbox").unwrap();
        std::os::unix::fs::symlink(&external, directory.join("ports")).unwrap();
        let runtime = runtime_fixture(&identity, &directory);
        runtime.delete(ID).await.unwrap();
        assert_eq!(runtime.inspect(ID).await.unwrap(), RuntimeState::Missing);
        assert!(!run.exists());
        assert!(!directory.join("identity.json").exists());
        assert!(!directory.join("preparing.json").exists());
        assert!(!directory.join("ports").exists());
        assert!(external.join("keep").is_file());
        let proof = fs::read_to_string(directory.join("tombstone.json")).unwrap();
        let rootfs_path = identity.image.rootfs.to_string_lossy().into_owned();
        for secret in [&identity.token, &identity.spec.entrypoint[1], &rootfs_path] {
            assert!(!proof.contains(secret));
        }
        assert!(directory.join("owner.lock").is_file());
        assert!(directory.join("started").is_file());
        assert!(runtime.create(&spec()).await.is_err());
        assert!(run_native_supervisor(&directory).await.is_err());
        runtime_fixture(&identity, &directory)
            .delete(ID)
            .await
            .unwrap();
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "requires PVISOR_TEST_CGROUP_ROOT: a real delegated cgroup v2 directory"]
    async fn native_empty_bound_cgroup_is_removed_even_after_image_resources_disappear() {
        let temp = tempfile::tempdir().unwrap();
        let (mut identity, directory) = identity_fixture(temp.path());
        let root = PathBuf::from(
            std::env::var_os("PVISOR_TEST_CGROUP_ROOT")
                .expect("explicit delegated cgroup root required"),
        );
        trusted_directory(&root).unwrap();
        verify_cgroup_filesystem(&File::open(&root).unwrap()).unwrap();
        identity.generation = uuid::Uuid::new_v4().to_string();
        identity.cgroup = root.join(format!("pvisor-{ID}-{}", identity.generation));
        fs::create_dir(&identity.cgroup).unwrap();
        identity.boot_id = current_boot_id().unwrap();
        let metadata = identity.cgroup.metadata().unwrap();
        identity.cgroup_device = metadata.dev();
        identity.cgroup_inode = metadata.ino();
        let firmware = temp.path().join("firmware");
        private_directory(&firmware).unwrap();
        identity.image.library_dir = Some(firmware.clone());
        publish_new(&directory.join("identity.json"), &identity).unwrap();
        fs::remove_dir_all(&identity.image.rootfs).unwrap();
        fs::remove_dir_all(&firmware).unwrap();
        let runtime = runtime_fixture(&identity, &directory);
        runtime.cleanup(&identity).await.unwrap();
        assert!(!identity.cgroup.exists());
        assert!(!directory.join("identity.json").exists());
        assert_eq!(runtime.inspect(ID).await.unwrap(), RuntimeState::Missing);
        runtime.delete(ID).await.unwrap();
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn tombstone_cannot_release_a_busy_owner_or_another_owners_reservation() {
        let temp = tempfile::tempdir().unwrap();
        let (identity, directory) = identity_fixture(temp.path());
        publish_new(&directory.join("identity.json"), &identity).unwrap();
        publish_new(
            &directory.join("tombstone.json"),
            &Tombstone::from_identity(&identity),
        )
        .unwrap();
        let owner = lock_file(&directory).unwrap();
        owner.try_lock_exclusive().unwrap();
        let runtime = runtime_fixture(&identity, &directory);
        assert!(runtime.inspect(ID).await.is_err());
        assert!(directory.join("identity.json").is_file());
        drop(owner);
        let mut foreign = runtime_fixture(&identity, &directory);
        foreign.config.owner = "other-owner".into();
        assert!(foreign.delete(ID).await.is_err());
        assert!(directory.join("identity.json").is_file());
    }
    #[tokio::test]
    async fn mismatched_tombstone_generation_never_reclaims_surviving_identity() {
        let temp = tempfile::tempdir().unwrap();
        let (identity, directory) = identity_fixture(temp.path());
        publish_new(&directory.join("identity.json"), &identity).unwrap();
        let mut proof = Tombstone::from_identity(&identity);
        proof.generation = uuid::Uuid::new_v4().to_string();
        proof.cgroup = identity
            .cgroup
            .parent()
            .unwrap()
            .join(format!("pvisor-{ID}-{}", proof.generation));
        publish_new(&directory.join("tombstone.json"), &proof).unwrap();
        let runtime = runtime_fixture(&identity, &directory);
        assert!(runtime.delete(ID).await.is_err());
        assert!(runtime.inspect(ID).await.is_err());
        assert!(directory.join("identity.json").is_file());
    }
    #[test]
    fn ownership_requires_owner_id_generation_and_secret() {
        let temp = tempfile::tempdir().unwrap();
        let (identity, _) = identity_fixture(temp.path());
        let request = supervisor_request(&identity, "attempt", Operation::Inspect);
        assert!(admit_supervisor_request(&identity, "attempt", &request).is_ok());
        for field in 0..8 {
            let mut bad = request.clone();
            match field {
                0 => bad.command.auth.owner.push('x'),
                1 => bad.command.auth.token.replace_range(0..1, "z"),
                2 => bad.target.as_mut().unwrap().job_id.push('x'),
                3 => bad.target.as_mut().unwrap().generation = Some("other".into()),
                4 => bad.target.as_mut().unwrap().attempt_id = Some("other".into()),
                5 => bad.version += 1,
                6 => bad.target.as_mut().unwrap().generation = None,
                _ => bad.target.as_mut().unwrap().attempt_id = None,
            }
            let expected = if field == 5 {
                AgentCtlHostErrorCode::VersionMismatch
            } else {
                AgentCtlHostErrorCode::Unauthorized
            };
            assert_eq!(
                admit_supervisor_request(&identity, "attempt", &bad)
                    .unwrap_err()
                    .code,
                expected,
                "field {field}"
            );
        }
        let response = Response {
            version: AGENTCTL_HOST_VERSION,
            request_id: request.request_id.clone(),
            result: Ok(HostSupervisorResult {
                owner: identity.owner.clone(),
                target: request.target.clone().unwrap(),
                state: HostSupervisorState::Running,
                endpoint: None,
            }),
        };
        assert!(validate_reply(&identity, &request, response.clone()).is_ok());
        let mut bad = response.clone();
        bad.request_id.push('x');
        assert!(validate_reply(&identity, &request, bad).is_err());
        let mut bad = response.clone();
        bad.version += 1;
        assert!(validate_reply(&identity, &request, bad).is_err());
        for field in 0..4 {
            let mut bad = response.clone();
            let result = bad.result.as_mut().unwrap();
            match field {
                0 => result.owner.push('x'),
                1 => result.target.job_id.push('x'),
                2 => result.target.generation = None,
                _ => result.target.attempt_id = None,
            }
            assert!(validate_reply(&identity, &request, bad).is_err());
        }
    }
    #[test]
    fn bootstrap_argv_and_env_are_guest_only_and_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let (mut identity, _) = identity_fixture(temp.path());
        identity
            .spec
            .env
            .insert("PATH".into(), "/custom/guest/bin".into());
        identity.spec.memory_bytes += MIB - 1;
        let run = make_run_spec(&identity).unwrap();
        let pvisor_core::RunInvocation::Process(process) = run.invocation;
        assert_eq!(process.program, "/guest/bootstrap");
        assert_eq!(process.args, ["--", "/bin/work", "a b;$HOME"]);
        assert!(!process.inherit_env);
        assert_eq!(process.env["PATH"], "/custom/guest/bin");
        assert_eq!(run.runtime.resource_limits.memory_bytes, Some(64 * MIB));
        identity.spec.entrypoint.clear();
        let pvisor_core::RunInvocation::Process(process) =
            make_run_spec(&identity).unwrap().invocation;
        assert_eq!(process.args, ["--", "/guest/default"]);
    }
    #[test]
    fn durable_identity_rejects_rebinding_or_host_root() {
        let temp = tempfile::tempdir().unwrap();
        let (mut identity, directory) = identity_fixture(temp.path());
        assert!(validate_identity(&identity, &directory).is_ok());
        identity.cgroup_inode = 0;
        assert!(validate_identity(&identity, &directory).is_err());
        identity.cgroup_inode = 1;
        identity.image.rootfs = PathBuf::from("/");
        assert!(validate_launch_resources(&identity, &directory).is_err());
        assert!(validate_identity(&identity, &directory).is_ok());
    }
    #[test]
    fn deleting_is_monotonic_and_fences_late_launch() {
        let temp = tempfile::tempdir().unwrap();
        let (_, directory) = identity_fixture(temp.path());
        let owner = lock_file(&directory).unwrap();
        owner.try_lock_exclusive().unwrap();
        assert!(lock_file(&directory).unwrap().try_lock_exclusive().is_err());
        mark(&directory, "deleting").unwrap();
        assert!(ensure_not_deleting(&directory).is_err());
        drop(owner);
        let late = lock_file(&directory).unwrap();
        late.try_lock_exclusive().unwrap();
        assert!(ensure_not_deleting(&directory).is_err());
        mark(&directory, "deleting").unwrap();
        mark(&directory, "deleted").unwrap();
        assert!(ensure_not_deleting(&directory).is_err());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn fake_controller_files_and_replaced_cgroups_are_not_enforcement() {
        let temp = tempfile::tempdir().unwrap();
        let (mut identity, _) = identity_fixture(temp.path());
        fs::create_dir_all(&identity.cgroup).unwrap();
        identity.boot_id = current_boot_id().unwrap();
        let metadata = identity.cgroup.metadata().unwrap();
        identity.cgroup_device = metadata.dev();
        identity.cgroup_inode = metadata.ino();
        assert!(Group::open(&identity).is_err()); // Not a real cgroup filesystem.
        identity.cgroup_inode += 1;
        assert!(Group::open(&identity).is_err()); // Immutable kernel identity mismatch.
        fs::remove_dir(&identity.cgroup).unwrap();
        assert!(Group::open(&identity).is_err()); // Missing on same boot is uncertainty.
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn reboot_cleanup_is_durable_and_recovery_never_relaunches() {
        let temp = tempfile::tempdir().unwrap();
        let (mut identity, directory) = identity_fixture(temp.path());
        let firmware = temp.path().join("firmware");
        private_directory(&firmware).unwrap();
        identity.image.library_dir = Some(firmware.clone());
        assert_ne!(identity.boot_id, current_boot_id().unwrap());
        publish_new(&directory.join("identity.json"), &identity).unwrap();
        mark(&directory, "started").unwrap();
        fs::remove_dir_all(&identity.image.rootfs).unwrap();
        fs::remove_dir_all(&firmware).unwrap();
        let config = NativeRuntimeConfig {
            state_dir: directory.parent().unwrap().to_owned(),
            owner: identity.owner.clone(),
            cgroup_root: identity.cgroup.parent().unwrap().to_owned(),
            images_dir: temp.path().to_owned(),
            executable: temp.path().join("unused"),
        };
        let runtime = NativeRuntime {
            config: config.clone(),
            operations: SandboxLocks::default(),
        };
        runtime.cleanup(&identity).await.unwrap();
        assert_eq!(runtime.inspect(ID).await.unwrap(), RuntimeState::Missing);
        let recovered = NativeRuntime {
            config,
            operations: SandboxLocks::default(),
        };
        assert_eq!(recovered.inspect(ID).await.unwrap(), RuntimeState::Missing);
        assert!(recovered.create(&spec()).await.is_err()); // Tombstone forbids reuse.
        assert!(directory.join("started").is_file());
        assert!(!directory.join("identity.json").exists());
        assert!(directory.join("tombstone.json").is_file());
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn partial_preparation_can_be_reconciled_without_signalling_a_pid() {
        let temp = tempfile::tempdir().unwrap();
        let (mut identity, directory) = identity_fixture(temp.path());
        identity.boot_id = current_boot_id().unwrap();
        identity.cgroup_device = 0;
        identity.cgroup_inode = 0;
        publish_new(&directory.join("preparing.json"), &identity).unwrap();
        let runtime = NativeRuntime {
            config: NativeRuntimeConfig {
                state_dir: directory.parent().unwrap().to_owned(),
                owner: identity.owner.clone(),
                cgroup_root: identity.cgroup.parent().unwrap().to_owned(),
                images_dir: temp.path().to_owned(),
                executable: temp.path().join("unused"),
            },
            operations: SandboxLocks::default(),
        };
        runtime
            .cleanup(&runtime.identity(ID).unwrap().unwrap())
            .await
            .unwrap();
        assert_eq!(runtime.inspect(ID).await.unwrap(), RuntimeState::Missing);
        assert!(directory.join("deleted").is_file());
    }
    #[tokio::test]
    async fn durable_running_observation_is_not_live_or_ready_proof() {
        let temp = tempfile::tempdir().unwrap();
        let (identity, directory) = identity_fixture(temp.path());
        publish_new(&directory.join("identity.json"), &identity).unwrap();
        // A legacy cache is deliberately left behind, but never read as proof.
        publish_new(
            &directory.join("observation.json"),
            &serde_json::json!({
                "generation": identity.generation,
                "run_id": "run-old",
                "attempt_id": "attempt-old",
                "state": "Running",
            }),
        )
        .unwrap();
        let runtime = NativeRuntime {
            config: NativeRuntimeConfig {
                state_dir: directory.parent().unwrap().to_owned(),
                owner: identity.owner.clone(),
                cgroup_root: identity.cgroup.parent().unwrap().to_owned(),
                images_dir: temp.path().to_owned(),
                executable: temp.path().join("unused"),
            },
            operations: SandboxLocks::default(),
        };
        assert!(runtime.inspect(ID).await.is_err());
        assert!(runtime.endpoint(ID, EXECD_PORT).await.is_err());
    }
    #[tokio::test]
    async fn lost_identity_with_run_record_is_not_missing_or_endpoint_proof() {
        let temp = tempfile::tempdir().unwrap();
        let (identity, directory) = identity_fixture(temp.path());
        publish_new(
            &directory.join("run.json"),
            &RunRecord {
                generation: identity.generation.clone(),
                run_id: "run-old".into(),
                attempt_id: "attempt-old".into(),
            },
        )
        .unwrap();
        let runtime = runtime_fixture(&identity, &directory);
        assert!(runtime.identity(ID).is_err());
        assert!(runtime.inspect(ID).await.is_err());
        assert!(runtime.endpoint(ID, EXECD_PORT).await.is_err());
        assert!(directory.join("run.json").exists());
    }

    #[test]
    fn unclaimed_registry_guard_is_a_portable_contract() {
        let temp = tempfile::tempdir().unwrap();
        let owner = "node-owner";
        let path = temp.path().join("sandboxes.json");
        publish_new(
            &path,
            &serde_json::json!({"version": 1, "owner": owner, "sandboxes": {(ID): {}}}),
        )
        .unwrap();
        let bytes = fs::read(&path).unwrap();
        assert!(reject_occupied_unclaimed_registry(temp.path(), owner).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"version": 1, "owner": owner, "sandboxes": {}}))
                .unwrap(),
        )
        .unwrap();
        assert!(reject_occupied_unclaimed_registry(temp.path(), owner).is_ok());
        assert!(reject_occupied_unclaimed_registry(temp.path(), "foreign-owner").is_err());
        let records = temp.path().join("records");
        private_directory(&records).unwrap();
        let record = records.join(format!("{ID}.json"));
        publish_new(&record, &serde_json::json!({"native": true})).unwrap();
        let record_bytes = fs::read(&record).unwrap();
        // Prepared records have no authority until the root header activates v2.
        assert!(reject_occupied_unclaimed_registry(temp.path(), owner).is_ok());
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"version": 2, "owner": owner, "sandboxes": {}}))
                .unwrap(),
        )
        .unwrap();
        let header_bytes = fs::read(&path).unwrap();
        assert!(reject_occupied_unclaimed_registry(temp.path(), owner).is_err());
        assert_eq!(fs::read(&path).unwrap(), header_bytes);
        assert_eq!(fs::read(&record).unwrap(), record_bytes);
        assert!(!temp.path().join("owner.json").exists());
    }

    #[tokio::test]
    async fn endpoint_bypasses_probes_and_limits_but_inspect_and_resume_do_not() {
        let temp = tempfile::tempdir().unwrap();
        let (_, directory) = identity_fixture(temp.path());
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let endpoints = BTreeMap::from([
            (EXECD_PORT, endpoint.clone()),
            (EGRESS_PORT, endpoint.clone()),
        ]);
        let client = health_client().unwrap();
        let result = observe_control(
            &directory,
            &client,
            &endpoints,
            Operation::Endpoint { port: EXECD_PORT },
            || Ok(RuntimeState::Running),
            || panic!("Endpoint must not verify full cgroup limits"),
        )
        .await
        .unwrap();
        assert_eq!(result, (RuntimeState::Running, Some(endpoint)));
        assert!(
            timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
        assert!(!directory.join("observation.json").exists());

        let server = tokio::spawn(async move {
            let mut paths = Vec::new();
            for _ in 0..6 {
                let (mut stream, _) = timeout(Duration::from_secs(5), listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(
                        timeout(Duration::from_secs(5), stream.read_u8())
                            .await
                            .unwrap()
                            .unwrap(),
                    );
                    assert!(request.len() < HEALTH_LIMIT);
                }
                paths.push(
                    String::from_utf8(request)
                        .unwrap()
                        .lines()
                        .next()
                        .unwrap()
                        .to_owned(),
                );
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\n{\"initialized\":true}").await.unwrap();
            }
            paths
        });
        for operation in [Operation::Inspect, Operation::Resume] {
            let verified = std::cell::Cell::new(false);
            assert!(
                observe_control(
                    &directory,
                    &client,
                    &endpoints,
                    operation,
                    || Ok(RuntimeState::Running),
                    || {
                        verified.set(true);
                        bail!("changed hard limits")
                    },
                )
                .await
                .is_err()
            );
            assert!(verified.get());
        }
        assert_eq!(
            server.await.unwrap(),
            [
                "GET /ping HTTP/1.1",
                "GET /ready HTTP/1.1",
                "GET /healthz HTTP/1.1",
                "GET /ping HTTP/1.1",
                "GET /ready HTTP/1.1",
                "GET /healthz HTTP/1.1",
            ]
        );
        assert!(!directory.join("observation.json").exists());
    }

    #[tokio::test]
    async fn endpoint_requires_current_live_running_state_and_deletion_fence() {
        let temp = tempfile::tempdir().unwrap();
        let (identity, directory) = identity_fixture(temp.path());
        let client = health_client().unwrap();
        let endpoints = BTreeMap::from([(EXECD_PORT, "http://127.0.0.1:1".into())]);
        for state in [
            RuntimeState::Paused,
            RuntimeState::Stopped,
            RuntimeState::Missing,
        ] {
            assert!(
                observe_control(
                    &directory,
                    &client,
                    &endpoints,
                    Operation::Endpoint { port: EXECD_PORT },
                    || Ok(state),
                    || panic!("Endpoint must not verify limits"),
                )
                .await
                .is_err()
            );
        }
        assert!(
            observe_control(
                &directory,
                &client,
                &endpoints,
                Operation::Endpoint { port: EXECD_PORT },
                || bail!("native run is failed or transitioning"),
                || panic!("Endpoint must not verify limits"),
            )
            .await
            .is_err()
        );
        for marker in ["deleting", "deleted"] {
            mark(&directory, marker).unwrap();
            assert!(
                observe_control(
                    &directory,
                    &client,
                    &endpoints,
                    Operation::Endpoint { port: EXECD_PORT },
                    || panic!("deletion must fence live state resolution"),
                    || panic!("Endpoint must not verify limits"),
                )
                .await
                .is_err()
            );
            fs::remove_file(directory.join(marker)).unwrap();
        }
        assert!(
            runtime_fixture(&identity, &directory)
                .endpoint(ID, EXECD_PORT)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn inspect_still_rejects_unready_services() {
        let temp = tempfile::tempdir().unwrap();
        let (_, directory) = identity_fixture(temp.path());
        let client = health_client().unwrap();
        let endpoints = BTreeMap::from([(EXECD_PORT, "http://127.0.0.1:1".into())]);
        assert!(
            observe_control(
                &directory,
                &client,
                &endpoints,
                Operation::Inspect,
                || Ok(RuntimeState::Running),
                || Ok(()),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn bounded_authenticated_control_frames_round_trip() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let (identity, _) = identity_fixture(temp.path());
        let request = supervisor_request(
            &identity,
            "attempt",
            Operation::Endpoint { port: EXECD_PORT },
        );
        send_frame(&mut sender, &request).await.unwrap();
        let received: Request = receive_frame(&mut receiver).await.unwrap();
        assert_eq!(received.request_id, request.request_id);
        assert_eq!(received.target, request.target);
        assert_eq!(received.command.auth.token, request.command.auth.token);
        assert!(matches!(
            received.command.operation,
            Operation::Endpoint { port: EXECD_PORT }
        ));
        authorize_host_peer(&receiver).unwrap();
    }
    #[tokio::test]
    async fn shared_sync_client_and_daemon_exchange_host_envelopes() {
        let temp = tempfile::tempdir().unwrap();
        let (identity, _) = identity_fixture(temp.path());
        let request = supervisor_request(&identity, "attempt", Operation::Inspect);
        let client_request = request.clone();
        let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        let mut server = UnixStream::from_std(server).unwrap();
        let client = tokio::task::spawn_blocking(move || {
            client.set_read_timeout(Some(CONTROL_TIMEOUT)).unwrap();
            client.set_write_timeout(Some(CONTROL_TIMEOUT)).unwrap();
            write_host_frame_sync(&mut client, &client_request).unwrap();
            read_host_frame_sync::<Response>(&mut client).unwrap()
        });
        authorize_host_peer(&server).unwrap();
        let received: Request = receive_frame(&mut server).await.unwrap();
        assert!(admit_supervisor_request(&identity, "attempt", &received).is_ok());
        let reply = Response {
            version: AGENTCTL_HOST_VERSION,
            request_id: received.request_id.clone(),
            result: Ok(HostSupervisorResult {
                owner: identity.owner.clone(),
                target: received.target.clone().unwrap(),
                state: HostSupervisorState::Running,
                endpoint: None,
            }),
        };
        send_frame(&mut server, &reply).await.unwrap();
        let response = client.await.unwrap();
        assert_eq!(
            validate_reply(&identity, &request, response).unwrap().state,
            Some(RuntimeState::Running)
        );
    }

    #[tokio::test]
    async fn shared_codec_leaves_version_validation_to_the_envelope() {
        let temp = tempfile::tempdir().unwrap();
        let (identity, _) = identity_fixture(temp.path());
        let mut request = supervisor_request(&identity, "attempt", Operation::Inspect);
        request.version = AGENTCTL_HOST_VERSION + 1;
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender
            .write_all(&encode_host_frame(&request).unwrap())
            .await
            .unwrap();
        let received: Request = receive_frame(&mut receiver).await.unwrap();
        assert_eq!(received.version, request.version);
        assert_eq!(
            admit_supervisor_request(&identity, "attempt", &received)
                .unwrap_err()
                .code,
            AgentCtlHostErrorCode::VersionMismatch
        );
    }

    #[test]
    fn readiness_requires_upstream_boolean_initialization() {
        assert!(validate_ready(br#"{"initialized":true}"#).is_ok());
        for bytes in [
            br#"{"initialized":false}"#.as_slice(),
            br#"{"initialized":"true"}"#,
            b"{}",
            b"OK",
        ] {
            assert!(validate_ready(bytes).is_err());
        }
    }
    #[test]
    fn quotas_are_rates_not_cpu_time_or_vcpu_counts() {
        assert_eq!(cpu_quota(500).unwrap(), 50000);
        assert_eq!(cpu_quota(1500).unwrap(), 150000);
        assert!(cpu_quota(0).is_err());
        assert!(cpu_quota(u64::MAX).is_err());
    }
    #[test]
    fn inputs_fail_closed() {
        assert!(validate_spec(&spec()).is_ok());
        for image in ["../host", "/rootfs", "registry/image:tag", "", ".hidden"] {
            let mut s = spec();
            s.image = image.into();
            assert!(validate_spec(&s).is_err());
        }
        for id in [
            "sb-00000000-0000-0000-0000-000000000000",
            "../../x",
            "sb-12345678123442348234123456789abc",
        ] {
            assert!(validate_id(id).is_err());
        }
        let mut s = spec();
        s.env
            .insert("PVISOR_KRUN_RUNNER_SPEC".into(), "attack".into());
        assert!(validate_spec(&s).is_err());
        s.env.clear();
        s.env.insert("PATH".into(), "/guest/bin".into());
        assert!(validate_spec(&s).is_ok());
        s.entrypoint.push("nul\0".into());
        assert!(validate_spec(&s).is_err());
        assert!(validate_port(22).is_err());
    }
    #[test]
    fn population_is_never_inferred_from_missing_or_malformed_events() {
        assert!(!parse_populated("populated 0\nfrozen 0\n").unwrap());
        assert!(parse_populated("populated 1\n").unwrap());
        for value in ["", "populated 2", "populated 0\npopulated 1", "frozen 0"] {
            assert!(parse_populated(value).is_err());
        }
    }
    #[test]
    fn endpoint_is_actual_loopback_http_not_arbitrary_authority() {
        assert!(validate_endpoint("http://127.0.0.1:3456").is_ok());
        for value in [
            "http://localhost:3456",
            "http://127.0.0.1:0",
            "https://127.0.0.1:3456",
            "http://user@127.0.0.1:3456",
            "http://127.0.0.1:3456/path",
        ] {
            assert!(validate_endpoint(value).is_err());
        }
    }
    #[test]
    fn create_errors_remain_downcastable() {
        for requires in [true, false] {
            let error: anyhow::Error = CreateError::new("failure", requires).into();
            assert_eq!(
                error
                    .downcast_ref::<CreateError>()
                    .unwrap()
                    .requires_reconciliation(),
                requires
            );
        }
    }
    #[test]
    fn records_are_private_immutable_and_symlink_safe() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("record.json");
        publish_new(&path, &"first").unwrap();
        assert!(publish_new(&path, &"second").is_err());
        assert_eq!(read_json::<String>(&path).unwrap(), "first");
        assert_eq!(path.metadata().unwrap().mode() & 0o777, 0o600);
        let link = temp.path().join("link.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_json::<String>(&link).is_err());
    }
    #[tokio::test]
    async fn locks_are_per_sandbox_and_reclaimed() {
        let locks = SandboxLocks::default();
        let first = locks.lock(ID).await.unwrap();
        assert_eq!(locks.entries.lock().unwrap().len(), 1);
        assert!(
            timeout(Duration::from_millis(1), locks.lock(ID))
                .await
                .is_err()
        );
        let second = locks
            .lock("sb-22345678-1234-4234-8234-123456789abc")
            .await
            .unwrap();
        drop(second);
        drop(first);
        assert!(locks.entries.lock().unwrap().is_empty());
    }

    #[test]
    fn token_comparison_checks_all_bytes() {
        assert!(constant_time_equal("abc", "abc"));
        assert!(!constant_time_equal("abc", "abd"));
        assert!(!constant_time_equal("abc", "abcd"));
    }
}
