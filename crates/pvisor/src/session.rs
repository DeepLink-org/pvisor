//! One execution Session owns its drivers, controls and lifecycle.
pub(crate) mod lifecycle;

use crate::executor::AttemptAttachments;
use crate::runtime::event::RunEventPublisher;
use pvisor_core::{AttemptId, RunSpec, RunState, RunStatus};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Attempt-scoped execution identity, controls, policy and lifecycle owner.
pub struct Session {
    pub(crate) spec: Arc<RunSpec>,
    pub(crate) created_at_unix_ms: u64,
    pub(crate) network_policy: pvisor_core::NetworkPolicy,
    pub(crate) attempt_id: AttemptId,
    pub(crate) cancel: CancellationToken,
    pub(crate) vm_control: crate::executor::vm::control::VmControl,
    pub(crate) status: watch::Sender<RunStatus>,
    pub(crate) events: RunEventPublisher,
    pub(crate) agentctl: crate::AgentCtlControl,
    pub(crate) attachments: AttemptAttachments,
    pub(crate) drivers: Option<crate::runtime::AttemptSession>,
    pub(crate) server: Option<crate::AgentCtlServer>,
}

impl Session {
    pub fn spec(&self) -> &RunSpec {
        &self.spec
    }

    pub fn network_policy(&self) -> &pvisor_core::NetworkPolicy {
        &self.network_policy
    }

    pub fn filesystem_policy(&self) -> Option<&pvisor_core::FileAccessPolicy> {
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
