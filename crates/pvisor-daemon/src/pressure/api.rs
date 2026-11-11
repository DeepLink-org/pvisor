//! Same-user cooperative scheduling protocol. No image or template operations.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

/// Frozen policy for already bound resident VMs; never changes their hard limits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentMemoryAccounting {
    /// Preserve full execution reservations, with optional smaller wait margins.
    #[default]
    Reserved,
    /// After successful Bind, count resident VMs only through measured parent
    /// charge. Startup, native restore and uncertain controls still reserve
    /// capacity. This permits memory overcommit, not guaranteed burst capacity.
    Measured,
}

/// Fixed startup policy; changing it requires draining this instance first.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
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
    pub resident_memory_accounting: ResidentMemoryAccounting,
    /// Reserved-mode opt-in allowance above measured charge for Waiting/Queued VMs
    /// and their in-flight offloads, capped by each VM's full reservation.
    /// Must be positive and no larger than the node budget. None (the default)
    /// preserves full resident reservations in Reserved mode. With Some, the owner must
    /// fence all guest work at Wait and reacquire a full execution permit even
    /// for resident VMs. This is accounting headroom, not a cgroup limit or a
    /// measured UKSM credit; uncooperative/background allocations remain unsafe.
    #[serde(default)]
    pub waiting_memory_margin_bytes: Option<u64>,
    /// Existing cgroup v2 subtree containing every managed VM and its helpers.
    /// The service reads it; it never installs or changes resource limits.
    pub cgroup_root: PathBuf,
}

/// An existing ordinary OCI execution, not instructions for creating a VM.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Existing authenticated Session or ordinary-container host socket.
    pub socket: PathBuf,
    /// Exact owner identity; container launches are never labeled Sessions.
    pub target: Target,
    /// Private VM subtree below Config::cgroup_root, including all helpers.
    pub cgroup: PathBuf,
}

/// Native owner protocol selected by its exact identity, without changing startup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Target {
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
pub enum Request {
    /// Queue before ordinary OCI creation. A repeated identical ID is idempotent.
    Reserve { id: String, memory_bytes: u64 },
    /// Bind after admission and normal OCI startup; no image conversion occurs.
    Bind { id: String, binding: Binding },
    /// Assert no tool/background process can access the VM until the next permit.
    /// Also finishes the previous Ready permit; stale tokens are rejected.
    /// With waiting_memory_margin_bytes configured, relinquishes unused full
    /// execution capacity in Reserved mode. A later Acquire must reserve it
    /// again before Ready. Measured mode already released that top-up at Bind;
    /// Wait still fences guest work and makes the VM eligible for offload.
    Wait { id: String, token: Option<String> },
    /// Queue an actual tool request; recovery has priority over new VM creation.
    Acquire { id: String, token: String },
    /// Release only after externally owned teardown is independently proven.
    /// The service checks the bound cgroup is empty, never kills or deletes it.
    Release { id: String },
    /// Read current states and the most recent accounting/control error.
    Status {},
}

/// A Ready/Admitted permit is valid only when `error` is None and in this live
/// daemon instance. The cooperative
/// owner must stop sending requests on daemon loss, not create a new scheduler.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    /// Fresh service lifetime identity; clients must pin it.
    pub instance: String,
    /// Entries retained until proven teardown; finished IDs cannot be reused.
    pub entries: BTreeMap<String, Entry>,
    /// In-flight plus uncertain writeback reservations.
    pub writebacks: usize,
    /// Last valid parent cgroup charge; absent until a complete sample succeeds.
    pub measured_bytes: Option<u64>,
    /// Latest accounting error. Grants stop while measurement is unavailable.
    pub error: Option<String>,
}

/// Public state, with logical reservations rather than claimed physical savings.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    /// Full startup/restore allowance and per-VM hard-limit ceiling, including
    /// VMM/helper overhead. Reserved mode also retains it for execution. Waiting
    /// states may hold less when Config::waiting_memory_margin_bytes is set;
    /// this does not change guest capacity or the externally installed hard limit.
    pub memory_bytes: u64,
    /// Current state; only Ready authorizes tool dispatch.
    pub state: State,
    /// The single current request token, if any.
    pub token: Option<String>,
    /// Control failure retained without retry or fabricated cleanup.
    pub error: Option<String>,
}

/// Startup reservations and recovery reservations are distinct from readiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
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
    /// Request queued for a resident VM; safe to reclaim to unblock the head.
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
/// instance; all mutations must echo Snapshot::instance from that same service.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    /// Expected service instance, mandatory for mutations.
    pub instance: Option<String>,
    /// Requested transition.
    pub request: Request,
}
