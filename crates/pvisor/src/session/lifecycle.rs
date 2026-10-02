//! Executor reports and the single Attempt completion path.
use super::Session;
use crate::executor::RunExecutor;
use crate::runtime::run::{PVisorError, ResolvedRun, RunHandle};
use crate::runtime::{
    RuntimeSupervisor,
    event::{EventSink, RunEventPublisher},
};
use crate::{AGENTCTL_VERSION, AgentCtlServer};
use pvisor_core::{AttemptInfo, RunInvocation, RunStatus};
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;

use crate::runtime::AttemptTeardown;
use crate::util::unix_now_ms;
use pvisor_core::{
    ArtifactRef, AttemptId, ExecutorObservations, PolicyMode, ProcessOutput, RunFailure,
    RunFailureKind, RunResult, RunSpec, RunState,
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

/// Backend report. Identity and terminal publication belong to Session.
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
        attempt_id: &AttemptId,
        started_at_unix_ms: u64,
    ) -> RunResult {
        RunResult {
            run_id: spec.run_id.clone(),
            attempt_id: attempt_id.clone(),
            state: self.state,
            started_at_unix_ms,
            finished_at_unix_ms: unix_now_ms(),
            exit_code: self.exit_code,
            failure: self.failure,
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

impl Session {
    pub(crate) async fn start(
        runtime: &RuntimeSupervisor,
        event_sink: Arc<dyn EventSink>,
        resolved: ResolvedRun,
    ) -> Result<RunHandle, PVisorError> {
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
        let attempt_id = AttemptId::new(format!("attempt-{}", uuid::Uuid::new_v4()));
        let cancellation = CancellationToken::new();
        let agentctl_server = {
            let run_id = spec.run_id.clone();
            let attempt_id = attempt_id.clone();
            tokio::task::spawn_blocking(move || AgentCtlServer::start(&run_id, &attempt_id))
                .await
                .map_err(|error| PVisorError::AgentCtl(error.into()))?
                .map_err(PVisorError::AgentCtl)?
        };
        let agentctl = agentctl_server.control();
        let safe_profile_requested = spec
            .metadata
            .get("pvisor.safe")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
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
            attempt: AttemptInfo {
                attempt_id: attempt_id.clone(),
                number: 0,
                executor: descriptor.clone(),
                started_at_unix_ms: None,
                finished_at_unix_ms: None,
            },
            updated_at_unix_ms: now,
            message: None,
        };
        let (status_tx, status_rx) = watch::channel(initial);
        let (live_tx, _) = broadcast::channel(256);
        let events = RunEventPublisher::new(
            run_id.clone(),
            attempt_id.clone(),
            "pvisor",
            event_sink,
            live_tx,
        );
        let mut context = Session {
            spec: Arc::new(spec),
            created_at_unix_ms: now,
            attempt_id: attempt_id.clone(),
            cancel: cancellation.clone(),
            status: status_tx,
            events: events.clone(),
            agentctl: agentctl.clone(),
            attachments: Default::default(),
            drivers: None,
            server: Some(agentctl_server),
            network_policy,
        };
        let prepared = (|| {
            context.drivers = runtime.prepare(
                Arc::make_mut(&mut context.spec),
                &preparation,
                &[],
                vm_network_executor,
                &attempt_id,
            )?;
            context.attachments = context
                .drivers
                .as_ref()
                .map(|session| session.attachments())
                .unwrap_or_default();
            Ok::<_, anyhow::Error>(())
        })();
        if let Err(error) = prepared {
            context.abort_startup(&error, safe_profile_requested);
            return Err(PVisorError::Prepare(error));
        }
        let checkpoint_record = context
            .drivers
            .as_ref()
            .and_then(|session| session.checkpoint_record());
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
            context.abort_startup(
                &anyhow::anyhow!("event sink rejected run creation: {error:#}"),
                safe_profile_requested,
            );
            return Err(PVisorError::EventSink(error));
        }
        let join = tokio::spawn(async move {
            context
                .complete(executor, operation, safe_profile_requested)
                .await
        });

        Ok(RunHandle {
            run_id,
            attempt_id,
            status: status_rx,
            cancellation,
            events,
            agentctl,
            checkpoint_record,
            join,
        })
    }
    /// All backends use the same exit/cancel/deadline ordering and cancellation transition.
    pub(crate) async fn wait_child(
        &self,
        child: &mut tokio::process::Child,
        timeout_ms: Option<u64>,
    ) -> SessionEnd {
        let deadline = async {
            match timeout_ms {
                Some(ms) => tokio::time::sleep(std::time::Duration::from_millis(ms)).await,
                None => std::future::pending().await,
            }
        };
        let end = tokio::select! {
            biased;
            status = child.wait() => SessionEnd::Exited(status),
            _ = self.cancel.cancelled() => SessionEnd::Cancelled,
            _ = deadline => SessionEnd::Deadline,
        };
        if matches!(end, SessionEnd::Cancelled) {
            self.transition(RunState::Cancelling, Some("cancellation requested".into()))
                .await;
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
        let invoked = !self.cancel.is_cancelled();
        let report = if invoked {
            executor.execute(&self).await
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
            .status
            .borrow()
            .attempt
            .started_at_unix_ms
            .unwrap_or(self.created_at_unix_ms);
        let mut result = report.into_result(self.spec(), self.attempt_id(), started);
        let teardown = self.finalize(&mut result, invoked).await;
        self.commit(&mut result, teardown, &operation, safe_profile_requested)
            .await;
        self.publish_finished(&result);
        result
    }

    async fn finalize(&mut self, result: &mut RunResult, invoked: bool) -> Option<AttemptTeardown> {
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
        if self.spec().runtime.policy_mode == PolicyMode::Enforce
            && result.state != RunState::Cancelled
        {
            let missing = result.executor_observations.enforcement.missing_dimensions(
                &self.spec().capabilities,
                &self.spec().runtime.resource_limits,
            );
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
        let teardown = self
            .drivers
            .take()
            .map(|session| session.teardown(result.exit_code, invoked));
        if let Some(error) = teardown
            .as_ref()
            .and_then(|teardown| teardown.error_message())
        {
            fail_finalization(result, format!("attempt teardown failed: {error}"));
        }
        teardown
    }

    async fn commit(
        &self,
        result: &mut RunResult,
        mut teardown: Option<AttemptTeardown>,
        operation: &pvisor_core::operation::Operation,
        safe_profile_requested: bool,
    ) {
        result.finished_at_unix_ms = unix_now_ms();
        if let Some(teardown) = teardown.as_mut() {
            persist_result(teardown, result, &self.agentctl, safe_profile_requested);
        }
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
        if let Err(error) = self
            .events()
            .publish_fact(pvisor_core::event::Fact::Completed {
                run_id: operation.run_id.clone(),
                outcome: run_observation.outcome.clone(),
                origin: result.executor_observations.origin,
            })
            .await
        {
            result
                .warnings
                .push(format!("execution completion audit gap: {error:#}"));
        }
        if result.warnings.len() != warnings_before_observation
            && let Some(teardown) = teardown.as_mut()
        {
            persist_result(teardown, result, &self.agentctl, safe_profile_requested);
        }
        let kind = match result.state {
            RunState::Completed => "run.completed",
            RunState::Cancelled => "run.cancelled",
            _ => "run.failed",
        };
        if let Err(error) = self
            .events()
            .publish(kind, "runtime", terminal_payload(result, &run_observation))
            .await
        {
            let append_error_kind = self.events().classify_append_error(&error);
            fail_finalization(result, format!("terminal event sink failed: {error:#}"));
            if append_error_kind == crate::EventAppendErrorKind::Unknown {
                result.warnings.push(
                    "terminal event append outcome is unknown; a replacement terminal event was suppressed"
                        .into(),
                );
            }
            if let Some(teardown) = teardown.as_mut() {
                persist_result(teardown, result, &self.agentctl, safe_profile_requested);
            }
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
                if let Some(teardown) = teardown.as_mut() {
                    persist_result(teardown, result, &self.agentctl, safe_profile_requested);
                }
            }
        }
    }

    fn publish_finished(&self, result: &RunResult) {
        self.finish(
            result.state,
            result.failure.as_ref().map(|f| f.message.clone()),
            result.finished_at_unix_ms,
        );
    }

    fn abort_startup(&mut self, error: &anyhow::Error, safe: bool) {
        if let Some(drivers) = self.drivers.take()
            && let Err(cleanup_error) = drivers.abort_startup(
                &self.attempt_id,
                self.agentctl.snapshot(),
                safe,
                format!("Session startup failed: {error:#}"),
            )
        {
            tracing::warn!(%cleanup_error, "persist Session startup failure");
        }
        let result = failed_output(format!("Session startup failed: {error:#}")).into_result(
            &self.spec,
            &self.attempt_id,
            self.created_at_unix_ms,
        );
        self.publish_finished(&result);
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

fn persist_result(
    teardown: &mut AttemptTeardown,
    result: &mut RunResult,
    agentctl: &crate::AgentCtlControl,
    safe_profile_requested: bool,
) {
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
}

fn fail_finalization(result: &mut RunResult, message: String) {
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

pub(crate) async fn terminate_process_tree(
    child: &mut tokio::process::Child,
    process_group: Option<u32>,
    grace_ms: u64,
) {
    #[cfg(unix)]
    {
        if let Some(pid) = process_group {
            // The child is the leader of the process group configured above.
            let process_group = -(pid as i32);
            unsafe {
                libc::kill(process_group, libc::SIGTERM);
            }
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(grace_ms);
            loop {
                // Reaping the leader does not mean its descendants have exited.
                let _ = child.try_wait();
                if unsafe { libc::kill(process_group, 0) } != 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
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
