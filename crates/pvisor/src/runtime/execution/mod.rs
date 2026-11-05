//! Attempt-local execution facts and their single state projection.
//!
//! Native effects run outside the state lock. The existing VM transition/idle
//! fences still order those effects; this owner records admission and results.
use super::event::RunEventPublisher;
use crate::util::unix_now_ms;
use anyhow::{Context, ensure};
#[cfg(test)]
use pvisor_core::event::Record;
use pvisor_core::event::{Event, Fact, Position};
use pvisor_core::operation::{OperationKind, Value, VmState};
use pvisor_core::{RunFailure, RunFailureKind, RunResult, RunState, RunStatus};
use pvisor_journal::api::{Journal, JournalStore, Trace, TraceProducer};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

const MAX_PENDING: usize = 32;
const EVENT_NAME: &str = "execution.transition";

/// Bounded execution progress; positions identify committed facts, not native effects.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionProgress {
    /// Last applied fact, absent before initialization.
    pub applied: Option<Position>,
    /// Accepted controls that have not entered the native exchange.
    pub queued: usize,
    /// Controls awaiting a native result; physical transitions remain serialized.
    pub in_flight: usize,
    /// A local observation could not be committed to the history.
    pub history_gap: bool,
    /// A dispatched control failed without proof that it had no effect.
    pub outcome_unknown: bool,
}

/// One coherent observation of an Attempt and the history position that produced it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionSnapshot {
    /// Status at `progress.applied`, including any explicitly marked local history gap.
    pub status: RunStatus,
    /// Pending controls and uncertainty sampled under the same lock as `status`.
    pub progress: ExecutionProgress,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControlOrigin {
    Manual,
    ModelWait,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    Initialized {
        status: RunStatus,
    },
    Lifecycle {
        state: RunState,
        message: Option<String>,
        at: u64,
    },
    Queued {
        id: String,
        kind: OperationKind,
        origin: ControlOrigin,
    },
    Dispatched {
        id: String,
    },
    Succeeded {
        id: String,
        value: Value,
        at: u64,
    },
    Failed {
        id: String,
        message: String,
        effect_unknown: bool,
        state: Option<VmState>,
        at: u64,
    },
    WaitFailed {
        message: String,
        cancelling: bool,
        at: u64,
    },
    Finished {
        state: RunState,
        message: Option<String>,
        at: u64,
    },
}

#[derive(Clone)]
struct Operation {
    kind: OperationKind,
    dispatched: bool,
}

#[derive(Clone)]
struct State {
    status: RunStatus,
    pending: BTreeMap<String, Operation>,
    applied: Option<Position>,
    history_gap: bool,
    outcome_unknown: bool,
}

impl State {
    fn new(status: RunStatus) -> Self {
        Self {
            status,
            pending: BTreeMap::new(),
            applied: None,
            history_gap: false,
            outcome_unknown: false,
        }
    }

    fn snapshot(&self) -> ExecutionSnapshot {
        let in_flight = self
            .pending
            .values()
            .filter(|operation| operation.dispatched)
            .count();
        ExecutionSnapshot {
            status: self.status.clone(),
            progress: ExecutionProgress {
                applied: self.applied.clone(),
                queued: self.pending.len() - in_flight,
                in_flight,
                history_gap: self.history_gap,
                outcome_unknown: self.outcome_unknown,
            },
        }
    }

    // Pure transition rules, also used when replaying committed records.
    fn apply(&mut self, input: &Input) -> anyhow::Result<()> {
        if matches!(input, Input::Queued { .. } | Input::Dispatched { .. }) {
            ensure!(
                !self.history_gap && !self.outcome_unknown,
                "execution outcome requires reconciliation"
            );
            ensure!(
                matches!(self.status.state, RunState::Running | RunState::Suspended),
                "attempt is not running or suspended"
            );
        }
        match input {
            Input::Initialized { .. } => anyhow::bail!("execution is already initialized"),
            Input::Queued { id, kind, .. } => {
                ensure!(
                    self.status.attempt.executor.kind == pvisor_core::ExecutorKind::VirtualMachine,
                    "live VM controls require a VM executor"
                );
                kind.validate()?;
                ensure!(
                    matches!(
                        kind,
                        OperationKind::RunPause
                            | OperationKind::RunResume
                            | OperationKind::RunOffload { .. }
                            | OperationKind::RunCheckpoint { .. }
                            | OperationKind::RunSuspend { .. }
                    ),
                    "unsupported live VM control primitive"
                );
                ensure!(
                    self.pending.len() < MAX_PENDING,
                    "execution control queue is full"
                );
                ensure!(
                    !id.is_empty() && !self.pending.contains_key(id),
                    "duplicate execution operation"
                );
                self.pending.insert(
                    id.clone(),
                    Operation {
                        kind: kind.clone(),
                        dispatched: false,
                    },
                );
            }
            Input::Dispatched { id } => {
                let operation = self
                    .pending
                    .get_mut(id)
                    .context("execution operation is not queued")?;
                ensure!(
                    !operation.dispatched,
                    "execution operation was already dispatched"
                );
                operation.dispatched = true;
            }
            Input::Succeeded { id, value, at } => {
                let operation = self
                    .pending
                    .get(id)
                    .context("unknown execution operation result")?;
                ensure!(
                    operation.dispatched,
                    "execution operation was not dispatched"
                );
                match (&operation.kind, value) {
                    (
                        OperationKind::RunCheckpoint { .. } | OperationKind::RunSuspend { .. },
                        Value::ExecutionCheckpoint { checkpoint },
                    ) => {
                        checkpoint.validate()?;
                        ensure!(
                            checkpoint.source_run_id == self.status.run_id.as_str()
                                && checkpoint.source_attempt_id
                                    == self.status.attempt.attempt_id.as_str(),
                            "checkpoint belongs to another attempt"
                        );
                    }
                    (
                        OperationKind::RunPause
                        | OperationKind::RunResume
                        | OperationKind::RunOffload { .. },
                        Value::Vm { .. },
                    ) => {}
                    _ => anyhow::bail!("execution result does not match operation"),
                }
                if matches!(self.status.state, RunState::Running | RunState::Suspended) {
                    if matches!(operation.kind, OperationKind::RunSuspend { .. }) {
                        self.status.state = RunState::Checkpointing;
                        self.status.message =
                            Some("snapshot sealed; waiting for native termination".into());
                    } else if let Value::Vm { state, .. } = value {
                        self.status.state = if *state == VmState::Running {
                            RunState::Running
                        } else {
                            RunState::Suspended
                        };
                        self.status.message = Some(format!("VM {state:?}"));
                    }
                    self.status.updated_at_unix_ms = *at;
                }
                self.pending.remove(id);
            }
            Input::Failed {
                id,
                effect_unknown,
                state,
                at,
                ..
            } => {
                let operation = self
                    .pending
                    .remove(id)
                    .context("unknown execution operation failure")?;
                self.outcome_unknown |= operation.dispatched && *effect_unknown;
                if let Some(state) = state {
                    ensure!(
                        matches!(state, VmState::Running | VmState::Paused),
                        "invalid rejection state"
                    );
                    if matches!(self.status.state, RunState::Running | RunState::Suspended) {
                        self.status.state = if *state == VmState::Running {
                            RunState::Running
                        } else {
                            RunState::Suspended
                        };
                        self.status.updated_at_unix_ms = *at;
                        self.status.message = Some(format!("VM control rejected; VM {state:?}"));
                    }
                }
            }
            Input::Lifecycle { state, message, at } => {
                ensure!(
                    !state.is_terminal(),
                    "terminal state requires completion ownership"
                );
                ensure!(
                    matches!(
                        state,
                        RunState::Starting | RunState::Running | RunState::Cancelling
                    ),
                    "lifecycle cannot change native VM state"
                );
                // Late startup notifications remain facts, but cannot undo a
                // native pause, cancellation or terminal publication.
                let advances = match state {
                    RunState::Starting => {
                        matches!(self.status.state, RunState::Created | RunState::Starting)
                    }
                    RunState::Running => matches!(
                        self.status.state,
                        RunState::Created | RunState::Starting | RunState::Running
                    ),
                    RunState::Cancelling => !self.status.state.is_terminal(),
                    _ => false,
                };
                if advances {
                    self.status.state = *state;
                    self.status.message = message.clone();
                    self.status.updated_at_unix_ms = *at;
                    if matches!(state, RunState::Starting | RunState::Running)
                        && self.status.attempt.started_at_unix_ms.is_none()
                    {
                        self.status.attempt.started_at_unix_ms = Some(*at);
                    }
                }
            }
            Input::WaitFailed {
                message,
                cancelling,
                at,
            } => {
                if *cancelling {
                    self.apply(&Input::Lifecycle {
                        state: RunState::Cancelling,
                        message: Some(message.clone()),
                        at: *at,
                    })?;
                }
            }
            Input::Finished { state, message, at } => {
                ensure!(state.is_terminal(), "completion must be terminal");
                if !self.status.state.is_terminal() {
                    self.status.state = *state;
                    self.status.message = message.clone();
                    self.status.updated_at_unix_ms = *at;
                    self.status.attempt.finished_at_unix_ms = Some(*at);
                }
            }
        }
        Ok(())
    }
}

struct Owner {
    state: State,
    trace: Trace,
}

/// Attempt-local execution owner for status, history, cancellation and native controls.
/// Clones share one state projection and the same native transition fence. Accepted
/// controls continue after a dropped waiter while the Tokio runtime remains alive.
/// Terminal publication follows resource teardown and completion evidence; history
/// gaps or unknown native outcomes fence further controls without preventing cancellation.
#[derive(Clone)]
pub struct Execution {
    owner: Arc<Mutex<Owner>>,
    status: watch::Sender<ExecutionSnapshot>,
    vm_control: crate::executor::vm::control::VmControl,
    control_operation: pvisor_core::operation::Operation,
    events: RunEventPublisher,
    cancellation: CancellationToken,
    #[cfg(feature = "gateway")]
    pub(crate) wait_coordinator: Option<Arc<super::wait_coordinator::WaitCoordinator>>,
}

impl Execution {
    pub(crate) fn normalize_result(
        &self,
        spec: &pvisor_core::RunSpec,
        result: &mut pvisor_core::RunResult,
    ) {
        use pvisor_core::{PolicyMode, RunFailure, RunFailureKind};
        if result.state == RunState::Hibernated
            && let Err(error) = pvisor_core::operation::ExecutionSuspension::from_result(result)
        {
            fail_finalization(
                result,
                format!("invalid native suspension evidence: {error:#}"),
            );
        }
        if !result.state.is_terminal()
            || (result.state == RunState::Completed && result.failure.is_some())
        {
            fail_finalization(
                result,
                "backend returned an inconsistent terminal outcome".into(),
            );
        } else if result.state == RunState::Completed
            && result.exit_code.is_some_and(|code| code != 0)
        {
            result.state = RunState::Failed;
            result.failure = Some(RunFailure {
                kind: RunFailureKind::ProcessExit,
                message: format!("workload exited with code {}", result.exit_code.unwrap()),
                retryable: false,
            });
        }
        // Cancellation leaves installation unknown without changing its terminal meaning.
        if spec.runtime.policy_mode == PolicyMode::Enforce && result.state != RunState::Cancelled {
            let missing = result
                .executor_observations
                .enforcement
                .missing_dimensions(&spec.capabilities, &spec.runtime.resource_limits);
            if !missing.is_empty() {
                fail_finalization(
                    result,
                    format!(
                        "required controls lack executor observations: {}",
                        missing
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", "),
                    ),
                );
            }
        }
    }

    pub(crate) fn new(
        initial: RunStatus,
        vm_control: crate::executor::vm::control::VmControl,
        control_operation: pvisor_core::operation::Operation,
        events: RunEventPublisher,
        cancellation: CancellationToken,
    ) -> anyhow::Result<Self> {
        let trace = Trace::with_id(
            Journal::memory(),
            initial.run_id.as_str(),
            "pvisor-execution",
        );
        let input = Input::Initialized {
            status: initial.clone(),
        };
        let receipt = trace.journal().append(event(&trace, &initial, &input)?)?;
        let mut state = State::new(initial.clone());
        state.applied = Some(receipt.position);
        let (status, _) = watch::channel(state.snapshot());
        Ok(Self {
            owner: Arc::new(Mutex::new(Owner { state, trace })),
            status,
            vm_control,
            control_operation,
            events,
            cancellation,
            #[cfg(feature = "gateway")]
            wait_coordinator: None,
        })
    }

    /// Subscribe to coherent status and progress projections. Notifications may coalesce;
    /// read the journal for the full timeline. Local history gaps are explicit in progress.
    pub fn subscribe(&self) -> watch::Receiver<ExecutionSnapshot> {
        self.status.subscribe()
    }

    /// Read the current projection without waiting for a native control exchange.
    pub fn status(&self) -> RunStatus {
        self.status.borrow().status.clone()
    }

    /// Read the latest coherent projection without waiting for journal I/O.
    /// Status, position and uncertainty are published together after each transition.
    pub fn snapshot(&self) -> ExecutionSnapshot {
        self.status.borrow().clone()
    }

    /// Fresh Attempts only. A retained stage gets its own per-Attempt journal;
    /// prior histories are never truncated or silently reused.
    pub(crate) async fn retain(&self, record: Option<&super::RunRecord>) -> anyhow::Result<()> {
        let Some(record) = record else { return Ok(()) };
        let path = record.stage_dir().join(format!(
            "execution-{}.jsonl",
            self.status().attempt.attempt_id
        ));
        let journal = tokio::task::spawn_blocking(move || {
            let journal = Journal::open(&path)?;
            ensure!(
                journal.records()?.is_empty(),
                "execution history already exists"
            );
            Ok::<_, anyhow::Error>(journal)
        })
        .await??;
        let mut owner = self.owner.lock().await;
        ensure!(
            owner.state.status.state == RunState::Created && owner.state.pending.is_empty(),
            "execution history must be retained before startup"
        );
        let trace = Trace::with_id(
            journal,
            owner.state.status.run_id.as_str(),
            "pvisor-execution",
        );
        let input = Input::Initialized {
            status: owner.state.status.clone(),
        };
        let receipt = trace
            .journal()
            .append_async(event(&trace, &owner.state.status, &input)?)
            .await?;
        owner.state.applied = Some(receipt.position);
        owner.trace = trace;
        self.status.send_replace(owner.state.snapshot());
        Ok(())
    }

    async fn commit(&self, input: Input) -> anyhow::Result<()> {
        let execution = self.clone();
        // Journal appends already outlive a dropped waiter. Retain projection
        // ownership too, so a committed append cannot leave a live owner behind.
        tokio::spawn(async move { execution.commit_owned(input).await }).await?
    }

    async fn commit_owned(&self, input: Input) -> anyhow::Result<()> {
        let mut owner = self.owner.lock().await;
        let mut next = owner.state.clone();
        let observation = !matches!(input, Input::Queued { .. } | Input::Dispatched { .. });
        if let Err(error) = next.apply(&input) {
            owner.state.history_gap |= observation;
            self.status.send_replace(owner.state.snapshot());
            return Err(error);
        }
        let result: anyhow::Result<()> = async {
            let record = event(&owner.trace, &owner.state.status, &input)?;
            next.applied = Some(owner.trace.journal().append_async(record).await?.position);
            Ok(())
        }
        .await;
        if let Err(error) = &result {
            owner.state.history_gap |= observation
                || matches!(
                    error.downcast_ref::<pvisor_journal::api::AppendError>(),
                    Some(pvisor_journal::api::AppendError::Unknown(_))
                );
            next.history_gap = owner.state.history_gap;
        }
        // Confirmed effects and cleanup survive failed writes and dropped waiters.
        // Admissions remain unapplied unless their append succeeds.
        if result.is_ok() || observation {
            owner.state = next;
        }
        self.status.send_replace(owner.state.snapshot());
        result
    }

    async fn observe(&self, input: Input) {
        if let Err(error) = self.commit(input).await {
            tracing::warn!(%error, "execution history gap");
        }
    }

    async fn transition(&self, state: RunState, message: Option<String>) {
        if !state.is_terminal() {
            self.observe(Input::Lifecycle {
                state,
                message,
                at: crate::unix_now_ms(),
            })
            .await;
            let _ = self
                .events
                .publish(
                    "run.state_changed",
                    "runtime",
                    serde_json::json!({"state":state,"message":self.status().message}),
                )
                .await;
        }
    }

    /// Observe executor startup. A late notification cannot undo cancellation or native state.
    pub async fn starting(&self, message: impl Into<Option<String>>) {
        self.transition(RunState::Starting, message.into()).await;
    }

    /// Observe that the executor is ready. This does not resume a paused VM.
    pub async fn ready(&self) {
        self.transition(RunState::Running, None).await;
    }

    pub(crate) async fn cancelling(&self) {
        self.transition(RunState::Cancelling, Some("cancellation requested".into()))
            .await;
    }

    pub(crate) async fn finish(&self, state: RunState, message: Option<String>, at: u64) {
        self.observe(Input::Finished { state, message, at }).await;
    }

    async fn enqueue(&self, kind: OperationKind, origin: ControlOrigin) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        self.commit(Input::Queued {
            id: id.clone(),
            kind,
            origin,
        })
        .await?;
        Ok(id)
    }

    async fn mark_dispatched(&self, id: &str) -> anyhow::Result<()> {
        self.commit(Input::Dispatched { id: id.into() }).await
    }

    async fn succeeded(&self, id: &str, value: Value) -> anyhow::Result<()> {
        self.commit(Input::Succeeded {
            id: id.into(),
            value,
            at: crate::unix_now_ms(),
        })
        .await
        .context("VM control completed but execution history failed")
    }

    #[cfg(any(feature = "gateway", test))]
    async fn wait_failed(&self, error: &anyhow::Error) {
        let cancelling = {
            let owner = self.owner.lock().await;
            error
                .downcast_ref::<crate::executor::vm::control::ControlFailure>()
                .is_none()
                || owner.state.status.state != RunState::Running
                || owner.state.history_gap
                || owner.state.outcome_unknown
        };
        if cancelling {
            self.cancellation.cancel();
        }
        self.observe(Input::WaitFailed {
            message: error_message(error),
            cancelling,
            at: crate::unix_now_ms(),
        })
        .await;
    }

    async fn control_error(&self, id: &str, error: &anyhow::Error, origin: ControlOrigin) {
        self.failed(id, error).await;
        let owner = self.owner.lock().await;
        if owner.state.outcome_unknown
            || (origin == ControlOrigin::ModelWait
                && owner.state.status.state == RunState::Suspended)
        {
            self.cancellation.cancel();
        }
    }

    async fn failed(&self, id: &str, error: &anyhow::Error) {
        if !self.owner.lock().await.state.pending.contains_key(id) {
            return;
        }
        let message = error_message(error);
        use crate::executor::vm::control::ControlFailure;
        let failure = error.downcast_ref::<ControlFailure>();
        self.observe(Input::Failed {
            id: id.into(),
            message,
            effect_unknown: failure.is_none(),
            state: match failure {
                Some(ControlFailure::Rejected { state, .. }) => Some(*state),
                _ => None,
            },
            at: crate::unix_now_ms(),
        })
        .await;
    }
}

fn error_message(error: &anyhow::Error) -> String {
    let mut message = format!("{error:#}");
    let mut end = message.len().min(1024);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message
}

pub(crate) fn fail_finalization(result: &mut RunResult, message: String) {
    result.warnings.push(message.clone());
    result.state = RunState::Failed;
    result.executor_observations.origin = pvisor_core::event::Origin::Runtime;
    result.finished_at_unix_ms = unix_now_ms();
    result.failure = Some(RunFailure {
        kind: RunFailureKind::Infrastructure,
        message,
        retryable: true,
    });
}

fn event(trace: &Trace, status: &RunStatus, input: &Input) -> anyhow::Result<Event> {
    Ok(trace.event(
        vec![
            "run".into(),
            status.run_id.to_string(),
            "attempt".into(),
            status.attempt.attempt_id.to_string(),
        ],
        None,
        None,
        vec![],
        Fact::Observation {
            domain: "execution".into(),
            name: EVENT_NAME.into(),
            version: 1,
            payload: serde_json::to_value(input)?,
        },
    ))
}

/// Replay validates typed execution facts and identity; it never invokes a backend.
#[cfg(test)]
fn replay(records: &[Record]) -> anyhow::Result<State> {
    let mut state: Option<State> = None;
    for record in records {
        let Fact::Observation {
            domain,
            name,
            version: 1,
            payload,
        } = &record.event.data
        else {
            anyhow::bail!("invalid execution history record")
        };
        ensure!(
            domain == "execution" && name == EVENT_NAME,
            "invalid execution history fact"
        );
        let input: Input = serde_json::from_value(payload.clone())?;
        match (&mut state, &input) {
            (None, Input::Initialized { status }) => state = Some(State::new(status.clone())),
            (Some(state), _) => state.apply(&input)?,
            _ => anyhow::bail!("execution history is not initialized"),
        }
        let state = state.as_mut().unwrap();
        ensure!(
            record.event.trace_id == state.status.run_id.as_str()
                && record.event.scope
                    == [
                        "run",
                        state.status.run_id.as_str(),
                        "attempt",
                        state.status.attempt.attempt_id.as_str()
                    ],
            "foreign execution history record"
        );
        state.applied = Some(record.position.clone());
    }
    state.context("empty execution history")
}

#[cfg(test)]
mod tests;
mod vm;

mod job;
pub(crate) use job::JobTransition;
pub use job::{JobExecution, JobState};

mod record;
pub use record::RunRecordState;
pub(crate) use record::{project_record, record_state};

mod status;
pub use status::StatusState;
