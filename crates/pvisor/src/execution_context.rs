//! The executor context owns resources and controls for one Session execution.
pub(crate) mod lifecycle;

use crate::executor::ExecutionAttachments;
use crate::runtime::event::RunEventPublisher;
use pvisor_core::{ExecutionId, RunSpec};
use std::sync::Arc;

/// Executor-facing identity, policy, controls and resources for one Session execution.
pub struct ExecutionContext {
    pub(crate) vm_inputs: crate::VmExecutionInputs,
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

    /// Snapshot the prepared guest network addresses without claiming its owner.
    /// None means no VM network attachment or an already-claimed owner. Mutex
    /// failure returns an error. Read before `take_vm_network` when preparing IO.
    pub fn vm_network_config(&self) -> anyhow::Result<Option<pvisor_guest::NetworkConfig>> {
        let Some(slot) = &self.attachments.vm_network else {
            return Ok(None);
        };
        let owner = slot
            .lock()
            .map_err(|_| anyhow::anyhow!("VM network attachment lock poisoned"))?;
        Ok(owner.as_ref().map(|owner| owner.guest_config()))
    }

    /// Claim the Execution's network owner once. Later calls return None. The
    /// caller owns shutdown and must stop guest IO before returning from execute.
    /// Mutex failure returns an error without transferring the owner.
    pub fn take_vm_network(&self) -> anyhow::Result<Option<crate::VmNetworkAttachment>> {
        let Some(attachment) = &self.attachments.vm_network else {
            return Ok(None);
        };
        let mut attachment = attachment
            .lock()
            .map_err(|_| anyhow::anyhow!("VM network attachment lock poisoned"))?;
        Ok(attachment.take())
    }

    /// Validated VM launch inputs, independent of metadata annotations.
    pub fn vm_inputs(&self) -> &crate::VmExecutionInputs {
        &self.vm_inputs
    }

    /// Prepared mountless workspace, available only after explicit executor opt-in.
    /// Borrowing does not transfer the stage lease or permit use after execute ends.
    pub fn workspace_overlay(&self) -> Option<&crate::GuestOverlay> {
        self.attachments.workspace_overlay.as_ref()
    }

    /// The Execution's sole status and live-control owner.
    pub fn execution(&self) -> &crate::Execution {
        &self.execution
    }

    pub(crate) fn events(&self) -> &RunEventPublisher {
        &self.events
    }
}
