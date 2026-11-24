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
pub use crate::image_repository::PreparedCatalog;
pub use crate::runtime::NativeRuntime;

/// A change to the ordered prepared-template registry configuration.
pub enum RegistryUpdate {
    /// Append the supplied source at lowest priority. Its location must be unique.
    Add(pvisor::docker::ImageRegistry),
    /// Remove the named source from future resolution. Existing bindings stay pinned.
    Remove(String),
    /// Move an existing source to a position in the current order.
    Move {
        /// Existing source name.
        name: String,
        /// One-based priority in `1..=registry count`.
        position: usize,
    },
}

/// Prepared-template operations for a trusted, same-user catalog. No daemon service
/// is needed. Mutations serialize per template name or registry configuration;
/// queries read committed records without taking mutation locks.
pub trait PreparedCatalogService: Sized {
    /// Select a directory without accessing it. Relative paths use the process
    /// working directory. The caller must control the catalog and its ancestors.
    ///
    /// ```
    /// # fn main() -> anyhow::Result<()> {
    /// use pvisor::docker::ImageRegistry;
    /// use pvisor_daemon::api::{PreparedCatalog, PreparedCatalogService, RegistryUpdate};
    /// let root = tempfile::tempdir()?;
    /// let path = root.path().join("catalog");
    /// let catalog = PreparedCatalog::new(path.clone());
    /// assert!(catalog.list()?.is_empty());
    /// assert!(catalog.inspect("../escape").is_err());
    /// assert!(!path.exists());
    /// let source = ImageRegistry {
    ///     name: "origin".into(),
    ///     location: "https://images.example.test/v2".into(),
    ///     resolve: true,
    /// };
    /// catalog.update_registries(RegistryUpdate::Add(source))?;
    /// assert!(catalog.update_registries(RegistryUpdate::Remove("missing".into())).is_err());
    /// assert_eq!(catalog.registries()?[0].name, "origin");
    /// # Ok(())
    /// # }
    /// ```
    fn new(images: PathBuf) -> Self;

    /// Read committed template names and pinned images in sorted order. An absent
    /// directory is empty. Invalid records fail the query without making changes.
    fn list(&self) -> Result<Vec<(String, pvisor::docker::Image)>>;

    /// Read one template's local OCI metadata as a Docker inspect object with the
    /// local name in `RepoTags`. Missing or invalid names or metadata fail locally.
    fn inspect(&self, name: &str) -> Result<serde_json::Value>;

    /// Publish an existing local template to S3, GCS or an absolute `file://` directory.
    /// Returns JSON with `source`, `image_handle`, `digest` and `platform`. The bundle
    /// is immutable. The name index advances after upload if its pre-upload
    /// version still matches or already points to the same publication. Failure
    /// can leave unreferenced objects. The local template is unchanged.
    fn push(&self, name: &str, destination: &str) -> Result<serde_json::Value>;

    /// Import a template from a filesystem, S3, GCS or HTTP(S) URL, or resolve its name through configured
    /// registries when `source` is absent. Only metadata is fetched at import.
    /// Engine profiles and launch settings are validated before commit. Existing
    /// names require `refresh`. Failure preserves their committed records;
    /// directories and verified metadata may remain. Trust the selected publisher.
    fn pull(
        &self,
        name: &str,
        source: Option<&str>,
        refresh: bool,
    ) -> Result<pvisor::docker::Image>;

    /// Build and atomically install a trusted local template. The callback receives
    /// the canonical store path while the name is locked and must return a manifest
    /// rooted at that path. Do not reenter a mutation of the same name. Errors leave
    /// the previous manifest in place before commit. Prepared data may remain.
    fn prepare(
        &self,
        name: &str,
        build: impl FnOnce(PathBuf) -> Result<PreparedImage>,
    ) -> Result<()>;

    /// Read configured sources in priority order. An absent file is empty. Invalid
    /// configurations fail without accessing the network or changing the catalog.
    fn registries(&self) -> Result<Vec<pvisor::docker::ImageRegistry>>;

    /// Apply a registry change under the configuration lock and return the new order.
    /// Validation errors preserve the previous file. Already imported images keep
    /// their fixed bindings. This operation can create private configuration state.
    fn update_registries(
        &self,
        update: RegistryUpdate,
    ) -> Result<Vec<pvisor::docker::ImageRegistry>>;
}

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
    /// False only with verified absence and no cleanup pending; true retains admission.
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

/// One resource class in the native VM warm pool. No image is selected at boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WarmPoolProfile {
    /// Exact requested CPU quota in millicores.
    pub cpu_millis: u64,
    /// Exact requested hard memory limit in bytes.
    pub memory_bytes: u64,
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
    /// Return CreateError with confirmed absence and no pending cleanup to release
    /// capacity; all other failures retain ownership/reservation until deletion is confirmed.
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
    /// Open the native service and start one idle VM per distinct profile.
    /// An empty profile list leaves pooling disabled. Eligible creates consume
    /// a matching slot once; cold create remains available on a miss.
    /// Idle slots use separate, explicit capacity in addition to `config`.
    async fn open_native(
        config: Config,
        profiles: Vec<WarmPoolProfile>,
        factory: impl FnOnce(String) -> Result<NativeRuntime>,
    ) -> Result<Arc<Self>>;
    /// After draining HTTP creates, stop replenishment and reclaim idle slots.
    /// Assigned sandboxes remain live under their detached supervisors.
    /// Errors leave uncertain slots durably recorded for the next startup.
    async fn stop_warm_pool(&self) -> Result<()>;
    /// Build authenticated OpenSandbox/E2B routes sharing this owner's lifecycle
    /// locks and proxy budget. The router retains the owner and its state lock.
    /// Serve behind TLS on untrusted networks; unsupported features fail explicitly.
    fn router(self: Arc<Self>) -> axum::Router;
    /// Spawn bounded retry of pending/expired deletion and interrupted transitions.
    /// Start once after open. The task exits when the owner is dropped or storage
    /// fails closed; supervise the handle while serving. Abort and await it at
    /// shutdown; dropping it does not stop the task. Idle work holds a weak owner;
    /// accepted work can retain the state lock even after the loop is stopped.
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

/// Frozen policy for already bound resident VMs; never changes their hard limits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PressureResidentMemoryAccounting {
    /// Preserve full execution reservations, with optional smaller wait margins.
    #[default]
    Reserved,
    /// After successful Bind, count resident VMs only through measured parent
    /// charge. Startup, native restore and uncertain controls still reserve
    /// capacity. This permits memory overcommit, not guaranteed burst capacity.
    Measured,
}

/// Fixed startup policy; changing it requires draining this instance first.
///
/// Omitted accounting options retain full resident reservations:
/// ```
/// use pvisor_daemon::api::{PressureConfig, PressureResidentMemoryAccounting};
/// let config: PressureConfig = serde_json::from_value(serde_json::json!({
///     "memory_bytes": 1024, "high_bytes": 900, "low_bytes": 700,
///     "max_writebacks": 2, "cgroup_root": "/sys/fs/cgroup/example"
/// })).unwrap();
/// assert_eq!(config.resident_memory_accounting, PressureResidentMemoryAccounting::Reserved);
/// assert_eq!(config.waiting_memory_margin_bytes, None);
/// assert!(!config.recovery_reclaim);
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PressureConfig {
    /// Logical memory budget in bytes, including measured non-VM usage.
    pub memory_bytes: u64,
    /// Measured memory high watermark in bytes; must not exceed the budget.
    pub high_bytes: u64,
    /// Stop pressure reclamation at this watermark, strictly below high.
    pub low_bytes: u64,
    /// Maximum offload calls in flight, including uncertain acknowledgements.
    /// Zero disables offload while keeping the same admission/request path.
    pub max_writebacks: usize,
    /// Defaults to Reserved for compatibility. Measured releases startup
    /// top-ups after successful Bind and does not reacquire them for resident
    /// tools. Cannot be combined with waiting_memory_margin_bytes. Requires
    /// fresh cgroup samples, external hard limits and room for workload growth;
    /// it does not guarantee that all guests can fill their configured memory.
    #[serde(default)]
    pub resident_memory_accounting: PressureResidentMemoryAccounting,
    /// In Measured mode, allow a VM leaf with memory.max=max when the node
    /// has a finite hard limit. The reservation sizes admission and restore.
    #[serde(default)]
    pub allow_uncapped_vm: bool,
    /// Reserved-mode opt-in allowance above measured charge for Waiting/Queued VMs
    /// and their in-flight offloads, capped by each VM's full reservation.
    /// Must be positive and no larger than the node budget. None (the default)
    /// preserves full resident reservations in Reserved mode. With Some, the owner must
    /// fence all guest work at Wait and reacquire a full execution permit even
    /// for resident VMs. This is accounting headroom, not a cgroup limit or a
    /// measured UKSM credit; uncooperative/background allocations remain unsafe.
    #[serde(default)]
    pub waiting_memory_margin_bytes: Option<u64>,
    /// Opt-in Linux cgroup reclaim for a budget-blocked native restore in Measured
    /// mode. Defaults false. At most three requests of at most 256 MiB per permit,
    /// with no swap and no accounting credit; grants require a fresh sample.
    #[serde(default)]
    pub recovery_reclaim: bool,
    /// Existing cgroup v2 subtree containing every managed VM and its helpers.
    /// The service reads it; it never installs or changes resource limits.
    pub cgroup_root: PathBuf,
}

/// An existing ordinary OCI execution, not instructions for creating a VM.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PressureBinding {
    /// Existing authenticated Session or ordinary-container host socket.
    pub socket: PathBuf,
    /// Exact owner identity; container launches are never labeled Sessions.
    pub target: PressureTarget,
    /// Private VM subtree below PressureConfig::cgroup_root, including all helpers.
    pub cgroup: PathBuf,
}

/// Native owner protocol selected by its exact identity, without changing startup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PressureTarget {
    /// Existing Session/Execution HostVm protocol.
    Session(pvisor_core::host_protocol::AgentCtlTarget),
    /// Ordinary `pvisor run --managed-memory` launch generation.
    Container(pvisor::docker::managed::Target),
}

/// Client requests are individually bounded. Poll queued requests instead of
/// holding a tool HTTP request open; tool execution timeout starts after Ready.
/// Tokens are fresh UUIDs chosen by the owner and never reused for another tool.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum PressureRequest {
    /// Queue before ordinary OCI creation. A repeated identical ID is idempotent.
    Reserve {
        #[doc = "Owner-selected execution identity, never reused after release."]
        id: String,
        #[doc = "Full startup allowance in bytes, including helper overhead."]
        memory_bytes: u64,
    },
    /// Bind after admission and normal OCI startup; no image conversion occurs.
    Bind {
        #[doc = "Owner-selected execution identity, never reused after release."]
        id: String,
        #[doc = "Existing owner endpoint and independently bounded VM cgroup."]
        binding: PressureBinding,
    },
    /// Assert no tool/background process can access the VM until the next permit.
    /// Also finishes the previous Ready permit; stale tokens are rejected.
    /// With waiting_memory_margin_bytes configured, relinquishes unused full
    /// execution capacity in Reserved mode. A later Acquire must reserve it
    /// again before Ready. Measured mode already released that top-up at Bind;
    /// Wait still fences guest work and makes the VM eligible for offload.
    Wait {
        #[doc = "Owner-selected execution identity, never reused after release."]
        id: String,
        #[doc = "Owner-selected tool token; Wait may omit it before the first permit."]
        token: Option<String>,
    },
    /// Queue an actual tool request; recovery has priority over new VM creation.
    /// Measured-mode resident requests may pass an Offloading head if all
    /// commitments plus its recovery allowance fit. Native restores remain FIFO;
    /// Restoring/failed controls and missing measurements still fence grants.
    Acquire {
        #[doc = "Owner-selected execution identity, never reused after release."]
        id: String,
        #[doc = "Owner-selected tool token; Wait may omit it before the first permit."]
        token: String,
    },
    /// Release only after externally owned teardown is independently proven.
    /// The service checks the bound cgroup is empty, never kills or deletes it.
    Release {
        #[doc = "Owner-selected execution identity, never reused after release."]
        id: String,
    },
    /// Read current states and the most recent accounting/control error.
    Status {},
}

/// A Ready/Admitted permit is valid only when `error` is None and in this live
/// daemon instance. The cooperative
/// owner must stop sending requests on daemon loss, not create a new scheduler.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PressureSnapshot {
    /// Fresh service lifetime identity; clients must pin it.
    pub instance: String,
    /// Entries retained until proven teardown; finished IDs cannot be reused.
    pub entries: BTreeMap<String, PressureEntry>,
    /// In-flight plus uncertain writeback reservations.
    pub writebacks: usize,
    /// Last valid parent cgroup charge; absent until a complete sample succeeds.
    pub measured_bytes: Option<u64>,
    /// Most recent sampled FIFO native-recovery shortage, not a grant.
    #[serde(default)]
    pub recovery_blocked: Option<PressureRecoveryBlocked>,
    /// Latest bounded reclaim attempt, including errors and in-flight ownership.
    #[serde(default)]
    pub recovery_reclaim: Option<PressureRecoveryReclaim>,
    /// Latest accounting error. Grants stop while measurement is unavailable.
    pub error: Option<String>,
}

/// Public state, with logical reservations rather than claimed physical savings.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PressureEntry {
    /// Full startup/restore allowance and per-VM hard-limit ceiling, including
    /// VMM/helper overhead. Reserved mode also retains it for execution. Waiting
    /// states may hold less when PressureConfig::waiting_memory_margin_bytes is set;
    /// this does not change guest capacity or the externally installed hard limit.
    pub memory_bytes: u64,
    /// Current state; only Ready authorizes tool dispatch.
    pub state: PressureState,
    /// The single current request token, if any.
    pub token: Option<String>,
    /// Control failure retained without retry or fabricated cleanup.
    pub error: Option<String>,
}

/// Startup reservations and recovery reservations are distinct from readiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PressureState {
    /// Waiting for full creation capacity.
    AdmissionQueued,
    /// Owner may start its ordinary OCI VM using this reservation.
    Admitted,
    /// Identity probe in progress; no tool permit or release is allowed yet.
    Binding,
    /// Bound VM; initial startup request or a tool is owned by the caller.
    Active,
    /// Explicit safe model-wait boundary, eligible for pressure reclaim.
    Waiting,
    /// PressureRequest queued for a resident VM; safe to reclaim to unblock the head.
    Queued,
    /// Native writeback in progress; wake cannot interrupt it.
    Offloading,
    /// Acknowledged offload, no tool request pending.
    Offloaded,
    /// Acknowledged offload with a queued request.
    RestoreQueued,
    /// Full memory reserved; native resume is still in progress.
    Restoring,
    /// Resume acknowledged (or VM already resident); owner may dispatch token.
    Ready,
    /// Uncertain control; retains reservations and requires owner cleanup.
    Failed,
    /// Confirmed empty resources; ID remains fenced against reuse.
    Released,
}

/// Response wrapper pins mutations to a live service instance. Status may omit
/// instance; all mutations must echo PressureSnapshot::instance from that same service.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PressureEnvelope {
    /// Expected service instance, mandatory for mutations.
    pub instance: Option<String>,
    /// Requested transition.
    pub request: PressureRequest,
}

/// Sampled native recovery deficit. Invalid when the snapshot has an error.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PressureRecoveryBlocked {
    /// FIFO head execution identity.
    pub id: String,
    /// Existing tool permit identity; recovery never creates a replacement token.
    pub token: String,
    /// Additional measured relief required to grant the full restore allowance.
    pub deficit_bytes: u64,
    /// Whether all native controls and startup reservations have finished and
    /// no safe waiter remains available for offload.
    pub reclaim_eligible: bool,
}

/// Diagnostic only; requested bytes never count as physical memory savings.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PressureRecoveryReclaim {
    /// Execution whose recovery caused this attempt.
    pub id: String,
    /// Permit retained across retries.
    pub token: String,
    /// Attempts started for this permit (maximum three).
    pub attempts: u8,
    /// Bytes requested by the most recent attempt (maximum 256 MiB).
    pub requested_bytes: u64,
    /// True until the kernel write finishes; a slow write is never duplicated.
    pub in_flight: bool,
    /// Last write error. EAGAIN may still have reclaimed some memory.
    pub error: Option<String>,
}

/// Same-user pressure scheduler transport.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait PressureService {
    /// Serve a fresh private Linux state directory until shutdown. Refuses an
    /// undrained ledger; on failure retain resources for independent cleanup.
    async fn serve(directory: PathBuf, config: PressureConfig) -> anyhow::Result<()>;
    /// Send one bounded request. Timeout leaves effects uncertain; query status
    /// with the pinned instance before attempting further mutations.
    async fn exchange(
        socket: &std::path::Path,
        envelope: &PressureEnvelope,
    ) -> anyhow::Result<PressureSnapshot>;
}

/// Stateless transport entry point; scheduling ownership remains in the service.
pub struct PressureScheduler;
