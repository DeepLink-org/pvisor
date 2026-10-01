//! One execution Session owns its drivers, controls, observations and lifecycle.
pub(crate) mod lifecycle;

use crate::executor::AttemptAttachments;
use crate::runtime::event::RunEventPublisher;
use async_trait::async_trait;
use persisting_control::{
    AttemptId, RunSpec, RunState, RunStatus, SessionIdentity, SessionObservation, SessionPhase,
};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Trusted in-process extensions run in registration order. Errors veto a phase;
/// execution errors still pass through teardown and durable finalization.
#[async_trait]
pub trait SessionExtension: Send + Sync {
    async fn on_phase(
        &self,
        session: &Session,
        observation: &SessionObservation,
    ) -> anyhow::Result<()>;
    /// Called after terminal publication. Notification only: cannot change the result.
    fn finished(&self, _session: &Session, _result: &persisting_control::RunResult) {}
}

/// Attempt-scoped execution identity, controls, policy and lifecycle owner.
pub struct Session {
    pub(crate) spec: Arc<RunSpec>,
    pub(crate) created_at_unix_ms: u64,
    pub(crate) network_policy: persisting_control::NetworkPolicy,
    pub(crate) attempt_id: AttemptId,
    pub(crate) cancel: CancellationToken,
    pub(crate) status: watch::Sender<RunStatus>,
    pub(crate) events: RunEventPublisher,
    pub(crate) agentctl: crate::AgentCtlControl,
    pub(crate) attachments: AttemptAttachments,
    pub(crate) drivers: Option<crate::runtime::AttemptSession>,
    pub(crate) server: Option<crate::AgentCtlServer>,
    pub(crate) extensions: Vec<Arc<dyn SessionExtension>>,
    pub(crate) observation: watch::Sender<SessionObservation>,
}

impl Session {
    pub fn identity(&self) -> SessionIdentity {
        SessionIdentity {
            run_id: self.spec.run_id.clone(),
            attempt_id: self.attempt_id.clone(),
            lease_epoch: self.spec.lease_epoch,
        }
    }

    pub fn observation(&self) -> SessionObservation {
        let mut observation = self.observation.borrow().clone();
        observation.status = self.status();
        observation
    }

    pub(crate) async fn phase(
        &self,
        phase: SessionPhase,
        result: Option<&persisting_control::RunResult>,
    ) -> anyhow::Result<()> {
        let observation = SessionObservation {
            version: persisting_control::SESSION_PROTOCOL_VERSION,
            session: self.identity(),
            phase,
            status: self.status(),
            result: result.cloned(),
        };
        self.observation.send_replace(observation.clone());
        for extension in &self.extensions {
            extension.on_phase(self, &observation).await?;
        }
        Ok(())
    }

    pub fn spec(&self) -> &RunSpec {
        &self.spec
    }

    pub fn network_policy(&self) -> &persisting_control::NetworkPolicy {
        &self.network_policy
    }

    pub fn filesystem_policy(&self) -> Option<&persisting_control::FileAccessPolicy> {
        self.attachments.filesystem.as_ref()
    }

    pub fn attempt_id(&self) -> &AttemptId {
        &self.attempt_id
    }

    pub fn cancellation(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub(crate) fn take_vm_network(
        &self,
    ) -> anyhow::Result<Option<crate::runtime::VmNetworkAttachment>> {
        let Some(attachment) = &self.attachments.vm_network else {
            return Ok(None);
        };
        let mut attachment = attachment
            .lock()
            .map_err(|_| anyhow::anyhow!("VM network attachment lock poisoned"))?;
        Ok(attachment.take())
    }

    pub fn status(&self) -> RunStatus {
        self.status.borrow().clone()
    }

    pub(crate) fn events(&self) -> &RunEventPublisher {
        &self.events
    }

    pub(crate) fn import_delegated_agentctl(&self, snapshot: crate::AgentCtlSnapshot) {
        self.agentctl.import_delegated_snapshot(snapshot);
    }

    pub async fn transition(&self, state: RunState, message: impl Into<Option<String>>) {
        // Terminal publication is reserved for Session completion after resource teardown.
        if state.is_terminal() {
            return;
        }
        let now = crate::util::unix_now_ms();
        let message = message.into();
        self.status.send_modify(|status| {
            status.state = state;
            status.updated_at_unix_ms = now;
            status.message = message.clone();
            if matches!(state, RunState::Starting | RunState::Running)
                && status.attempt.started_at_unix_ms.is_none()
            {
                status.attempt.started_at_unix_ms = Some(now);
            }
        });
        let status = self.status();
        self.observation
            .send_modify(|observation| observation.status = status);
        let _ = self
            .events
            .publish(
                "run.state_changed",
                "runtime",
                json!({
                    "state": state,
                    "message": message,
                }),
            )
            .await;
    }

    /// Make a terminal status visible after finalization and terminal-event commit.
    pub(crate) fn finish(&self, state: RunState, message: Option<String>, now: u64) {
        self.status.send_modify(|status| {
            status.state = state;
            status.updated_at_unix_ms = now;
            status.message = message.clone();
            status.attempt.finished_at_unix_ms = Some(now);
        });
    }
}
