//! Versioned cluster contracts. Reservations describe admission budgets, never
//! proof that an executor installed resource or isolation controls.
use crate::{ExecutorKind, IsolationKind, RunResult, RunSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod inference;
pub use inference::*;

pub const CLUSTER_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    pub slots: u32,
    pub memory_bytes: u64,
    pub cpu_millis: u64,
}
impl Resources {
    pub fn fits(self, capacity: Self) -> bool {
        self.slots <= capacity.slots
            && self.memory_bytes <= capacity.memory_bytes
            && self.cpu_millis <= capacity.cpu_millis
    }
    pub fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            slots: self.slots.checked_add(other.slots)?,
            memory_bytes: self.memory_bytes.checked_add(other.memory_bytes)?,
            cpu_millis: self.cpu_millis.checked_add(other.cpu_millis)?,
        })
    }
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        Some(Self {
            slots: self.slots.checked_sub(other.slots)?,
            memory_bytes: self.memory_bytes.checked_sub(other.memory_bytes)?,
            cpu_millis: self.cpu_millis.checked_sub(other.cpu_millis)?,
        })
    }
    pub fn saturating_sub(self, other: Self) -> Self {
        Self {
            slots: self.slots.saturating_sub(other.slots),
            memory_bytes: self.memory_bytes.saturating_sub(other.memory_bytes),
            cpu_millis: self.cpu_millis.saturating_sub(other.cpu_millis),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionClass {
    pub executor: ExecutorKind,
    pub isolation: IsolationKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    /// Require verified trace and/or private VM writable-layer retention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain_artifacts: Option<ArtifactRetention>,
    /// Exact model availability and capture level required for this Attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewayRequirement>,
    /// Strict outer field makes older controllers reject explicit QoS requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_qos: Option<crate::CpuQosClass>,
    pub version: u32,
    /// Immutable idempotency key, scoped to this cluster.
    pub id: String,
    pub tenant: String,
    pub run: RunSpec,
    pub execution: ExecutionClass,
    pub resources: Resources,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Immutable image/layer/RAM-pool keys already resident on a worker.
    #[serde(default)]
    pub cache_keys: Vec<String>,
    /// Require durable controller-side retention of the native Run Bundle.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retain_bundle: bool,
    /// Content digest of a registered immutable VM environment template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// Explicitly continue a sealed control observation in a new Run/Attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore: Option<ExecutionRestore>,
}
impl TaskSpec {
    pub fn requires_artifacts(&self) -> bool {
        self.retain_bundle || self.retain_artifacts.is_some()
    }
    pub fn validate_artifacts(&self) -> anyhow::Result<()> {
        if let Some(retention) = &self.retain_artifacts {
            retention.validate()?;
            anyhow::ensure!(
                !retention.workspace_upper
                    || self.execution
                        == ExecutionClass {
                            executor: ExecutorKind::VirtualMachine,
                            isolation: IsolationKind::VirtualMachine,
                        },
                "writable-layer retention requires native VM isolation"
            );
        }
        Ok(())
    }
    pub fn validate_gateway(&self) -> anyhow::Result<()> {
        if let Some(requirement) = &self.gateway {
            requirement.validate()?;
            anyhow::ensure!(
                requirement.models.iter().all(|model| self
                    .run
                    .capabilities
                    .models
                    .iter()
                    .any(|pattern| crate::gateway::model_matches(pattern, model))),
                "required Gateway models must be explicitly authorized in RunSpec capabilities"
            );
        }
        Ok(())
    }
    /// Keep explicit CPU QoS visible to strict cluster parsers, including older
    /// controllers. Never send an optional nested-only requirement over the wire.
    pub fn validate_cpu_qos(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.run.runtime.cpu_qos.is_none() || self.run.runtime.cpu_qos == self.cpu_qos,
            "RunSpec CPU QoS must match explicit TaskSpec cpu_qos"
        );
        anyhow::ensure!(
            self.cpu_qos.is_none()
                || self.execution
                    == ExecutionClass {
                        executor: ExecutorKind::VirtualMachine,
                        isolation: IsolationKind::VirtualMachine,
                    },
            "CPU QoS requires VM execution"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionRestore {
    pub task_id: String,
    pub request_id: String,
}

pub const MAX_EXECUTION_FORK_BRANCHES: usize = 64;
/// Bound duplicated specifications and one controller journal commit.
pub const MAX_EXECUTION_FORK_BYTES: usize = 4 * 1024 * 1024;

/// Explicit identities for a continuation of captured CPU/process state.
/// Invocation, input, authorization and admission budgets come from the source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionForkBranch {
    pub task_id: String,
    pub run_id: crate::RunId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionForkRequest {
    pub version: u32,
    /// Idempotency key within the source task's fork history.
    pub request_id: String,
    /// A sealed checkpoint/completed suspension for fork creation, or a fresh
    /// checkpoint request ID for the live capture workflow.
    pub checkpoint_request_id: String,
    pub branches: Vec<ExecutionForkBranch>,
}

/// An immutable receipt proves that every branch was committed together.
/// Branch execution and admission are independent of this creation receipt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionForkRecord {
    pub version: u32,
    pub source_task_id: String,
    pub source_key: LeaseKey,
    pub request: ExecutionForkRequest,
    pub checkpoint: crate::operation::ExecutionCheckpoint,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LiveForkPhase {
    Capturing,
    Ready,
    Failed,
}

/// Durable capture-to-branch workflow. Ready proves creation, not execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LiveForkRecord {
    pub version: u32,
    pub source_task_id: String,
    pub source_key: LeaseKey,
    pub request: ExecutionForkRequest,
    pub phase: LiveForkPhase,
    pub fork: Option<ExecutionForkRecord>,
    pub error: Option<String>,
    pub created_at_ms: u64,
    pub completed_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRegistration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_export: Option<ArtifactExportSupport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewaySupport>,
    /// Explicit opt-in makes older controllers reject unsupported CPU reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_observation_protocol: Option<u32>,
    /// Native CPU classes this Worker can actually install. Empty preserves legacy scheduling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cpu_qos_classes: Vec<crate::CpuQosClass>,
    pub version: u32,
    pub id: String,
    /// New UUID on every worker process start; old incarnations cannot renew.
    pub incarnation: String,
    pub capacity: Resources,
    pub execution: Vec<ExecutionClass>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub cache_keys: Vec<String>,
    /// Absent on older workers; unsupported controls are rejected at admission.
    #[serde(default)]
    pub vm_control_protocol: Option<u32>,
    /// Actions supported by this worker's native VM configuration.
    #[serde(default)]
    pub vm_control_actions: Vec<ControlAction>,
    #[serde(default)]
    pub artifact_protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_support: Option<EnvironmentSupport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_restore_protocol: Option<u32>,
    /// Direct save-and-stop from paused/offloaded state, without CPU resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked_execution_suspend_protocol: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayRequirement {
    pub version: u32,
    pub level: crate::gateway::CaptureLevel,
    /// Exact model IDs; policy patterns remain separately in RunSpec capabilities.
    pub models: Vec<String>,
}
impl GatewayRequirement {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == CLUSTER_VERSION,
            "unsupported Gateway version"
        );
        anyhow::ensure!(
            !self.models.is_empty() && self.models.len() <= 64,
            "Gateway needs 1..64 models"
        );
        for (index, model) in self.models.iter().enumerate() {
            anyhow::ensure!(
                !model.trim().is_empty()
                    && model.len() <= 256
                    && !model.contains('*')
                    && !model.chars().any(char::is_control)
                    && !self.models[..index].contains(model),
                "invalid or duplicate Gateway model id"
            );
        }
        Ok(())
    }
}

/// Public capability omits upstream endpoints and credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewaySupport {
    pub version: u32,
    pub level: crate::gateway::CaptureLevel,
    pub model_patterns: Vec<String>,
}
impl GatewaySupport {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == CLUSTER_VERSION,
            "unsupported Gateway capability version"
        );
        anyhow::ensure!(
            !self.model_patterns.is_empty() && self.model_patterns.len() <= 128,
            "Gateway capability needs 1..128 model routes"
        );
        for (index, pattern) in self.model_patterns.iter().enumerate() {
            let stars = pattern.bytes().filter(|b| *b == b'*').count();
            anyhow::ensure!(
                !pattern.trim().is_empty()
                    && pattern.len() <= 256
                    && !pattern.chars().any(char::is_control)
                    && (stars == 0
                        || stars == 1 && (pattern.starts_with('*') || pattern.ends_with('*')))
                    && !self.model_patterns[..index].contains(pattern),
                "invalid or duplicate Gateway route pattern"
            );
        }
        Ok(())
    }
    pub fn satisfies(&self, requirement: &GatewayRequirement) -> bool {
        self.version == requirement.version
            && self.level == requirement.level
            && requirement.models.iter().all(|model| {
                self.model_patterns
                    .iter()
                    .any(|pattern| crate::gateway::model_matches(pattern, model))
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentSupport {
    pub version: u32,
    pub architecture: String,
}

/// A native cache revision, independent of mutable OCI tags and cache locations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentLayer {
    pub handle: String,
    pub manifest_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentTemplate {
    pub version: u32,
    pub architecture: String,
    pub base: EnvironmentLayer,
    #[serde(default)]
    pub workspace: Option<EnvironmentLayer>,
    /// Bottom to top: later toolkits take precedence in the merged root.
    #[serde(default)]
    pub toolkits: Vec<EnvironmentLayer>,
}
impl EnvironmentTemplate {
    pub fn layers(&self) -> impl DoubleEndedIterator<Item = &EnvironmentLayer> {
        std::iter::once(&self.base)
            .chain(self.workspace.iter())
            .chain(self.toolkits.iter())
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == CLUSTER_VERSION && self.toolkits.len() <= 16,
            "invalid environment version/layer count"
        );
        let platform = match self.architecture.as_str() {
            "amd64" => "linux-amd64",
            "arm64" => "linux-arm64-v8",
            _ => anyhow::bail!("unsupported environment architecture"),
        };
        let hex = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        for layer in self.layers() {
            let parts: Vec<_> = layer.handle.split(':').collect();
            anyhow::ensure!(
                parts.len() == 4
                    && parts[0] == "pvisor-v1"
                    && hex(parts[1])
                    && parts[2] == platform
                    && hex(parts[3]),
                "environment requires an immutable native cache handle for its platform"
            );
            anyhow::ensure!(
                layer
                    .manifest_digest
                    .strip_prefix("sha256:")
                    .is_some_and(hex),
                "invalid layer manifest digest"
            );
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentRecord {
    pub digest: String,
    pub template: EnvironmentTemplate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPhase {
    /// Committed graph node; predecessors must succeed before admission.
    WaitingDependencies,
    /// Identity committed; a matching full checkpoint must arrive before admission.
    WaitingCheckpoint,
    Queued,
    Leased,
    Running,
    Paused,
    Offloaded,
    /// Snapshot sealed; native termination/completion is still pending.
    Suspending,
    /// Native execution and local artifact sealing ended; delivery lease remains live.
    RetainingArtifacts,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
    Lost,
    /// Native hibernation is durably complete and all reservations released.
    Suspended,
}
impl TaskPhase {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Lost | Self::Suspended
        )
    }
}

/// Immutable, bounded DAG. Dependencies refer to task IDs inside this graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskGraphSpec {
    pub version: u32,
    pub id: String,
    pub tenant: String,
    pub nodes: Vec<TaskGraphNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskGraphNode {
    pub task: TaskSpec,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskGraphPhase {
    Queued,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskGraphNodeState {
    pub task_id: String,
    pub phase: TaskPhase,
}

/// Node outcomes are the existing fenced task records, never independent runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskGraphRecord {
    pub spec: TaskGraphSpec,
    pub phase: TaskGraphPhase,
    pub nodes: Vec<TaskGraphNodeState>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub cancel_requested_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseKey {
    pub task_id: String,
    pub worker_id: String,
    pub incarnation: String,
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub key: LeaseKey,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    /// The persisted execution identity has not yet been confirmed by its
    /// Worker after Controller restart. The phase is a historical hint until
    /// this is false; resources remain reserved and execution is not retried.
    #[serde(default)]
    pub reconciliation_pending: bool,
    /// Version of durable upload pinning used for this assignment. Legacy live
    /// assignments conservatively prevent orphan reclamation during migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_pin_protocol: Option<u32>,
    /// Native outcome and original artifact receipt survive evidence retirement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_retired_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_sample: Option<ReceivedCpuSample>,
    pub spec: TaskSpec,
    pub phase: TaskPhase,
    pub generation: u64,
    pub lease: Option<Lease>,
    /// Native terminal evidence can be known while artifact delivery is pending;
    /// only `phase.terminal()` indicates aggregate task completion.
    pub result: Option<RunResult>,
    pub error: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub controls: Vec<ControlRecord>,
    /// Current charge, distinct from the immutable full execution budget.
    /// Old journal records lacking it use the full budget while leased.
    #[serde(default)]
    pub reserved: Option<Resources>,
    #[serde(default)]
    pub admission_rejections: u64,
    #[serde(default)]
    pub last_admission_rejection: Option<AdmissionRejection>,
    #[serde(default)]
    pub artifacts: Option<BlobRef>,
    #[serde(default)]
    pub artifact_error: Option<String>,
    /// Ephemeral observation. It never changes lease expiry or admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_sample: Option<ReceivedMemorySample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptMemorySample {
    pub key: LeaseKey,
    pub sequence: u64,
    /// Monotonic worker age, measured before the report is sent.
    pub sample_age_ms: u64,
    pub sample: crate::memory::RunMemorySample,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceivedMemorySample {
    pub report: AttemptMemorySample,
    pub received_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryReportRequest {
    pub worker_id: String,
    pub incarnation: String,
    pub samples: Vec<AttemptMemorySample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryReportReceipt {
    pub accepted: Vec<LeaseKey>,
    /// Ended/expired leases are ignored, never revived by an observation.
    pub ignored: Vec<LeaseKey>,
}
impl TaskRecord {
    pub fn current_reservation(&self) -> Resources {
        if self.lease.is_none() || self.phase.terminal() {
            Resources::default()
        } else {
            self.reserved.unwrap_or(self.spec.resources)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    Pause,
    Offload,
    Resume,
    /// Seal a full owned-overlay snapshot; source execution continues.
    Checkpoint,
    /// Seal full state and stop the frozen source; completion releases resources.
    Suspend,
}
impl ControlAction {
    pub fn operation(self, request_id: &str) -> crate::operation::OperationKind {
        match self {
            Self::Pause => crate::operation::OperationKind::RunPause,
            Self::Offload => crate::operation::OperationKind::RunOffload { file: None },
            Self::Resume => crate::operation::OperationKind::RunResume,
            Self::Checkpoint => crate::operation::OperationKind::RunCheckpoint {
                request_id: request_id.into(),
                ram_storage: crate::operation::SnapshotRamStorage::Compressed,
            },
            Self::Suspend => crate::operation::OperationKind::RunSuspend {
                request_id: request_id.into(),
                ram_storage: crate::operation::SnapshotRamStorage::Compressed,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRequest {
    /// Caller-owned idempotency key within the task's immutable control history.
    pub request_id: String,
    pub action: ControlAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlCommand {
    pub key: LeaseKey,
    pub revision: u64,
    pub request: ControlRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlOutcome {
    Checkpointed {
        checkpoint: crate::operation::ExecutionCheckpoint,
    },
    Succeeded {
        state: crate::operation::VmState,
        memory: Option<crate::operation::VmMemory>,
    },
    Failed {
        error: String,
    },
}
impl ControlOutcome {
    pub fn validate(&self, action: ControlAction) -> anyhow::Result<()> {
        match self {
            Self::Checkpointed { checkpoint } => {
                anyhow::ensure!(
                    matches!(action, ControlAction::Checkpoint | ControlAction::Suspend)
                        && checkpoint.ram_storage
                            == crate::operation::SnapshotRamStorage::Compressed,
                    "checkpoint observation does not match command"
                );
                checkpoint.validate()?;
            }
            Self::Failed { error } => anyhow::ensure!(
                !error.is_empty() && error.len() <= 8192,
                "invalid control error"
            ),
            Self::Succeeded { state, memory } => {
                let expected = match action {
                    ControlAction::Pause => crate::operation::VmState::Paused,
                    ControlAction::Offload => crate::operation::VmState::Offloaded,
                    ControlAction::Resume => crate::operation::VmState::Running,
                    ControlAction::Checkpoint | ControlAction::Suspend => {
                        anyhow::bail!("checkpoint requires a sealed object observation")
                    }
                };
                anyhow::ensure!(
                    *state == expected,
                    "control observation does not match command"
                );
                crate::operation::Outcome::success(crate::operation::Value::Vm {
                    state: *state,
                    memory: memory.clone(),
                })
                .validate()?;
                if let Some(memory) = memory {
                    anyhow::ensure!(
                        memory
                            .resident_before_bytes
                            .is_none_or(|r| r <= memory.backed_bytes)
                            && memory
                                .resident_after_bytes
                                .is_none_or(|r| r <= memory.backed_bytes),
                        "invalid RAM residency sample"
                    );
                }
            }
        }
        Ok(())
    }
    /// Stop confirmed vCPUs from consuming admission CPU. Offload residency is
    /// a cache sample, so it does not authorize releasing physical RAM budgets.
    pub fn reservation(&self, full: Resources, current: Resources) -> Resources {
        match self {
            Self::Succeeded {
                state: crate::operation::VmState::Running,
                ..
            } => full,
            Self::Succeeded { .. } => Resources {
                cpu_millis: 0,
                ..full
            },
            Self::Failed { .. } | Self::Checkpointed { .. } => current,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlAcknowledgement {
    pub command: ControlCommand,
    pub outcome: ControlOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlPhase {
    Pending,
    Issued,
    Succeeded,
    Failed,
    Aborted,
}
impl ControlPhase {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Aborted)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlRecord {
    pub command: ControlCommand,
    pub phase: ControlPhase,
    pub outcome: Option<ControlOutcome>,
    pub requested_at_ms: u64,
    pub issued_at_ms: Option<u64>,
    pub completed_at_ms: Option<u64>,
    /// Extra budget charged before issuing this control. Until acknowledgement,
    /// redelivery conservatively holds it out of local availability as well.
    #[serde(default)]
    pub admission: Resources,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PollRequest {
    pub worker_id: String,
    pub incarnation: String,
    /// Complete live inventory for this incarnation, including terminal
    /// delivery until the Controller durably acknowledges it. Omitted keys may
    /// be redelivered with the same identity; partial inventories are unsafe.
    pub active: Vec<LeaseKey>,
    /// Local final admission: free capacity can be below the advertised limit.
    pub available: Resources,
    pub max_assignments: u32,
    #[serde(default)]
    pub admission: Option<AdmissionReport>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionMode {
    #[default]
    Reservations,
    LinuxPressure,
}

/// Node observations are estimates, not per-task enforcement or reclaim proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeMeasurements {
    pub system_memory_available_bytes: u64,
    pub cgroup_memory_headroom_bytes: Option<u64>,
    /// Affinity/cpuset intersected with visible ancestor CPU bandwidth limits.
    pub cpu_limit_millis: u64,
    /// Finite leaf cpu.max, required evidence for explicit CPU overcommit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_cpu_quota_millis: Option<u64>,
    pub cpu_some_avg10_bps: u16,
    pub memory_full_avg10_bps: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionBlock {
    ProbeFailed,
    StaleSample,
    CpuPressure,
    MemoryPressure,
    MemoryHeadroom,
    CpuQuota,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionReport {
    pub mode: AdmissionMode,
    #[serde(
        default = "cpu_no_overcommit",
        skip_serializing_if = "cpu_is_not_overcommitted"
    )]
    pub cpu_overcommit_bps: u16,
    /// Age uses the worker's monotonic clock, not cross-node wall clocks.
    pub sample_age_ms: u64,
    pub available: Resources,
    pub measurements: Option<NodeMeasurements>,
    pub blocked: Vec<AdmissionBlock>,
    pub error: Option<String>,
}
fn cpu_no_overcommit() -> u16 {
    10_000
}
fn cpu_is_not_overcommitted(value: &u16) -> bool {
    *value == 10_000
}

impl AdmissionReport {
    pub fn cpu_reservation_limit_millis(&self) -> Option<u64> {
        let physical = self.measurements.as_ref()?.cpu_limit_millis;
        u64::try_from(u128::from(physical) * u128::from(self.cpu_overcommit_bps) / 10_000).ok()
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (10_000..=40_000).contains(&self.cpu_overcommit_bps),
            "CPU overcommit ratio must be 10000..40000 basis points"
        );
        anyhow::ensure!(self.blocked.len() <= 6, "too many admission blocks");
        for (index, block) in self.blocked.iter().enumerate() {
            anyhow::ensure!(
                !self.blocked[..index].contains(block),
                "duplicate admission block"
            );
        }
        if let Some(error) = &self.error {
            anyhow::ensure!(
                !error.is_empty() && error.len() <= 4096,
                "invalid probe error"
            );
        }
        match self.mode {
            AdmissionMode::Reservations => anyhow::ensure!(
                self.cpu_overcommit_bps == 10_000
                    && self.measurements.is_none()
                    && self.error.is_none()
                    && self.blocked.is_empty()
                    && self.sample_age_ms == 0,
                "reservation admission cannot claim measurements"
            ),
            AdmissionMode::LinuxPressure => {
                anyhow::ensure!(
                    self.measurements.is_some() != self.error.is_some(),
                    "node report needs exactly one sample or probe error"
                );
                anyhow::ensure!(
                    !self.blocked.contains(&AdmissionBlock::ProbeFailed) || self.error.is_some(),
                    "missing probe failure evidence"
                );
                if self.error.is_some() {
                    anyhow::ensure!(
                        self.available == Resources::default()
                            && self.blocked.contains(&AdmissionBlock::ProbeFailed),
                        "probe failure must stop admission"
                    );
                }
                if let Some(m) = &self.measurements {
                    anyhow::ensure!(
                        self.cpu_overcommit_bps == 10_000
                            || m.local_cpu_quota_millis
                                .is_some_and(|quota| quota > 0 && m.cpu_limit_millis <= quota),
                        "CPU overcommit requires a finite local kernel quota"
                    );
                    let cpu_reserved_limit = self
                        .cpu_reservation_limit_millis()
                        .ok_or_else(|| anyhow::anyhow!("CPU reservation limit overflow"))?;
                    anyhow::ensure!(
                        m.cpu_some_avg10_bps <= 10_000 && m.memory_full_avg10_bps <= 10_000,
                        "invalid pressure observation"
                    );
                    anyhow::ensure!(
                        self.available.memory_bytes <= m.system_memory_available_bytes
                            && m.cgroup_memory_headroom_bytes
                                .is_none_or(|r| self.available.memory_bytes <= r)
                            && self.available.cpu_millis <= cpu_reserved_limit,
                        "availability exceeds observed limit"
                    );
                }
                for block in &self.blocked {
                    match block {
                        AdmissionBlock::ProbeFailed | AdmissionBlock::StaleSample => {
                            anyhow::ensure!(
                                self.available == Resources::default(),
                                "unavailable sample must stop admission"
                            )
                        }
                        AdmissionBlock::CpuPressure | AdmissionBlock::CpuQuota => anyhow::ensure!(
                            self.available.cpu_millis == 0,
                            "CPU admission is blocked"
                        ),
                        AdmissionBlock::MemoryPressure | AdmissionBlock::MemoryHeadroom => {
                            anyhow::ensure!(
                                self.available.memory_bytes == 0 && self.available.cpu_millis == 0,
                                "memory pressure must block new work and resume"
                            )
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assignment {
    pub spec: TaskSpec,
    pub lease: Lease,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<EnvironmentRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<crate::operation::ExecutionCheckpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollResponse {
    pub version: u32,
    /// Duration avoids trusting clock synchronization on worker hosts.
    pub lease_duration_ms: u64,
    pub assignments: Vec<Assignment>,
    pub renewed: Vec<LeaseKey>,
    pub stop: Vec<LeaseKey>,
    #[serde(default)]
    pub controls: Vec<ControlCommand>,
}

/// Restart delivery only. These keys have durable native terminal results;
/// this request never adopts or redelivers an execution whose outcome is unknown.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRequest {
    pub worker_id: String,
    pub incarnation: String,
    pub completed: Vec<LeaseKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryResponse {
    pub version: u32,
    pub lease_duration_ms: u64,
    pub renewed: Vec<LeaseKey>,
    pub stop: Vec<LeaseKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Completion {
    pub key: LeaseKey,
    pub result: Option<RunResult>,
    pub error: Option<String>,
    #[serde(default)]
    pub artifacts: Option<BlobRef>,
    #[serde(default)]
    pub artifact_error: Option<String>,
}

/// Optional handoff protocol. Older controllers return 404 and retain full charges.
pub const ARTIFACT_DELIVERY_VERSION: u32 = 1;
pub const MAX_ARTIFACT_DELIVERIES: usize = 64;

/// Admission budget for bounded chunk upload, retries and terminal evidence.
/// This is a reservation, not a measured or enforced memory/CPU limit.
pub fn artifact_delivery_reservation(current: Resources) -> Option<Resources> {
    let delivery = Resources {
        slots: 0,
        memory_bytes: 16 * 1024 * 1024,
        cpu_millis: 100,
    };
    delivery.fits(current).then_some(delivery)
}

/// Worker asserts that native teardown, mount release, durable terminal outbox
/// and the joined, durable local spool are complete before sending this request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeDone {
    pub version: u32,
    pub key: LeaseKey,
    pub result: RunResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeDoneReceipt {
    pub version: u32,
    pub key: LeaseKey,
    pub reserved: Resources,
}

/// Limits unique published object bytes/count, including concurrent and failed-cleanup reservations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactStorageLimits {
    pub version: u32,
    pub max_bytes: Option<u64>,
    pub max_objects: Option<u64>,
}
impl Default for ArtifactStorageLimits {
    fn default() -> Self {
        Self {
            version: CLUSTER_VERSION,
            max_bytes: None,
            max_objects: None,
        }
    }
}
impl ArtifactStorageLimits {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == CLUSTER_VERSION
                && self.max_bytes.is_none_or(|limit| limit > 0)
                && self.max_objects.is_none_or(|limit| limit > 0),
            "invalid artifact storage limits"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactStorageUsage {
    pub version: u32,
    pub limits: ArtifactStorageLimits,
    pub stored_bytes: u64,
    pub stored_objects: u64,
    pub reserved_bytes: u64,
    pub reserved_objects: u64,
    /// Conservative reservations retained when an unpublished temporary cannot
    /// be removed; exclusive startup recovery reclaims them.
    pub failed_reserved_bytes: u64,
    pub failed_reserved_objects: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactGcRequest {
    pub version: u32,
    /// Explicitly retire terminal evidence completed strictly before this time.
    pub retire_before_ms: Option<u64>,
    pub max_objects: u32,
}
impl ArtifactGcRequest {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == CLUSTER_VERSION && (1..=4096).contains(&self.max_objects),
            "invalid artifact GC request"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRetirement {
    pub task_id: String,
    pub generation: u64,
    pub reference: BlobRef,
    pub finished_at_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactGcPlan {
    pub version: u32,
    pub id: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub retire: Vec<ArtifactRetirement>,
    pub objects: Vec<BlobRef>,
    pub bytes: u64,
    pub blocked_by_legacy_leases: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactGcApply {
    pub version: u32,
    pub plan_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactGcReport {
    pub version: u32,
    pub plan_id: String,
    pub retired_tasks: u64,
    pub deleted_objects: u64,
    pub deleted_bytes: u64,
    pub skipped_objects: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactDownload {
    pub version: u32,
    pub id: String,
    pub expires_at_ms: u64,
    pub reference: BlobRef,
    pub manifest: ArtifactManifest,
}

pub const ARTIFACT_EXPORT_VERSION: u32 = 1;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRetention {
    pub version: u32,
    pub trace: bool,
    /// Archive of the private upper, including whiteouts; not a merged rootfs.
    pub workspace_upper: bool,
}
impl ArtifactRetention {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == ARTIFACT_EXPORT_VERSION
                && (self.trace || self.workspace_upper),
            "invalid artifact retention requirement"
        );
        Ok(())
    }
    pub fn filenames(&self) -> Vec<&'static str> {
        let mut names = vec!["run-bundle.json"];
        if self.trace {
            names.push("trace");
        }
        if self.workspace_upper {
            names.push("workspace-upper.tar");
        }
        names
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactExportSupport {
    pub version: u32,
    pub trace: bool,
    pub workspace_upper: bool,
}
impl ArtifactExportSupport {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == ARTIFACT_EXPORT_VERSION
                && (self.trace || self.workspace_upper),
            "invalid artifact export capability"
        );
        Ok(())
    }
    pub fn satisfies(&self, retention: &ArtifactRetention) -> bool {
        self.version == retention.version
            && (!retention.trace || self.trace)
            && (!retention.workspace_upper || self.workspace_upper)
    }
}

pub const ARTIFACT_CHUNK_BYTES: usize = 1024 * 1024;
pub const ARTIFACT_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// BLAKE3 over immutable bytes, lowercase hex, in the artifact namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobRef {
    pub digest: String,
    pub bytes: u64,
}
impl BlobRef {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.digest.len() == 64
                && self
                    .digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid artifact digest"
        );
        anyhow::ensure!(
            self.bytes <= ARTIFACT_CHUNK_BYTES as u64,
            "artifact chunk too large"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactFile {
    pub name: String,
    pub bytes: u64,
    /// Whole-file BLAKE3; individual chunks are independently verified.
    pub digest: String,
    pub chunks: Vec<BlobRef>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifest {
    pub version: u32,
    pub key: LeaseKey,
    pub files: Vec<ArtifactFile>,
}
impl ArtifactManifest {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == CLUSTER_VERSION && !self.files.is_empty() && self.files.len() <= 16,
            "invalid artifact manifest"
        );
        let mut names = std::collections::BTreeSet::new();
        for file in &self.files {
            anyhow::ensure!(
                !file.name.is_empty()
                    && file.name.len() <= 128
                    && !file.name.starts_with('.')
                    && file
                        .name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                    && names.insert(&file.name),
                "invalid or duplicate artifact filename"
            );
            anyhow::ensure!(
                file.bytes <= ARTIFACT_FILE_BYTES && file.chunks.len() <= 64,
                "artifact file too large"
            );
            BlobRef {
                digest: file.digest.clone(),
                bytes: 0,
            }
            .validate()?;
            let mut total = 0_u64;
            for chunk in &file.chunks {
                chunk.validate()?;
                anyhow::ensure!(chunk.bytes > 0, "empty file chunk");
                total = total
                    .checked_add(chunk.bytes)
                    .ok_or_else(|| anyhow::anyhow!("artifact size overflow"))?;
            }
            anyhow::ensure!(
                total == file.bytes,
                "artifact chunks do not match file size"
            );
        }
        Ok(())
    }
}

/// Worker assertion that this exact assignment was never started. Only a
/// still-unacknowledged leased assignment can return to the ready queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionRejection {
    pub key: LeaseKey,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRecord {
    pub registration: WorkerRegistration,
    pub seen_at_ms: u64,
    pub draining: bool,
    pub reserved: Resources,
    #[serde(default)]
    pub admission: Option<AdmissionReport>,
    /// Last report receipt. Retained reports are historical after restart.
    #[serde(default)]
    pub admission_received_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_sample: Option<ReceivedNodeMemorySample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeMemoryReportRequest {
    pub worker_id: String,
    pub incarnation: String,
    pub sequence: u64,
    pub sample_age_ms: u64,
    pub sample: crate::memory::NodeMemorySample,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceivedNodeMemorySample {
    pub report: NodeMemoryReportRequest,
    pub received_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeMemoryReportReceipt {
    pub worker_id: String,
    pub incarnation: String,
    pub sequence: u64,
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptCpuSample {
    pub key: LeaseKey,
    pub sequence: u64,
    pub sample_age_ms: u64,
    pub sample: crate::cpu::RunCpuSample,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceivedCpuSample {
    pub report: AttemptCpuSample,
    pub received_at_ms: u64,
    pub interval: Option<crate::cpu::CpuIntervalUsage>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuReportRequest {
    pub worker_id: String,
    pub incarnation: String,
    pub samples: Vec<AttemptCpuSample>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuReportReceipt {
    pub accepted: Vec<LeaseKey>,
    pub ignored: Vec<LeaseKey>,
}
