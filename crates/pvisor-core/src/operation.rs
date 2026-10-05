//! Immutable Operation definition and its boundary observations.
//! Core defines inputs, policy decisions, placements and outcomes; pvisor executes them.
use crate::execution::{CapabilityDimension, EnforcementPlan};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::{BTreeMap, BTreeSet};

pub const OPERATION_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Value {
    Run {
        state: crate::execution::RunState,
        exit_code: Option<i32>,
    },
    Vm {
        state: VmState,
        memory: Option<VmMemory>,
    },
    ExecutionCheckpoint {
        checkpoint: ExecutionCheckpoint,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotRamStorage {
    Raw,
    Compressed,
}

/// Runtime compatibility is independent of repository addressing. Host boot is
/// still an exact requirement; this version does not permit cross-host migration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotCompatibility {
    pub host_boot: String,
    pub build: String,
    pub firmware: String,
    pub profile: String,
}

/// Immutable repository receipt. Neither endpoint nor credentials are task data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotTransfer {
    pub version: u32,
    pub snapshot_id: String,
    pub transfer_id: String,
}
impl SnapshotTransfer {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.version == 1, "unsupported checkpoint transfer version");
        for id in [&self.snapshot_id, &self.transfer_id] {
            ensure!(
                id.len() == 64
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "invalid checkpoint transfer identity"
            );
        }
        Ok(())
    }
}

/// A sealed full machine/environment object, distinct from live RAM offload.
/// Store is a host-local location; this record does not claim portable recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionCheckpoint {
    pub snapshot_id: String,
    pub store: std::path::PathBuf,
    pub source_run_id: String,
    pub source_attempt_id: String,
    pub created_at_unix_ms: u64,
    pub ram_storage: SnapshotRamStorage,
}

impl ExecutionCheckpoint {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.snapshot_id.len() == 64
                && self
                    .snapshot_id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid execution checkpoint identity"
        );
        ensure!(
            self.store.is_absolute()
                && self.store.as_os_str().len() <= 4096
                && !self.source_run_id.trim().is_empty()
                && self.source_run_id.len() <= 256
                && !self.source_attempt_id.trim().is_empty()
                && self.source_attempt_id.len() <= 256
                && self.created_at_unix_ms > 0,
            "incomplete execution checkpoint binding"
        );
        Ok(())
    }
}

/// Durable native termination evidence for a suspend operation. Stored in the
/// terminal RunResult's value so completion/outbox recovery cannot lose the
/// checkpoint when the separate control acknowledgement races VM exit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSuspension {
    pub request_id: String,
    pub checkpoint: ExecutionCheckpoint,
}
impl ExecutionSuspension {
    pub fn from_result(result: &crate::RunResult) -> Result<Self> {
        ensure!(
            result.state == crate::RunState::Hibernated
                && result.exit_code.is_none()
                && result.failure.is_none(),
            "hibernation requires native termination without a guest exit or failure"
        );
        let receipt: Self = serde_json::from_value(
            result
                .value
                .clone()
                .ok_or_else(|| anyhow::anyhow!("missing suspension receipt"))?,
        )?;
        receipt.checkpoint.validate()?;
        ensure!(
            !receipt.request_id.trim().is_empty()
                && receipt.request_id.len() <= 256
                && receipt.checkpoint.source_run_id == result.run_id.as_str()
                && receipt.checkpoint.source_attempt_id == result.attempt_id.as_str(),
            "suspension receipt belongs to another Run/Attempt or request"
        );
        Ok(receipt)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmState {
    Running,
    Paused,
    Offloaded,
}

/// File backing is live RAM, not a standalone VM checkpoint. Residency is a
/// best-effort mincore sample, not a guarantee that all physical pages were freed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmMemory {
    pub backing_file: std::path::PathBuf,
    pub backed_bytes: u64,
    pub resident_before_bytes: Option<u64>,
    pub resident_after_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Success { value: Value },
    Error { failure: Failure },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Failure {
    Failed {
        domain: String,
        code: String,
        effects: Json,
    },
    Denied {
        reason: String,
    },
    Unsupported {
        reason: String,
    },
    Unknown {
        reason: String,
        known_effects: Json,
    },
}
impl Outcome {
    pub fn success(value: Value) -> Self {
        Self::Success { value }
    }
    pub fn validate(&self) -> Result<()> {
        if let Self::Success {
            value: Value::ExecutionCheckpoint { checkpoint },
        } = self
        {
            checkpoint.validate()?;
        }
        if let Self::Success {
            value: Value::Run { state, .. },
        } = self
        {
            ensure!(state.is_terminal(), "run result requires a terminal state");
        }
        if let Self::Success {
            value: Value::Vm { state, memory },
        } = self
        {
            ensure!(
                (*state == VmState::Offloaded) == memory.is_some(),
                "offloaded VM requires a RAM report; pause/resume must not carry one"
            );
            if let Some(memory) = memory {
                ensure!(
                    memory.backing_file.is_absolute() && memory.backed_bytes > 0,
                    "invalid live RAM report"
                );
            }
        }
        Ok(())
    }
}

/// Trusted execution metadata is outside the execution mechanism. A binding is
/// a description of a capability; the backend must check actual authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub backend: String,
    pub resource: String,
    pub generation: u64,
    pub contract: u16,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub revision: u64,
    pub principal: String,
    pub scope: Vec<String>,
    pub policy: String,
    #[serde(deserialize_with = "unique_map")]
    pub bindings: BTreeMap<String, Binding>,
}
impl Context {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.principal.trim().is_empty() && !self.policy.trim().is_empty(),
            "context requires principal and policy"
        );
        ensure!(
            !self.scope.is_empty()
                && self.scope.len() <= 16
                && self
                    .scope
                    .iter()
                    .all(|s| !s.trim().is_empty() && s.len() <= 256),
            "invalid scope"
        );
        for (name, binding) in &self.bindings {
            ensure!(
                !name.is_empty()
                    && !binding.backend.is_empty()
                    && !binding.resource.is_empty()
                    && binding.contract == 1,
                "invalid resource binding"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "placement", rename_all = "snake_case", deny_unknown_fields)]
pub enum Placement {
    Vm { name: String },
    Overlay { name: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationDecision {
    pub id: String,
    pub dimension: CapabilityDimension,
    pub target: String,
    /// The effective access decision, such as deny, ask, read, stage or write.
    pub action: String,
    pub enforcement_plan: EnforcementPlan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", deny_unknown_fields, from = "OperationKindWire")]
pub enum OperationKind {
    #[serde(rename = "run.pause")]
    RunPause,
    #[serde(rename = "run.resume")]
    RunResume,
    #[serde(rename = "run.offload")]
    RunOffload { file: Option<std::path::PathBuf> },
    #[serde(rename = "run.checkpoint")]
    RunCheckpoint {
        request_id: String,
        ram_storage: SnapshotRamStorage,
    },
    #[serde(rename = "run.suspend")]
    /// Seal and initiate frozen termination. Reaping is separately evidenced by
    /// the terminal RunResult's Hibernated state and ExecutionSuspension value.
    RunSuspend {
        request_id: String,
        ram_storage: SnapshotRamStorage,
    },
    #[serde(rename = "run.execute")]
    RunExecute {
        program: String,
        args: Vec<String>,
        cwd: Option<String>,
    },
}
// Empty struct variants enforce unknown-field rejection; serde's internally
// tagged unit variants otherwise ignore fields even with deny_unknown_fields.
#[derive(Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
enum OperationKindWire {
    #[serde(rename = "run.pause")]
    Pause {},
    #[serde(rename = "run.resume")]
    Resume {},
    #[serde(rename = "run.offload")]
    Offload { file: Option<std::path::PathBuf> },
    #[serde(rename = "run.checkpoint")]
    Checkpoint {
        request_id: String,
        ram_storage: SnapshotRamStorage,
    },
    #[serde(rename = "run.suspend")]
    Suspend {
        request_id: String,
        ram_storage: SnapshotRamStorage,
    },
    #[serde(rename = "run.execute")]
    Execute {
        program: String,
        args: Vec<String>,
        cwd: Option<String>,
    },
}
impl From<OperationKindWire> for OperationKind {
    fn from(wire: OperationKindWire) -> Self {
        match wire {
            OperationKindWire::Pause {} => Self::RunPause,
            OperationKindWire::Resume {} => Self::RunResume,
            OperationKindWire::Offload { file } => Self::RunOffload { file },
            OperationKindWire::Checkpoint {
                request_id,
                ram_storage,
            } => Self::RunCheckpoint {
                request_id,
                ram_storage,
            },
            OperationKindWire::Suspend {
                request_id,
                ram_storage,
            } => Self::RunSuspend {
                request_id,
                ram_storage,
            },
            OperationKindWire::Execute { program, args, cwd } => {
                Self::RunExecute { program, args, cwd }
            }
        }
    }
}

impl OperationKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::RunExecute { .. } => "run.execute",
            Self::RunPause => "run.pause",
            Self::RunResume => "run.resume",
            Self::RunOffload { .. } => "run.offload",
            Self::RunCheckpoint { .. } => "run.checkpoint",
            Self::RunSuspend { .. } => "run.suspend",
        }
    }
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::RunCheckpoint { request_id, .. } | Self::RunSuspend { request_id, .. } => {
                ensure!(
                    !request_id.trim().is_empty() && request_id.len() <= 256,
                    "checkpoint request id must contain 1..256 bytes"
                );
            }
            Self::RunExecute { program, .. } => {
                ensure!(!program.trim().is_empty(), "empty operation program")
            }
            Self::RunOffload { file: Some(file) } => {
                ensure!(
                    !file.as_os_str().is_empty()
                        && file.as_os_str().len() <= 4096
                        && file.file_name().is_some(),
                    "invalid RAM backing path"
                );
            }
            _ => (),
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub version: u16,
    pub context: Context,
    pub run_id: String,
    pub kind: OperationKind,
    /// Selected placements, ordered inner-to-outer; these do not execute or grant authority.
    pub placements: Vec<Placement>,
    pub rules: Vec<OperationDecision>,
}

impl Operation {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == OPERATION_VERSION,
            "unsupported Operation version"
        );
        ensure!(!self.run_id.is_empty(), "empty Run identity");
        self.kind.validate()?;
        self.context.validate()?;
        ensure!(self.placements.len() <= 32, "too many placements");
        for placement in &self.placements {
            let (Placement::Vm { name } | Placement::Overlay { name }) = placement;
            ensure!(!name.trim().is_empty(), "empty placement name");
        }
        ensure!(self.rules.len() <= 1024, "too many Operation rules");
        let mut ids = BTreeSet::new();
        for rule in &self.rules {
            symbol(&rule.id)?;
            ensure!(ids.insert(&rule.id), "duplicate Operation rule {}", rule.id);
            ensure!(
                !rule.target.is_empty() && !rule.action.is_empty(),
                "empty Operation rule"
            );
        }
        Ok(())
    }
}

/// `None` means the boundary cannot observe this quantity. Zero means it can
/// observe it and saw no matching operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleCounters {
    pub allowed: Option<u64>,
    pub succeeded: Option<u64>,
    pub denied: Option<u64>,
    pub failed: Option<u64>,
    pub effects: Option<u64>,
    /// Mutating operations that failed after execution began may have partial effects.
    pub uncertain_effects: Option<u64>,
    pub bytes_read: Option<u64>,
    pub bytes_written: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathOperationCounters {
    pub hits: u64,
    pub allowed: u64,
    pub succeeded: u64,
    pub denied: u64,
    pub failed: u64,
    pub effects: u64,
    pub uncertain_effects: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemObservation {
    /// Mount-relative paths; only operations that reached the FUSE boundary.
    pub paths: BTreeMap<String, BTreeMap<String, PathOperationCounters>>,
    /// Exact overlay access rules matched by observed operations.
    pub rules: BTreeMap<String, PathOperationCounters>,
    /// Operations omitted after the bounded path table filled.
    pub overflow_hits: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationObservation {
    pub outcome: Outcome,
    pub rules: BTreeMap<String, RuleCounters>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<FilesystemObservation>,
}

impl OperationObservation {
    pub fn validate(&self, plan: &Operation) -> Result<()> {
        plan.validate()?;
        self.outcome.validate()?;
        if let Outcome::Success { value } = &self.outcome {
            ensure!(
                matches!(
                    (&plan.kind, value),
                    (OperationKind::RunExecute { .. }, Value::Run { .. })
                        | (
                            OperationKind::RunCheckpoint { .. } | OperationKind::RunSuspend { .. },
                            Value::ExecutionCheckpoint { .. }
                        )
                        | (
                            OperationKind::RunPause,
                            Value::Vm {
                                state: VmState::Paused,
                                ..
                            }
                        )
                        | (
                            OperationKind::RunResume,
                            Value::Vm {
                                state: VmState::Running,
                                ..
                            }
                        )
                        | (
                            OperationKind::RunOffload { .. },
                            Value::Vm {
                                state: VmState::Offloaded,
                                ..
                            }
                        )
                ),
                "outcome does not match operation primitive"
            );
            if let Value::ExecutionCheckpoint { checkpoint } = value {
                ensure!(
                    checkpoint.source_run_id == plan.run_id,
                    "checkpoint belongs to another Run"
                );
                if let OperationKind::RunCheckpoint { ram_storage, .. }
                | OperationKind::RunSuspend { ram_storage, .. } = &plan.kind
                {
                    ensure!(
                        checkpoint.ram_storage == *ram_storage,
                        "checkpoint RAM encoding mismatch"
                    );
                }
            }
        }
        let ids: BTreeSet<_> = plan.rules.iter().map(|rule| rule.id.as_str()).collect();
        ensure!(
            self.rules.keys().all(|id| ids.contains(id.as_str())),
            "observation references an unknown rule"
        );
        ensure!(
            self.rules.len() == ids.len(),
            "observation omits a Operation rule"
        );
        if let Some(filesystem) = &self.filesystem {
            ensure!(filesystem.paths.len() <= 8192, "too many observed paths");
            ensure!(
                filesystem.rules.keys().all(|id| ids.contains(id.as_str())),
                "filesystem observation references an unknown rule"
            );
            for counters in filesystem
                .paths
                .values()
                .flat_map(|operations| operations.values())
                .chain(filesystem.rules.values())
            {
                ensure!(
                    counters.allowed.saturating_add(counters.denied) == counters.hits,
                    "filesystem hit counts do not reconcile"
                );
                ensure!(
                    counters.succeeded.saturating_add(counters.failed) == counters.allowed,
                    "filesystem outcomes do not reconcile"
                );
                ensure!(
                    counters.effects <= counters.succeeded
                        && counters.uncertain_effects <= counters.failed,
                    "filesystem effect counts exceed outcomes"
                );
            }
        }
        Ok(())
    }
}
pub fn symbol(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
        "invalid symbol {name:?}"
    );
    Ok(())
}

fn unique_map<'de, D, T>(deserializer: D) -> std::result::Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Visitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an object with unique keys")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> std::result::Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, T>()? {
                if result.insert(key.clone(), value).is_some() {
                    return Err(serde::de::Error::custom(format!("duplicate key {key:?}")));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Visitor(std::marker::PhantomData))
}
