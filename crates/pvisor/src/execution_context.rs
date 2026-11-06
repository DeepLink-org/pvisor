//! The executor context owns resources and controls for one Session execution.
pub(crate) mod lifecycle;

use crate::executor::ExecutionAttachments;
use crate::runtime::event::RunEventPublisher;
use pvisor_core::{ExecutionId, RunSpec};
use std::sync::Arc;

/// Executor-facing identity, policy, controls and resources for one Session execution.
pub struct ExecutionContext {
    pub(crate) spec: Arc<RunSpec>,
    pub(crate) created_at_unix_ms: u64,
    pub(crate) network_policy: pvisor_core::NetworkPolicy,
    pub(crate) execution_id: ExecutionId,
    pub(crate) vm_control: crate::executor::vm::control::VmControl,
    pub(crate) execution: crate::runtime::execution::Execution,
    pub(crate) events: RunEventPublisher,
    pub(crate) agentctl: crate::AgentCtlControl,
    pub(crate) attachments: ExecutionAttachments,
    pub(crate) drivers: Option<crate::runtime::SessionDrivers>,
    pub(crate) server: Option<crate::AgentCtlServer>,
    pub(crate) output_observer: Option<crate::runtime::run::ExecutorOutputObserver>,
}

impl ExecutionContext {
    pub fn spec(&self) -> &RunSpec {
        &self.spec
    }

    pub fn network_policy(&self) -> &pvisor_core::NetworkPolicy {
        &self.network_policy
    }

    pub fn filesystem_policy(&self) -> Option<&pvisor_core::FileAccessPolicy> {
        self.attachments.filesystem.as_ref()
    }

    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
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

    /// The Execution's sole status and live-control owner.
    pub fn execution(&self) -> &crate::Execution {
        &self.execution
    }

    pub(crate) fn events(&self) -> &RunEventPublisher {
        &self.events
    }
}
