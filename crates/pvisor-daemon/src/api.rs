//! Node-local sandbox service and detached memory-pool contracts.
//!
//! HTTP is the sandbox lifecycle entry point; wire schemas and authentication
//! remain private. Dropping the API service preserves native VMs and pool RAM.
//! State directories and same-UID IPC require a trusted host account; they do
//! not protect against hostile code running as that account or root.
//!
//! Implementation modules are unavailable to consumers:
//! ```compile_fail
//! use pvisor_daemon::runtime::Runtime;
//! ```
//! Sandbox mutations are available only through authenticated HTTP:
//! ```compile_fail
//! use pvisor_daemon::api::Daemon;
//! async fn bypass(daemon: std::sync::Arc<Daemon>) {
//!     daemon.delete("sb-12345678-1234-4234-8234-123456789abc").await;
//! }
//! ```
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

pub use crate::daemon::Daemon;
pub use crate::runtime::NativeRuntime;

/// Operator-provisioned E2B template settings, retained with each native identity.
/// These are not client-selected resources or a template build service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct E2bTemplate {
    /// Aggregate supervisor/VM CPU quota in millicores, from 10 through 8000.
    pub cpu_millis: u64,
    /// Hard supervisor/VM memory limit in bytes, including host overhead.
    pub memory_bytes: u64,
    /// Exact version of the real envd binary initialized by the trusted bootstrap.
    /// A three-component numeric version of at least 0.1.0 is required.
    pub envd_version: String,
}

/// One admitted launch; validation is required before creating native state.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSpec {
    /// Canonical lowercase, nonnil sb-UUID; never reuse an admitted ID.
    pub id: String,
    /// Trusted local prepared-image key, 1..128 ASCII alphanumeric/underscore/dot/hyphen bytes; no leading dot.
    pub image: String,
    /// Guest argv without shell interpolation; empty selects the prepared image command.
    pub entrypoint: Vec<String>,
    /// Guest environment overrides; valid POSIX keys, no NUL or reserved control names.
    /// Combined argv/environment encoding must fit 64 KiB; no host env inheritance.
    pub env: BTreeMap<String, String>,
    /// Hard aggregate CPU quota in thousandths of one CPU, not shares/vCPUs.
    pub cpu_millis: u64,
    /// Hard cgroup memory limit, including supervisor/VMM overhead; no swap.
    /// At least 6 MiB, with MiB count fitting u32 and bytes fitting i64.
    pub memory_bytes: u64,
    /// Selects real envd on port 49983 instead of OpenSandbox execd/egress.
    /// Omission preserves existing OpenSandbox identities across restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e2b: Option<E2bTemplate>,
}

/// Confirmed live runtime observation; errors are distinct from absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeState {
    /// Live VM with confirmed readiness and resource enforcement.
    Running,
    /// Acknowledged pause of the same live VM; resources and TTL remain charged.
    Paused,
    /// Workload terminated; explicit confirmed deletion still releases its reservation.
    Stopped,
    /// Confirmed absence of the owned native instance, never unavailable control.
    Missing,
}

/// Downcastable create failure. Unknown cleanup must retain admission/intent.
#[derive(Debug)]
pub struct CreateError {
    /// False only after verified cleanup; true retains admission and durable intent.
    pub requires_reconciliation: bool,
    /// Diagnostic detail; must not contain credentials. Unknown error types also retain capacity.
    pub message: String,
}
/// All paths must be absolute and persist unchanged across daemon restarts.
#[derive(Clone)]
pub struct NativeRuntimeConfig {
    /// The daemon's private registry/runtime state directory. Construct inside
    /// Daemon::open's factory while Store holds its exclusive daemon.lock.
    pub state_dir: PathBuf,
    /// Durable node owner passed by the locked daemon factory; must match existing state.
    pub owner: String,
    /// Existing delegated cgroup v2 directory with cpu/memory/pids enabled.
    pub cgroup_root: PathBuf,
    /// Trusted local prepared-image repository; no network pull fallback.
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
    /// Private Docker image-store directory owning the pinned V2 pool.
    pub rootfs: PathBuf,
    /// Trusted long-lived guest bootstrap argv; must not execute on the host.
    pub entrypoint: Vec<String>,
    /// Default workload argv appended when the launch request supplies none.
    #[serde(default)]
    pub cmd: Vec<String>,
    /// Template guest environment, overridden by launch env without host inheritance.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Optional absolute firmware directory, retained for the lifetime of native state.
    #[serde(default)]
    pub library_dir: Option<PathBuf>,
    /// E2B-only image profile. Its bootstrap must initialize real envd with
    /// `PVISOR_ENVD_ACCESS_TOKEN` and publish it over guest vsock port 49983.
    /// Omission retains the OpenSandbox execd/egress bootstrap contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e2b: Option<E2bTemplate>,
    /// Pinned shared-pool image. Its projection owner lives until VM teardown;
    /// mutable registry tags are never resolved during sandbox creation.
    pub pooled_image: pvisor::docker::Image,
    /// Managed execd/vsock bootstrap with pVisor's explicit unsupported-policy
    /// adapter, rather than a guest upstream egress process. Requires pooled_image.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub native_opensandbox: bool,
    /// Managed real E2B envd, initialized privately by the supervisor.
    /// Requires a pooled image and an E2B resource/version profile.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub native_e2b: bool,
    /// Use the shared pvisor-guest process/file engine and host protocol adapters.
    /// No execd/envd is launched. Requires a pooled image; old manifests omit it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub native_guest: bool,
}

/// Storage selection, fixed for the lifetime of this pool state directory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum PoolMode {
    /// Linux raw 4 KiB pages shared by private mappings and kernel COW;
    /// no userfaultfd. Old macOS records use this label for legacy compression.
    #[default]
    Physical,
    /// Deduplicated compressed blocks. Linux retains sealed client backing;
    /// macOS HVF restores through the live pool and requires it to stay alive.
    Compressed,
    /// Raw host pages with private COW mappings: 16 KiB on Apple Silicon,
    /// 4 KiB on Linux. Separate from the legacy macOS `physical` record label.
    PhysicalPages,
}

/// Immutable pool budgets, all positive. Default mode is compressed on macOS,
/// physical elsewhere; persisted missing mode retains the legacy physical label.
/// Default budgets: 512 MiB payload, 32768 objects/references and 32 connections.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolConfig {
    /// Missing in older persisted configurations means physical sharing.
    #[serde(default)]
    pub mode: PoolMode,
    /// Positive encoded/page-storage byte budget; excludes indexes, threads and client RAM.
    pub max_bytes: usize,
    /// Positive number of distinct retained objects.
    pub max_objects: usize,
    /// Positive maximum simultaneous clients.
    pub max_connections: usize,
    /// Positive maximum pinned references per client.
    pub max_references: usize,
}
/// Pinned OpenSandbox protocol version implemented by the HTTP adapter.
pub const OPENSANDBOX_VERSION: &str = "1.1.0";
/// Upstream source revision defining the supported OpenSandbox profile.
pub const OPENSANDBOX_COMMIT: &str = "b1a29cf93a823a95913f7943010febb3f29de05c";

/// Node-wide admission, authentication and durable registry configuration.
pub struct Config {
    /// Private, current-user-owned durable directory, exclusively locked for this owner.
    pub state_dir: PathBuf,
    /// Lifecycle secret of at least 32 bytes; authenticate ingress and keep records private.
    pub api_key: String,
    /// Externally reachable authority (host:port), without a scheme or path.
    pub public_endpoint: String,
    /// Positive slot budget, including paused, failed and uncertain launches.
    pub max_sandboxes: usize,
    /// Positive aggregate admitted hard CPU quota, in millicores.
    pub cpu_millis: u64,
    /// Positive aggregate admitted hard memory limits; excludes pool and API overhead.
    pub memory_bytes: u64,
    /// Maximum requested TTL, 60 seconds through 365 days; omission requests manual cleanup.
    pub max_timeout_seconds: u64,
}

/// Validates an owned runtime input without launching a workload or mutating state.
pub trait RuntimeValidation {
    /// Reject invalid identifiers, local image keys, resource limits or env/argv.
    /// E2B profiles also require a numeric envd version of at least 0.1.0;
    /// E2B launch specs require matching resources and a private bootstrap token.
    /// Errors leave the input unchanged. This does not authenticate an image.
    fn validate(&self) -> Result<()>;
}

/// Concurrent backend operations bound to the durable owner supplied at open.
/// Missing means confirmed absence; unavailable IPC must return an error.
/// Calls for different sandbox IDs may overlap. Never reuse an ID or replay a launch.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait Runtime: Send + Sync {
    /// Check actual host capabilities before admission. Failure prevents service open.
    async fn preflight(&self) -> Result<()>;
    /// Launch once after durable admission; success confirms readiness and enforcement.
    /// Return CreateError with confirmed cleanup to release capacity; all other
    /// failures retain ownership/reservation until deletion is confirmed.
    async fn create(&self, spec: &RuntimeSpec) -> Result<()>;
    /// Authenticate the current instance and observe live state, never cached liveness.
    async fn inspect(&self, id: &str) -> Result<RuntimeState>;
    /// Return acknowledged live state; errors may follow an applied pause.
    async fn pause(&self, id: &str) -> Result<RuntimeState>;
    /// Confirm Running only after readiness/enforcement; errors may follow resume.
    async fn resume(&self, id: &str) -> Result<RuntimeState>;
    /// Fence late launch and tear down the owned instance. The daemon also
    /// requires inspect to confirm Missing before releasing its reservation.
    async fn delete(&self, id: &str) -> Result<()>;
    /// Resolve a live Running instance's authenticated HTTP loopback publication.
    /// Native supports only execd/egress or envd ports; unsupported ports error.
    async fn endpoint(&self, id: &str, port: u16) -> Result<String>;
    /// Read a trusted local E2B template key before admission, without launching.
    /// Unsupported backends must return an explicit error, never a fabricated profile.
    async fn e2b_template(&self, image: &str) -> Result<E2bTemplate>;
}

/// Construction and HTTP exposure of one exclusively locked node registry.
#[allow(async_fn_in_trait)]
pub trait DaemonService: Sized {
    /// Open private state under an exclusive lock; the factory receives its durable
    /// owner before migration and preflight. Reject keys below 32 bytes, zero
    /// capacities, invalid authority and timeout limits outside 60 seconds..365 days.
    /// Errors release the lock but may leave durable migration/owner records.
    /// Success schedules bounded reconciliation; retained records still hold capacity.
    async fn open(
        config: Config,
        factory: impl FnOnce(String) -> Result<Arc<dyn Runtime>>,
    ) -> Result<Arc<Self>>;
    /// Build authenticated OpenSandbox/E2B routes sharing this owner's lifecycle
    /// locks and proxy budget. The router retains the owner and its state lock.
    /// Serve behind TLS on untrusted networks; unsupported features fail explicitly.
    fn router(self: Arc<Self>) -> axum::Router;
    /// Spawn one bounded TTL/deletion retry loop per owner. Start once after open;
    /// abort and await its handle at shutdown. Dropping a handle does not stop it.
    /// Idle work holds a weak owner; in-flight work can retain the state lock.
    fn start_maintenance(self: &Arc<Self>) -> tokio::task::JoinHandle<()>;
}

/// Native VM owner; construction errors explicitly on hosts other than Linux x86-64.
#[allow(async_fn_in_trait)]
pub trait NativeRuntimeService: Sized {
    /// Construct only inside DaemonService::open's locked factory. Validate trusted
    /// absolute paths and durable owner; reject occupied unclaimed registries.
    /// Failure may retain an ownership marker; never erase state to retry.
    fn new(config: NativeRuntimeConfig) -> Result<Self>;
    /// Consume configuration before handing the owner to the daemon. The optional
    /// socket selects a live, same-user pool; no reconnection or RAM recovery is implied.
    fn with_memory_pool(self, socket: Option<PathBuf>) -> Self;
    /// Hidden executable reentry for a prepared sandbox, after VM process preparation.
    /// Dispatch pVisor VM reentry before Tokio/argument parsing; this entrypoint
    /// verifies the pre-exec cgroup binding and never moves existing memory charges.
    /// Require the existing identity-bound cgroup and exclusive supervisor ownership.
    /// Run until VM teardown; errors do not establish absence or release capacity.
    async fn run_supervisor(sandbox_dir: &Path) -> Result<()>;
}

/// Detached pool process startup and foreground executable reentry.
pub trait MemoryPoolService: Sized {
    /// Start or reuse this exact configuration under a startup lock. State must
    /// already be private and owned; executable must implement memory-pool reentry.
    /// Return its authenticated socket. Never replace sockets or kill stale PIDs.
    /// Errors may leave persisted config or a live child owning RAM; preserve state.
    fn ensure_service(self, state: &Path, executable: &Path) -> Result<PathBuf>;
    /// Run the foreground component with private persisted config and positive
    /// budgets, holding the exclusive service lock. Existing sockets are fatal.
    /// Blocks while serving; service loss can terminate dependent VMs (including
    /// macOS compression). No mode reconnects or replaces live RAM ownership.
    fn run(directory: &Path) -> Result<()>;
}
