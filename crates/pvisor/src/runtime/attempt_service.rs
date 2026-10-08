//! Shared in-process dispatcher for one Attempt; not an RPC endpoint.
//!
//! Callers must authenticate and validate the exact Job/Attempt target before
//! dispatch. Generation and resource-owner authority remain endpoint-specific.

use super::RunControlHandle;
use pvisor_core::RunStatus;
use pvisor_core::host_protocol::{
    AgentCtlHostError, AgentCtlHostErrorCode, HostAttemptCommand, HostAttemptResult,
};
use pvisor_core::operation::OperationKind;

/// Cloneable authority for an already selected Attempt, without lifecycle ownership.
#[derive(Clone)]
pub struct AttemptService {
    controls: RunControlHandle,
}

impl AttemptService {
    pub fn new(controls: RunControlHandle) -> Self {
        Self { controls }
    }

    pub fn status(&self) -> RunStatus {
        self.controls.status()
    }

    /// Dispatch against this handle's Attempt only. Terminate requests cancellation;
    /// its reply is not evidence of reaping or resource release. Native transitions
    /// retain RunControlHandle's ordering and survive cancellation of the waiter.
    pub async fn dispatch(
        &self,
        command: HostAttemptCommand,
    ) -> Result<HostAttemptResult, AgentCtlHostError> {
        let value = match command {
            HostAttemptCommand::Status => None,
            HostAttemptCommand::Terminate => {
                self.controls.cancel();
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
                Some(self.controls.control(kind).await.map_err(|error| {
                    AgentCtlHostError::new(AgentCtlHostErrorCode::Unavailable, format!("{error:#}"))
                })?)
            }
        };
        Ok(HostAttemptResult {
            status: self.status(),
            value,
        })
    }
}
