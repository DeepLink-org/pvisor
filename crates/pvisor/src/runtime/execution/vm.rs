//! Native control effects; state changes are committed through the execution owner.
use super::{ControlOrigin, Execution, error_message};
use pvisor_core::host_protocol::{
    AgentCtlHostError, AgentCtlHostErrorCode, HostAttemptCommand, HostAttemptResult,
};
use pvisor_core::operation::{OperationKind, Value};
use tokio_util::sync::CancellationToken;

impl Execution {
    /// Dispatch against this handle's Attempt only. Terminate requests cancellation;
    /// its reply is not evidence of reaping or resource release. Native transitions
    /// retain Execution's ordering and survive cancellation of the waiter.
    pub async fn dispatch(
        &self,
        command: HostAttemptCommand,
    ) -> Result<HostAttemptResult, AgentCtlHostError> {
        let value = match command {
            HostAttemptCommand::Status => None,
            HostAttemptCommand::Terminate => {
                self.cancel();
                None
            }
            HostAttemptCommand::Operation { kind } => {
                if matches!(kind, OperationKind::RunExecute { .. }) {
                    return Err(AgentCtlHostError::new(
                        AgentCtlHostErrorCode::Unsupported,
                        "run.execute cannot control an existing Attempt",
                    ));
                }
                if self.status().attempt.executor.kind != pvisor_core::ExecutorKind::VirtualMachine
                {
                    return Err(AgentCtlHostError::new(
                        AgentCtlHostErrorCode::Unsupported,
                        "live VM controls require a VM executor",
                    ));
                }
                kind.validate().map_err(|error| {
                    AgentCtlHostError::new(
                        AgentCtlHostErrorCode::InvalidRequest,
                        format!("{error:#}"),
                    )
                })?;
                Some(self.control(kind).await.map_err(|error| {
                    AgentCtlHostError::new(AgentCtlHostErrorCode::Unavailable, format!("{error:#}"))
                })?)
            }
        };
        Ok(HostAttemptResult {
            status: self.status(),
            value,
        })
    }
    /// Capture full native CPU/RAM/devices and owned filesystem copies in one
    /// freeze/publication transaction. The source resumes after durable seal.
    /// Requires a no-network VM with durable Run storage. This API does not
    /// suspend the Job or hand execution to another Attempt.
    pub async fn capture_execution(
        &self,
        request_id: impl Into<String>,
        ram_storage: pvisor_core::operation::SnapshotRamStorage,
    ) -> anyhow::Result<pvisor_core::operation::ExecutionCheckpoint> {
        let value = self
            .control(pvisor_core::operation::OperationKind::RunCheckpoint {
                request_id: request_id.into(),
                ram_storage,
            })
            .await?;
        let pvisor_core::operation::Value::ExecutionCheckpoint { checkpoint } = value else {
            anyhow::bail!("native capture returned no execution checkpoint")
        };
        Ok(checkpoint)
    }

    /// Pause all vCPUs and await native acknowledgement. Device I/O and the wall-time
    /// deadline remain active. Unsupported executors reject; uncertain effects fail closed.
    pub async fn pause(&self) -> anyhow::Result<()> {
        self.control(pvisor_core::operation::OperationKind::RunPause)
            .await
            .map(|_| ())
    }

    /// Resume the same live Attempt and await native acknowledgement. Repeated calls
    /// are idempotent while available; this does not restore a terminated VM.
    pub async fn resume(&self) -> anyhow::Result<()> {
        self.control(pvisor_core::operation::OperationKind::RunResume)
            .await
            .map(|_| ())
    }

    /// Pause and reclaim live file-backed RAM. A new destination must be on the
    /// same filesystem as the current backing. None retains the existing file.
    pub async fn offload(
        &self,
        file: Option<std::path::PathBuf>,
    ) -> anyhow::Result<pvisor_core::operation::VmMemory> {
        let value = self
            .control(pvisor_core::operation::OperationKind::RunOffload { file })
            .await?;
        let pvisor_core::operation::Value::Vm {
            memory: Some(memory),
            ..
        } = value
        else {
            anyhow::bail!("offload returned no RAM report")
        };
        Ok(memory)
    }

    #[cfg(feature = "gateway")]
    pub(crate) fn monitor_wait_termination(&self) {
        let mut status = self.subscribe();
        let cancellation = self.cancellation.clone();
        tokio::spawn(async move {
            loop {
                if status.borrow().status.state.is_terminal() {
                    cancellation.cancel();
                    return;
                }
                tokio::select! {
                    _ = cancellation.cancelled() => return,
                    changed = status.changed() => {
                        if changed.is_err() { cancellation.cancel(); return; }
                    }
                }
            }
        });
    }

    /// Request cancellation immediately; completion still waits for resource teardown.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Clone the latched cancellation token for cancellation-safe frontend waits.
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Read-only physical observations never acquire the native control exchange
    /// lock, modify VM state, or retain snapshot RAM/pager ownership.
    pub async fn memory_sample(&self) -> pvisor_core::memory::RunMemorySample {
        let status = self.status();
        let started = pvisor_core::unix_now_ms();
        let result = if status.attempt.executor.kind != pvisor_core::ExecutorKind::VirtualMachine
            || status.state.is_terminal()
        {
            Err(anyhow::anyhow!(
                "memory observation requires a live native VM"
            ))
        } else {
            self.vm_control.memory_usage().await
        };
        let (usage, error) = match result {
            Ok(usage) => (Some(usage), None),
            Err(error) => (None, Some(error_message(&error))),
        };
        pvisor_core::memory::RunMemorySample {
            run_id: status.run_id,
            attempt_id: status.attempt.attempt_id,
            sampled_at_unix_ms: started,
            usage,
            error,
        }
    }

    /// Read-only CPU observations never acquire the native control exchange
    /// lock, modify VM state, or retain snapshot RAM/pager ownership.
    pub async fn cpu_sample(&self) -> pvisor_core::cpu::RunCpuSample {
        let status = self.status();
        let started = pvisor_core::unix_now_ms();
        let result = if status.attempt.executor.kind != pvisor_core::ExecutorKind::VirtualMachine
            || status.state.is_terminal()
        {
            Err(anyhow::anyhow!("CPU observation requires a live native VM"))
        } else {
            self.vm_control.cpu_usage().await
        };
        let (usage, error) = match result {
            Ok(usage) => (Some(usage), None),
            Err(error) => (None, Some(error_message(&error))),
        };
        pvisor_core::cpu::RunCpuSample {
            run_id: status.run_id,
            attempt_id: status.attempt.attempt_id,
            sampled_at_unix_ms: started,
            usage,
            error,
        }
    }

    /// Wait through asynchronous attempt startup without issuing a primitive
    /// before the native control endpoint is usable. Cancellation still fences it.
    pub async fn wait_ready(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.status().attempt.executor.kind == pvisor_core::ExecutorKind::VirtualMachine,
            "VM controls require a VM executor"
        );
        let mut status = self.subscribe();
        loop {
            let state = status.borrow_and_update().status.state;
            anyhow::ensure!(
                !self.cancellation.is_cancelled()
                    && !state.is_terminal()
                    && state != pvisor_core::RunState::Cancelling,
                "attempt ended before control readiness"
            );
            if matches!(
                state,
                pvisor_core::RunState::Running | pvisor_core::RunState::Suspended
            ) {
                return Ok(());
            }
            tokio::select! {
                _ = self.cancellation.cancelled() => {
                    anyhow::bail!("attempt ended before control readiness");
                }
                changed = status.changed() => {
                    changed.map_err(|_| anyhow::anyhow!("attempt status closed before control readiness"))?;
                }
            }
        }
    }

    /// Admit and execute a live VM primitive. Accepted work survives a dropped waiter.
    /// Manual controls share the automatic-wait arbitration and native transition fence.
    pub async fn control(&self, kind: OperationKind) -> anyhow::Result<Value> {
        #[cfg(feature = "gateway")]
        if let Some(coordinator) = &self.wait_coordinator {
            return coordinator.control(kind).await;
        }
        Ok(self.exchange(kind, None, ControlOrigin::Manual).await?.0)
    }

    fn ensure_live(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.cancellation.is_cancelled(), "attempt is cancelling");
        anyhow::ensure!(
            matches!(
                self.status().state,
                pvisor_core::RunState::Running | pvisor_core::RunState::Suspended
            ),
            "attempt is not running or suspended"
        );
        Ok(())
    }

    async fn exchange(
        &self,
        kind: OperationKind,
        idle_epoch: Option<u64>,
        origin: ControlOrigin,
    ) -> anyhow::Result<(Value, Option<u64>)> {
        let execution = self.clone();
        tokio::spawn(async move {
            execution.ensure_live()?;
            let id = execution.enqueue(kind.clone(), origin).await?;
            let result = async {
                let _transition = execution.vm_control.transition.lock().await;
                execution.ensure_live()?;
                execution.perform_control(kind, idle_epoch, &id).await
            }
            .await;
            if let Err(error) = &result {
                execution.control_error(&id, error, origin).await;
            }
            result
        })
        .await?
    }

    async fn perform_control(
        &self,
        kind: pvisor_core::operation::OperationKind,
        idle_epoch: Option<u64>,
        id: &str,
    ) -> anyhow::Result<(pvisor_core::operation::Value, Option<u64>)> {
        let mut operation = self.control_operation.clone();
        operation.kind = kind.clone();
        operation.rules.clear();
        operation
            .placements
            .retain(|placement| matches!(placement, pvisor_core::operation::Placement::Vm { .. }));
        operation.validate()?;
        let events = &self.events;
        events
            .publish(
                "vm.control_requested",
                "runtime",
                serde_json::json!({"operation": operation, "control_id": id}),
            )
            .await?;
        self.ensure_live()?;
        self.mark_dispatched(id).await?;
        match if idle_epoch.is_some() {
            self.vm_control.command_with_idle(kind, idle_epoch).await
        } else {
            self.vm_control.command(kind).await
        } {
            Ok(reply) => {
                let state = reply
                    .state
                    .ok_or_else(|| anyhow::anyhow!("VM control returned no state"))?;
                let value = if let Some(checkpoint) = reply.checkpoint {
                    anyhow::ensure!(
                        checkpoint.source_run_id == operation.run_id
                            && checkpoint.source_attempt_id
                                == self.status().attempt.attempt_id.as_str(),
                        "checkpoint belongs to another attempt"
                    );
                    Value::ExecutionCheckpoint { checkpoint }
                } else {
                    Value::Vm {
                        state,
                        memory: reply.memory,
                    }
                };
                self.succeeded(id, value.clone()).await?;
                events
                    .publish(
                        "vm.control_completed",
                        "runtime",
                        serde_json::json!({"operation": operation, "value": value, "control_id": id, "guest_idle_epoch": idle_epoch, "next_guest_idle_epoch": reply.idle_epoch}),
                    )
                    .await
                    .map_err(|error| {
                        anyhow::anyhow!("VM control completed but observation failed: {error}")
                    })?;
                Ok((value, reply.idle_epoch))
            }
            Err(error) => {
                let _ = events
                    .publish(
                        "vm.control_failed",
                        "runtime",
                        serde_json::json!({"operation": operation, "error": error.to_string(), "control_id": id}),
                    )
                    .await;
                Err(error)
            }
        }
    }
}

#[cfg(feature = "gateway")]
#[async_trait::async_trait]
impl crate::runtime::wait_coordinator::WaitControl for Execution {
    fn supports_resume_rearm(&self) -> bool {
        true
    }
    async fn idle_settled(&self) -> anyhow::Result<()> {
        self.vm_control.idle.settled().await
    }
    async fn idle_resume_rearm(
        &self,
        epoch: u64,
        wake: CancellationToken,
    ) -> anyhow::Result<tokio::sync::oneshot::Receiver<Option<u64>>> {
        self.vm_control
            .idle
            .resume_prepare(wake, async {
                let (_, next) = self
                    .exchange(
                        pvisor_core::operation::OperationKind::RunResume,
                        Some(epoch),
                        ControlOrigin::ModelWait,
                    )
                    .await?;
                next.ok_or_else(|| anyhow::anyhow!("missing resume-rearm ACK"))
            })
            .await
    }
    fn pending_host_reads(&self) -> u64 {
        self.vm_control.idle.pending_host_reads()
    }
    async fn idle_permit(&self, wake: CancellationToken) -> anyhow::Result<Option<u64>> {
        self.vm_control.idle.prepare(wake).await
    }
    async fn idle_pause(&self, epoch: u64) -> anyhow::Result<bool> {
        let value = self
            .exchange(
                pvisor_core::operation::OperationKind::RunPause,
                Some(epoch),
                ControlOrigin::ModelWait,
            )
            .await?
            .0;
        Ok(matches!(
            value,
            pvisor_core::operation::Value::Vm {
                state: pvisor_core::operation::VmState::Paused,
                ..
            }
        ))
    }
    async fn idle_offload(&self, epoch: u64) -> anyhow::Result<bool> {
        let value = self
            .exchange(
                pvisor_core::operation::OperationKind::RunOffload { file: None },
                Some(epoch),
                ControlOrigin::ModelWait,
            )
            .await?
            .0;
        Ok(matches!(
            value,
            pvisor_core::operation::Value::Vm {
                state: pvisor_core::operation::VmState::Offloaded,
                memory: Some(_),
            }
        ))
    }
    fn state(&self) -> pvisor_core::RunState {
        self.status().state
    }
    fn can_offload(&self) -> bool {
        self.vm_control.automatic_offload_supported()
    }
    async fn wait_failed(&self, error: &anyhow::Error) {
        Execution::wait_failed(self, error).await;
    }
    fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
    async fn execute(
        &self,
        operation: pvisor_core::operation::OperationKind,
        origin: ControlOrigin,
    ) -> anyhow::Result<pvisor_core::operation::Value> {
        Ok(self.exchange(operation, None, origin).await?.0)
    }
}
