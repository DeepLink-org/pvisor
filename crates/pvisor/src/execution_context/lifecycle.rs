//! Executor reports and the single Execution completion path.
use super::ExecutionContext;
use crate::executor::RunExecutor;
use crate::runtime::execution::fail_finalization;
use crate::runtime::run::{PVisorError, ResolvedRun, RunHandle};
use crate::runtime::{
    RuntimeSupervisor,
    event::{EventSink, RunEventPublisher},
};
use crate::{AGENTCTL_VERSION, AgentCtlServer};
use pvisor_core::{ExecutionInfo, RunInvocation, RunStatus};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::runtime::ExecutionTeardown;
use crate::util::unix_now_ms;
use pvisor_core::{
    ArtifactRef, ExecutionId, ExecutorObservations, ProcessOutput, RunFailure, RunFailureKind,
    RunResult, RunSpec, RunState,
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

/// Backend report. Identity and terminal publication belong to ExecutionContext.
/// No Default: every backend must explicitly report its observations.
pub struct ExecutorOutput {
    pub state: RunState,
    pub exit_code: Option<i32>,
    pub failure: Option<RunFailure>,
    pub output: ProcessOutput,
    pub value: Option<serde_json::Value>,
    pub metrics: BTreeMap<String, f64>,
    pub artifacts: Vec<ArtifactRef>,
    pub event_stream_ref: Option<String>,
    pub warnings: Vec<String>,
    pub executor_observations: ExecutorObservations,
}
impl ExecutorOutput {
    fn into_result(
        self,
        spec: &RunSpec,
        execution_id: &ExecutionId,
        started_at_unix_ms: u64,
    ) -> RunResult {
        RunResult {
            run_id: spec.run_id.clone(),
            execution_id: execution_id.clone(),
            state: self.state,
            started_at_unix_ms,
            finished_at_unix_ms: unix_now_ms(),
            exit_code: self.exit_code,
            failure: self.failure,
            completion_error: None,
            output: self.output,
            value: self.value,
            metrics: self.metrics,
            artifacts: self.artifacts,
            event_stream_ref: self.event_stream_ref,
            warnings: self.warnings,
            executor_observations: self.executor_observations,
        }
    }
}
impl From<RunResult> for ExecutorOutput {
    fn from(result: RunResult) -> Self {
        Self {
            state: result.state,
            exit_code: result.exit_code,
            failure: result.failure,
            output: result.output,
            value: result.value,
            metrics: result.metrics,
            artifacts: result.artifacts,
            event_stream_ref: result.event_stream_ref,
            warnings: result.warnings,
            executor_observations: result.executor_observations,
        }
    }
}
pub(crate) enum SessionEnd {
    Exited(std::io::Result<std::process::ExitStatus>),
    Cancelled,
    Deadline,
}

pub(crate) fn exit_outcome(
    status: std::process::ExitStatus,
) -> (RunState, Option<i32>, Option<RunFailure>) {
    if status.success() {
        return (RunState::Completed, status.code(), None);
    }
    (
        RunState::Failed,
        status.code(),
        Some(RunFailure {
            kind: RunFailureKind::ProcessExit,
            message: format!("workload exited with {status}"),
            retryable: false,
        }),
    )
}

impl ExecutionContext {
    pub(crate) async fn start(
        runtime: &RuntimeSupervisor,
        event_sink: Arc<dyn EventSink>,
        resolved: ResolvedRun,
        execution_id: ExecutionId,
    ) -> Result<(RunHandle, Arc<crate::runtime::instance_control::Owner>), PVisorError> {
        let mut startup = crate::startup_profile::Span::start("session.start");
        let ResolvedRun {
            preparation,
            mut spec,
            executor,
            descriptor,
            vm_network_executor,
            operation,
            requested_operation,
            network_policy,
        } = resolved;
        #[cfg(feature = "gateway")]
        runtime
            .validate_model_wait(descriptor.kind)
            .map_err(PVisorError::Prepare)?;
        crate::util::startup_mark_run("session.begin", spec.run_id.as_str());
        let cancellation = CancellationToken::new();
        let vm_control = crate::executor::vm::control::VmControl::new(cancellation.clone());
        let agentctl_server = {
            let run_id = spec.run_id.clone();
            let execution_id = execution_id.clone();
            tokio::task::spawn_blocking(move || AgentCtlServer::start(&run_id, &execution_id))
                .await
                .map_err(|error| PVisorError::AgentCtl(error.into()))?
                .map_err(PVisorError::AgentCtl)?
        };
        crate::util::startup_mark_run("session.agentctl_ready", spec.run_id.as_str());
        let agentctl = agentctl_server.control();
        let safe_profile_requested = spec.runtime.profile == pvisor_core::RunProfile::Safe;
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.env.extend(agentctl_server.environment());
        spec.metadata.insert(
            "pvisor.agentctl".into(),
            json!({
                "version": AGENTCTL_VERSION, "transport": "unix", "endpoint": agentctl.endpoint(),
            }),
        );

        let run_id = spec.run_id.clone();
        let now = unix_now_ms();
        let initial = RunStatus {
            run_id: run_id.clone(),
            state: RunState::Created,
            execution: ExecutionInfo {
                execution_id: execution_id.clone(),
                number: 0,
                executor: descriptor.clone(),
                started_at_unix_ms: None,
                finished_at_unix_ms: None,
            },
            updated_at_unix_ms: now,
            message: None,
        };
        let (live_tx, _) = broadcast::channel(256);
        let events = RunEventPublisher::new(
            run_id.clone(),
            execution_id.clone(),
            "pvisor",
            event_sink,
            live_tx,
        );
        let execution = crate::Execution::new(
            initial,
            vm_control.clone(),
            operation.clone(),
            events.clone(),
            cancellation.clone(),
        )
        .map_err(PVisorError::Prepare)?;
        #[cfg(feature = "gateway")]
        let (runtime, wait_coordinator) = runtime.bind_model_wait(execution.clone());
        #[cfg(feature = "gateway")]
        let execution = {
            let mut execution = execution;
            execution.wait_coordinator = wait_coordinator.clone();
            execution
        };
        let mut context = ExecutionContext {
            vm_inputs: preparation.vm_inputs.clone(),
            spec: Arc::new(spec),
            created_at_unix_ms: now,
            execution_id: execution_id.clone(),
            vm_control: vm_control.clone(),
            execution,
            events: events.clone(),
            agentctl: agentctl.clone(),
            attachments: Default::default(),
            drivers: None,
            server: Some(agentctl_server),
            output_observer: preparation.output_observer.clone(),
            network_policy,
        };
        crate::util::startup_mark_run("session.storage_begin", context.spec.run_id.as_str());
        let execution = context.execution.clone();
        let prepared = (|| {
            context.drivers = runtime.prepare(
                Arc::make_mut(&mut context.spec),
                &preparation,
                &[],
                vm_network_executor,
                &execution_id,
            )?;
            context.attachments = context
                .drivers
                .as_ref()
                .map(|session| session.attachments())
                .unwrap_or_default();
            Ok::<_, anyhow::Error>(())
        })();
        if let Err(error) = prepared {
            context.abort_startup(&error, safe_profile_requested).await;
            return Err(PVisorError::Prepare(error));
        }
        crate::util::startup_mark_run("session.storage_ready", context.spec.run_id.as_str());
        let record = context.drivers.as_ref().map(|session| session.record());
        if let Err(error) = context.execution.retain(record.as_ref()).await {
            context.abort_startup(&error, safe_profile_requested).await;
            return Err(PVisorError::Prepare(error));
        }
        let checkpoint_record = context
            .drivers
            .as_ref()
            .and_then(|session| session.checkpoint_record());
        let (filesystem, network) = context
            .drivers
            .as_ref()
            .map(|session| session.control_metrics())
            .unwrap_or_default();
        let owner = match crate::runtime::instance_control::register(
            execution.clone(),
            record.clone(),
            preparation.live_control.clone(),
            filesystem,
            network,
        ) {
            Ok(owner) => owner,
            Err(error) => {
                context.abort_startup(&error, safe_profile_requested).await;
                return Err(PVisorError::Prepare(error));
            }
        };
        let creation = async {
            events
                .publish(
                    "run.created",
                    "runtime",
                    json!({
                        "agent": context.spec.agent,
                        "task_id": context.spec.task_id,
                        "executor": descriptor,
                        "policy_mode": context.spec.runtime.policy_mode,
                        "capture_session": context.drivers.as_ref().map(|session| session.root_session()),
                        "agentctl_version": AGENTCTL_VERSION,
                    }),
                )
                .await?;
            events.begin_execution(&requested_operation, &operation, &descriptor.name).await?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = creation {
            context
                .abort_startup(
                    &anyhow::anyhow!("event sink rejected run creation: {error:#}"),
                    safe_profile_requested,
                )
                .await;
            return Err(PVisorError::EventSink(error));
        }
        crate::util::startup_mark_run("session.events_ready", context.spec.run_id.as_str());
        #[cfg(feature = "gateway")]
        if wait_coordinator.is_some() {
            execution.monitor_wait_termination();
        }
        let controls = owner.owner.clone();
        let join = tokio::spawn(async move {
            let _owner = owner;
            context
                .complete(executor, operation, safe_profile_requested)
                .await
        });

        startup.success(None);
        Ok((
            RunHandle {
                run_id,
                execution_id,
                execution,
                events,
                agentctl,
                record,
                checkpoint_record,
                join,
                instance_control: None,
            },
            controls,
        ))
    }
    /// All backends use the same exit/cancel/deadline ordering and cancellation transition.
    pub(crate) async fn wait_child(
        &self,
        child: &mut tokio::process::Child,
        timeout_ms: Option<u64>,
    ) -> SessionEnd {
        self.wait_exit(child.wait(), timeout_ms).await
    }

    /// Preserve exit/cancel/deadline ordering for an executor that observes the
    /// exited child before reaping it.
    pub(crate) async fn wait_exit(
        &self,
        exit: impl std::future::Future<Output = std::io::Result<std::process::ExitStatus>>,
        timeout_ms: Option<u64>,
    ) -> SessionEnd {
        let deadline = async {
            match timeout_ms {
                Some(ms) => tokio::time::sleep(std::time::Duration::from_millis(ms)).await,
                None => std::future::pending().await,
            }
        };
        let cancellation = self.execution.cancellation();
        let end = tokio::select! {
            biased;
            status = exit => SessionEnd::Exited(status),
            _ = cancellation.cancelled() => SessionEnd::Cancelled,
            _ = deadline => SessionEnd::Deadline,
        };
        if matches!(end, SessionEnd::Cancelled) {
            self.execution.cancelling().await;
        }
        end
    }
    pub(crate) async fn complete(
        mut self,
        executor: Arc<dyn RunExecutor>,
        operation: pvisor_core::operation::Operation,
        safe_profile_requested: bool,
    ) -> RunResult {
        let _server = self.server.take();
        if let Some(policy) = &self.attachments.filesystem {
            policy.arm();
        }
        // Keep the Run-scoped endpoint alive until executor finalization finishes.
        let invoked = !self.execution.cancellation().is_cancelled();
        let report = if invoked {
            let report = executor.execute(&self).await;
            if let Some(observer) = &self.output_observer {
                observer(&report);
            }
            report
        } else {
            ExecutorOutput {
                state: RunState::Cancelled,
                exit_code: None,
                failure: None,
                output: Default::default(),
                value: None,
                metrics: Default::default(),
                artifacts: Vec::new(),
                event_stream_ref: None,
                warnings: Vec::new(),
                executor_observations: Default::default(),
            }
        };
        let started = self
            .execution
            .status()
            .execution
            .started_at_unix_ms
            .unwrap_or(self.created_at_unix_ms);
        let mut result = report.into_result(self.spec(), self.execution_id(), started);
        let teardown = self.finalize(&mut result, invoked).await;
        self.commit(&mut result, teardown, &operation, safe_profile_requested)
            .await;
        self.execution
            .finish(
                result.state,
                result.failure.as_ref().map(|f| f.message.clone()),
                result.finished_at_unix_ms,
            )
            .await;
        result
    }

    async fn finalize(
        &mut self,
        result: &mut RunResult,
        invoked: bool,
    ) -> Option<ExecutionTeardown> {
        self.execution.normalize_result(self.spec(), result);
        crate::util::startup_mark_run("execution.teardown_begin", self.spec().run_id.as_str());
        let teardown = self.drivers.take().map(|session| {
            session.teardown(
                result.exit_code,
                invoked,
                result.state == RunState::Hibernated,
            )
        });
        crate::util::startup_mark_run("execution.teardown_ready", self.spec().run_id.as_str());
        if let Some(error) = teardown
            .as_ref()
            .and_then(|teardown| teardown.error_message())
        {
            fail_finalization(result, format!("execution teardown failed: {error}"));
        }
        teardown
    }

    async fn commit(
        &self,
        result: &mut RunResult,
        mut teardown: Option<ExecutionTeardown>,
        operation: &pvisor_core::operation::Operation,
        safe_profile_requested: bool,
    ) {
        let durable = teardown.is_some();
        result.finished_at_unix_ms = unix_now_ms();
        persist_result(
            &mut teardown,
            result,
            &self.agentctl,
            safe_profile_requested,
        )
        .await;
        let warnings_before_observation = result.warnings.len();
        let run_observation = match crate::runtime::operation::observe(
            operation,
            result,
            teardown
                .as_ref()
                .and_then(|teardown| teardown.run_record().network_interception_metrics.as_ref()),
            teardown
                .as_ref()
                .and_then(|teardown| teardown.run_record().filesystem_observation.as_ref()),
        ) {
            Ok(observation) => observation,
            Err(error) => {
                let known_effects = json!({
                    "result": result,
                    "filesystem": teardown.as_ref().and_then(|t| t.run_record().filesystem_observation.as_ref()),
                });
                fail_finalization(result, format!("invalid Run observation: {error:#}"));
                pvisor_core::operation::OperationObservation {
                    outcome: pvisor_core::operation::Outcome::Error {
                        failure: pvisor_core::operation::Failure::Unknown {
                            reason: error.to_string(),
                            known_effects,
                        },
                    },
                    rules: operation
                        .rules
                        .iter()
                        .map(|rule| (rule.id.clone(), Default::default()))
                        .collect(),
                    filesystem: None,
                }
            }
        };
        if let Some(filesystem) = &run_observation.filesystem
            && let Err(error) = self
                .events()
                .publish("filesystem.observed", "filesystem", json!(filesystem))
                .await
        {
            result
                .warnings
                .push(format!("filesystem observation audit gap: {error:#}"));
        }
        if let Some(network) = teardown
            .as_ref()
            .and_then(|t| t.run_record().network_interception_metrics.as_ref())
            && let Err(error) = self
                .events()
                .publish("network.observed", "network", json!(network))
                .await
        {
            result
                .warnings
                .push(format!("network observation audit gap: {error:#}"));
        }
        if result.warnings.len() != warnings_before_observation {
            persist_result(
                &mut teardown,
                result,
                &self.agentctl,
                safe_profile_requested,
            )
            .await;
        }
        if durable {
            let Some(mut owned) = teardown.take() else {
                // A failed persistence task lost finalization ownership.
                return;
            };
            let saved = result.clone();
            match tokio::task::spawn_blocking(move || {
                let committed = owned.complete(&saved);
                (owned, committed)
            })
            .await
            {
                Ok((owned, Ok(()))) => teardown = Some(owned),
                Ok((_owned, Err(error))) => {
                    // Publication may have succeeded before a failed fsync. Never
                    // rewrite the Bundle/result or announce an unconfirmed commit.
                    result.warnings.push(format!(
                        "{error}; reconcile retained execution; do not repeat workload"
                    ));
                    result.completion_error = Some(error);
                    return;
                }
                Err(error) => {
                    result.completion_error = Some(pvisor_core::CompletionError {
                        phase: pvisor_core::CompletionPhase::CommitTask,
                        publication: pvisor_core::CompletionPublication::Unknown,
                        message: format!("completion commit task failed: {error}"),
                    });
                    return;
                }
            }
        }
        if let Err(error) = self
            .events()
            .publish_fact(pvisor_core::event::Fact::Completed {
                run_id: operation.run_id.clone(),
                outcome: run_observation.outcome.clone(),
                origin: result.executor_observations.origin,
            })
            .await
        {
            tracing::warn!(run_id = %result.run_id, %error, "execution completion audit gap");
            result
                .warnings
                .push(format!("execution completion audit gap: {error:#}"));
        }
        let kind = match result.state {
            RunState::Completed => "run.completed",
            RunState::Cancelled => "run.cancelled",
            RunState::Hibernated => "run.hibernated",
            _ => "run.failed",
        };
        if let Err(error) = self
            .events()
            .publish(kind, "runtime", terminal_payload(result, &run_observation))
            .await
        {
            let append_error_kind = self.events().classify_append_error(&error);
            if durable {
                tracing::warn!(run_id = %result.run_id, ?append_error_kind, %error,
                    "terminal event audit gap; completion receipt remains authoritative");
                result.warnings.push(format!(
                    "terminal event audit gap ({append_error_kind:?}); completion receipt remains authoritative: {error:#}"
                ));
                // No replacement terminal and no changes to committed Bundle bytes.
                return;
            }
            fail_finalization(result, format!("terminal event sink failed: {error:#}"));
            if append_error_kind == crate::EventAppendErrorKind::Unknown {
                result.warnings.push(
                    "terminal event append outcome is unknown; a replacement terminal event was suppressed"
                        .into(),
                );
            }
            persist_result(
                &mut teardown,
                result,
                &self.agentctl,
                safe_profile_requested,
            )
            .await;
            if append_error_kind == crate::EventAppendErrorKind::Rejected
                && let Err(error) = self
                    .events()
                    .publish(
                        "run.failed",
                        "runtime",
                        terminal_payload(result, &run_observation),
                    )
                    .await
            {
                result.warnings.push(format!(
                    "publish finalization failure event failed: {error:#}"
                ));
                persist_result(
                    &mut teardown,
                    result,
                    &self.agentctl,
                    safe_profile_requested,
                )
                .await;
            }
        }
        drop(teardown);
    }

    async fn abort_startup(&mut self, error: &anyhow::Error, safe: bool) {
        if let Some(drivers) = self.drivers.take()
            && let Err(cleanup_error) = drivers.abort_startup(
                &self.execution_id,
                self.agentctl.snapshot(),
                safe,
                format!("ExecutionContext startup failed: {error:#}"),
            )
        {
            tracing::warn!(%cleanup_error, "persist ExecutionContext startup failure");
        }
        let result = failed_output(format!("ExecutionContext startup failed: {error:#}"))
            .into_result(&self.spec, &self.execution_id, self.created_at_unix_ms);
        self.execution
            .finish(
                result.state,
                result.failure.as_ref().map(|f| f.message.clone()),
                result.finished_at_unix_ms,
            )
            .await;
    }
}

fn failed_output(message: String) -> ExecutorOutput {
    ExecutorOutput {
        state: RunState::Failed,
        exit_code: None,
        failure: Some(RunFailure {
            kind: RunFailureKind::Infrastructure,
            message,
            retryable: false,
        }),
        output: Default::default(),
        value: None,
        metrics: Default::default(),
        artifacts: Vec::new(),
        event_stream_ref: None,
        warnings: Vec::new(),
        executor_observations: Default::default(),
    }
}

fn terminal_payload(
    result: &RunResult,
    observation: &pvisor_core::operation::OperationObservation,
) -> serde_json::Value {
    json!({
        "state": result.state,
        "origin": result.executor_observations.origin,
        "exit_code": result.exit_code,
        "failure": result.failure,
        "started_at_unix_ms": result.started_at_unix_ms,
        "finished_at_unix_ms": result.finished_at_unix_ms,
        "rule_observations": observation.rules,
    })
}

async fn persist_result(
    teardown: &mut Option<ExecutionTeardown>,
    result: &mut RunResult,
    agentctl: &crate::AgentCtlControl,
    safe_profile_requested: bool,
) {
    let Some(mut owned) = teardown.take() else {
        return;
    };
    let mut saved = result.clone();
    let agentctl = agentctl.clone();
    // Bundle filesystem summaries can block on lazy FUSE lowers or storage.
    // Keep the async runtime progressing even on a one-thread Worker.
    // Retain Run ownership through the blocking capture and durable commit.
    match tokio::task::spawn_blocking(move || {
        persist_result_blocking(&mut owned, &mut saved, &agentctl, safe_profile_requested);
        (owned, saved)
    })
    .await
    {
        Ok((owned, saved)) => {
            *teardown = Some(owned);
            *result = saved;
        }
        Err(error) => fail_finalization(
            result,
            format!("Run Bundle persistence task failed: {error}"),
        ),
    }
}

fn persist_result_blocking(
    teardown: &mut ExecutionTeardown,
    result: &mut RunResult,
    agentctl: &crate::AgentCtlControl,
    safe_profile_requested: bool,
) {
    crate::util::startup_mark_run("execution.bundle_begin", result.run_id.as_str());
    if let Err(error) = teardown.persist(result, agentctl.snapshot(), safe_profile_requested) {
        fail_finalization(result, format!("{error:#}"));
        if let Err(error) = teardown.persist(result, agentctl.snapshot(), safe_profile_requested) {
            result
                .warnings
                .push(format!("persist failed Run Bundle: {error:#}"));
            if let Err(error) = crate::RunBundle::invalidate(&teardown.run_record().stage_dir()) {
                result
                    .warnings
                    .push(format!("invalidate stale Run Bundle: {error:#}"));
            }
        }
    }
    crate::util::startup_mark_run("execution.bundle_ready", result.run_id.as_str());
}

/// Observe without releasing the PID that reserves the process group's identity.
pub(crate) async fn observe_child_exit(
    child: &mut tokio::process::Child,
) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // Match Child::wait's stdin ownership, without polling its reaping future.
        drop(child.stdin.take());
        let pid = child
            .id()
            .ok_or_else(|| std::io::Error::other("child already reaped"))?;
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
        loop {
            {
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                let ret = unsafe {
                    libc::waitid(
                        libc::P_PID,
                        pid,
                        &mut info,
                        libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                    )
                };
                if ret != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                if unsafe { info.si_pid() } != 0 {
                    let status = unsafe { info.si_status() };
                    let raw = if info.si_code == libc::CLD_EXITED {
                        status << 8
                    } else {
                        status
                            | if info.si_code == libc::CLD_DUMPED {
                                0x80
                            } else {
                                0
                            }
                    };
                    return Ok(std::process::ExitStatus::from_raw(raw));
                }
            }
            signal.recv().await;
        }
    }
    #[cfg(not(unix))]
    child.wait().await
}

#[cfg(target_os = "linux")]
fn group_has_live_members(proc: &std::path::Path, group: u32) -> std::io::Result<bool> {
    fn live_and_group(stat: &str) -> std::io::Result<(bool, u32)> {
        let invalid = || std::io::Error::other("invalid process-group stat");
        let (_, fields) = stat.rsplit_once(')').ok_or_else(invalid)?;
        let mut fields = fields.split_whitespace();
        let state = fields.next().ok_or_else(invalid)?;
        fields.next().ok_or_else(invalid)?; // parent PID
        let group = fields
            .next()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;
        // Fields 6..19 precede num_threads (field 20).
        let threads = fields
            .nth(14)
            .ok_or_else(invalid)?
            .parse::<u32>()
            .map_err(|_| invalid())?;
        if threads == 0 {
            return Err(invalid());
        }
        // A zombie thread-group leader can still have live worker threads.
        Ok((!matches!(state, "Z" | "X") || threads > 1, group))
    }
    // A running leader itself needs the configured grace; avoid a full scan.
    let leader = std::fs::read_to_string(proc.join(group.to_string()).join("stat"))?;
    let (live, _) = live_and_group(&leader)?;
    if live {
        return Ok(true);
    }
    for entry in std::fs::read_dir(proc)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
            .is_none()
        {
            continue;
        }
        let stat = match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let (live, member_group) = live_and_group(&stat)?;
        if member_group == group && live {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "macos")]
fn group_has_live_members(group: u32) -> std::io::Result<bool> {
    #[link(name = "proc")]
    unsafe extern "C" {}
    // SAFETY: a null buffer with zero size requests the required PID count.
    let count = unsafe { libc::proc_listpgrppids(group as i32, std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Err(std::io::Error::other("cannot enumerate process group"));
    }
    let mut pids = vec![0i32; count as usize + 16];
    let size = std::mem::size_of_val(pids.as_slice()) as i32;
    // SAFETY: the initialized PID buffer is writable for exactly size bytes.
    let count = unsafe { libc::proc_listpgrppids(group as i32, pids.as_mut_ptr().cast(), size) };
    if count <= 0 || count as usize >= pids.len() {
        return Err(std::io::Error::other("incomplete process-group census"));
    }
    for pid in pids.into_iter().take(count as usize).filter(|pid| *pid > 0) {
        // SAFETY: proc_bsdinfo contains only integer and byte-array fields.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as i32;
        // SAFETY: info is aligned and writable for size bytes; libproc retains no pointer.
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        if read != size {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                continue;
            }
            return Err(error);
        }
        if info.pbi_pgid == group && info.pbi_status != libc::SZOMB {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) async fn terminate_process_tree(
    child: &mut tokio::process::Child,
    process_group: Option<u32>,
    grace_ms: u64,
) {
    #[cfg(unix)]
    {
        if let Some(pid) = process_group {
            if child.id() != Some(pid) {
                // Never signal a cached numeric group after its owner was reaped.
                return;
            }
            // The child is the leader of the process group configured above.
            let process_group = -(pid as i32);
            unsafe {
                libc::kill(process_group, libc::SIGTERM);
            }
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(grace_ms);
            loop {
                // Keep the leader waitable through the last group signal.
                if unsafe { libc::kill(process_group, 0) } != 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    break;
                }
                #[cfg(target_os = "linux")]
                let idle = matches!(
                    group_has_live_members(std::path::Path::new("/proc"), pid),
                    Ok(false)
                );
                #[cfg(target_os = "macos")]
                let idle = matches!(group_has_live_members(pid), Ok(false));
                #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                let idle = false;
                if idle || tokio::time::Instant::now() >= deadline {
                    // Even an idle scan is not authority to skip the final group
                    // signal: members may fork/exit during enumeration. PID stays pinned.
                    unsafe {
                        libc::kill(process_group, libc::SIGKILL);
                    }
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            let _ = child.wait().await;
            return;
        }
    }

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[cfg(all(test, unix))]
mod process_group_tests {
    use super::*;
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn zombie_only_group_does_not_consume_termination_grace() {
        let mut child = tokio::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        assert!(observe_child_exit(&mut child).await.unwrap().success());
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            terminate_process_tree(&mut child, Some(pid), 30_000),
        )
        .await
        .expect("a pinned zombie alone must not consume the 30-second grace");
        assert!(child.wait().await.unwrap().success());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn group_scan_preserves_live_members_and_propagates_unknown_state() {
        let root = tempfile::tempdir().unwrap();
        let write_stat = |pid: u32, state: &str, group: u32, threads: u32| {
            let dir = root.path().join(pid.to_string());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("stat"),
                format!(
                    "{pid} (name with ) spaces) {state} 1 {group} {}{threads}",
                    "0 ".repeat(14)
                ),
            )
            .unwrap();
        };
        write_stat(12, "Z", 12, 1);
        write_stat(13, "S", 99, 1);
        assert!(!group_has_live_members(root.path(), 12).unwrap());
        for state in ["S", "R", "D", "T"] {
            write_stat(13, state, 12, 1);
            assert!(group_has_live_members(root.path(), 12).unwrap());
        }
        write_stat(13, "Z", 12, 1);
        assert!(!group_has_live_members(root.path(), 12).unwrap());
        write_stat(13, "Z", 12, 2);
        assert!(group_has_live_members(root.path(), 12).unwrap());
        write_stat(13, "Z", 12, 1);
        write_stat(12, "Z", 12, 2);
        assert!(group_has_live_members(root.path(), 12).unwrap());
        write_stat(12, "Z", 12, 1);
        write_stat(13, "Z", 12, 0);
        assert!(group_has_live_members(root.path(), 12).is_err());
        std::fs::write(root.path().join("13/stat"), "invalid").unwrap();
        assert!(group_has_live_members(root.path(), 12).is_err());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn live_worker_survives_its_thread_group_leader_until_grace_expires() {
        let mut child = tokio::process::Command::new("python3")
            .args([
                "-c",
                r#"
import ctypes, os, signal, sys, threading, time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
def worker():
    os.write(1, b'ready')
    while True:
        time.sleep(3600)
threading.Thread(target=worker).start()
libc = ctypes.CDLL(None)
libc.syscall.argtypes = [ctypes.c_long, ctypes.c_long]
libc.syscall(int(sys.argv[1]), 0)
"#,
            ])
            .arg(libc::SYS_exit.to_string())
            .stdout(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            stdout.read_exact(&mut [0; 5]).await.unwrap();
            loop {
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
                if stat.rsplit_once(')').unwrap().1.split_whitespace().next() == Some("Z") {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("main thread did not exit while its worker remained alive");
        let live = group_has_live_members(std::path::Path::new("/proc"), pid).unwrap();
        let started = tokio::time::Instant::now();
        terminate_process_tree(&mut child, Some(pid), 100).await;
        assert!(live, "a zombie thread-group leader is not a dead process");
        assert!(started.elapsed() >= std::time::Duration::from_millis(100));
        assert_eq!(stdout.read(&mut [0]).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn group_cleanup_pins_leader_until_escalation_finishes() {
        let mut child = tokio::process::Command::new("/bin/sh")
            .args([
                "-c",
                "(trap '' TERM; printf ready; exec /bin/sleep 30) & exit 7",
            ])
            .stdout(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        stdout.read_exact(&mut [0; 5]).await.unwrap();
        assert_eq!(
            observe_child_exit(&mut child).await.unwrap().code(),
            Some(7)
        );
        {
            let cleanup = terminate_process_tree(&mut child, Some(pid), 100);
            tokio::pin!(cleanup);
            tokio::select! {
                _ = &mut cleanup => panic!("cleanup released the group identity before grace elapsed"),
                _ = tokio::time::sleep(std::time::Duration::from_millis(30)) => {}
            }
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let observed = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                )
            };
            let error = std::io::Error::last_os_error();
            cleanup.await;
            assert_eq!(
                observed, 0,
                "leader identity released during cleanup: {error}"
            );
            assert_eq!(unsafe { info.si_status() }, 7);
        }
        assert_eq!(child.wait().await.unwrap().code(), Some(7));
        assert_eq!(
            stdout.read(&mut [0]).await.unwrap(),
            0,
            "descendant survived KILL"
        );
    }

    #[tokio::test]
    async fn observing_exit_closes_stdin_and_preserves_status_until_cleanup() {
        for script in ["read line; exit 7", "read line; kill -TERM $$"] {
            let mut child = tokio::process::Command::new("/bin/sh")
                .args(["-c", script])
                .stdin(Stdio::piped())
                .process_group(0)
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let pid = child.id().unwrap();
            let status = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                observe_child_exit(&mut child),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(observe_child_exit(&mut child).await.unwrap(), status);
            assert_eq!(child.id(), Some(pid));
            terminate_process_tree(&mut child, Some(pid), 0).await;
            assert_eq!(child.wait().await.unwrap(), status);
        }
    }

    #[tokio::test]
    async fn reaped_or_mismatched_child_cannot_signal_a_cached_group() {
        let mut unrelated = tokio::process::Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let group = unrelated.id().unwrap();
        let mut child = tokio::process::Command::new("/bin/sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        terminate_process_tree(&mut child, Some(group), 0).await;
        assert!(unrelated.try_wait().unwrap().is_none());
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        terminate_process_tree(&mut child, Some(group), 0).await;
        let alive = unrelated.try_wait().unwrap().is_none();
        unrelated.kill().await.unwrap();
        unrelated.wait().await.unwrap();
        assert!(alive, "cached group killed an unrelated live child");
    }
}
