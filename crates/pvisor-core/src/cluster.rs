//! Versioned cluster contracts. Reservations describe admission budgets, never
//! proof that an executor installed resource or isolation controls.
use crate::{ExecutorKind, IsolationKind, RunResult, RunSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRegistration {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPhase {
    Queued,
    Leased,
    Running,
    Paused,
    Offloaded,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
    Lost,
}
impl TaskPhase {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Lost
        )
    }
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
    pub spec: TaskSpec,
    pub phase: TaskPhase,
    pub generation: u64,
    pub lease: Option<Lease>,
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
}
impl ControlAction {
    pub fn operation(self) -> crate::operation::OperationKind {
        match self {
            Self::Pause => crate::operation::OperationKind::RunPause,
            Self::Offload => crate::operation::OperationKind::RunOffload { file: None },
            Self::Resume => crate::operation::OperationKind::RunResume,
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
            Self::Failed { error } => anyhow::ensure!(
                !error.is_empty() && error.len() <= 8192,
                "invalid control error"
            ),
            Self::Succeeded { state, memory } => {
                let expected = match action {
                    ControlAction::Pause => crate::operation::VmState::Paused,
                    ControlAction::Offload => crate::operation::VmState::Offloaded,
                    ControlAction::Resume => crate::operation::VmState::Running,
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
            Self::Failed { .. } => current,
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
    /// Keys retained until the controller durably acknowledges completion.
    pub active: Vec<LeaseKey>,
    /// Local final admission: free capacity can be below the advertised limit.
    pub available: Resources,
    pub max_assignments: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assignment {
    pub spec: TaskSpec,
    pub lease: Lease,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Completion {
    pub key: LeaseKey,
    pub result: Option<RunResult>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRecord {
    pub registration: WorkerRegistration,
    pub seen_at_ms: u64,
    pub draining: bool,
    pub reserved: Resources,
}
