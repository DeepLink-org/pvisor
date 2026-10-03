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
        self.slots <= capacity.slots && self.memory_bytes <= capacity.memory_bytes
            && self.cpu_millis <= capacity.cpu_millis
    }
    pub fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self { slots: self.slots.checked_add(other.slots)?,
            memory_bytes: self.memory_bytes.checked_add(other.memory_bytes)?,
            cpu_millis: self.cpu_millis.checked_add(other.cpu_millis)? })
    }
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        Some(Self { slots: self.slots.checked_sub(other.slots)?,
            memory_bytes: self.memory_bytes.checked_sub(other.memory_bytes)?,
            cpu_millis: self.cpu_millis.checked_sub(other.cpu_millis)? })
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPhase { Queued, Leased, Running, Cancelling, Succeeded, Failed, Cancelled, Lost }
impl TaskPhase {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled | Self::Lost)
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
pub struct Assignment { pub spec: TaskSpec, pub lease: Lease }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollResponse {
    pub version: u32,
    /// Duration avoids trusting clock synchronization on worker hosts.
    pub lease_duration_ms: u64,
    pub assignments: Vec<Assignment>,
    pub renewed: Vec<LeaseKey>,
    pub stop: Vec<LeaseKey>,
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
