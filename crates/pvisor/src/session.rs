//! One execution Session owns its drivers, controls and lifecycle.
pub(crate) mod lifecycle;

use crate::executor::AttemptAttachments;
use crate::runtime::event::RunEventPublisher;
use pvisor_core::{AttemptId, RunSpec};
use std::sync::Arc;

/// Attempt-scoped execution identity, controls, policy and lifecycle owner.
pub struct Session {
    pub(crate) spec: Arc<RunSpec>,
    pub(crate) created_at_unix_ms: u64,
    pub(crate) network_policy: pvisor_core::NetworkPolicy,
    pub(crate) attempt_id: AttemptId,
    pub(crate) vm_control: crate::executor::vm::control::VmControl,
    pub(crate) execution: crate::runtime::execution::Execution,
    pub(crate) events: RunEventPublisher,
    pub(crate) agentctl: crate::AgentCtlControl,
    pub(crate) attachments: AttemptAttachments,
    pub(crate) drivers: Option<crate::runtime::AttemptSession>,
    pub(crate) server: Option<crate::AgentCtlServer>,
    pub(crate) output_observer: Option<crate::runtime::run::ExecutorOutputObserver>,
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

    /// The Attempt's sole status and live-control owner.
    pub fn execution(&self) -> &crate::Execution {
        &self.execution
    }

    pub(crate) fn events(&self) -> &RunEventPublisher {
        &self.events
    }
}
