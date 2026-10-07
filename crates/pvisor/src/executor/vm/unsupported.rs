//! Intel macOS VM stub.
//!
//! The pvisor-vm macOS backend is supported only on Apple Silicon. Keeping this
//! target out of the pvisor-vm runtime dependency graph lets the host/container CLI and
//! their tests build on Intel macOS while producing a clear VM error.

use crate::config::VmSettings;
use crate::executor::{ExecutorOutput, RunExecutor, Session};
use async_trait::async_trait;
use pvisor_core::{
    CapabilityEnforcementPlan, ExecutorKind, ExecutorPlan, IsolationKind, ProcessOutput,
    RunFailure, RunFailureKind, RunInvocation, RunState,
};

const UNSUPPORTED_MESSAGE: &str =
    "VM execution is unsupported on Intel macOS; use Linux x86_64 or Apple Silicon macOS";

#[derive(Debug, Clone)]
pub struct VmExecutor {
    settings: VmSettings,
}

impl VmExecutor {
    pub(crate) fn materialize_restore_ram(
        &mut self,
        _storage: &std::path::Path,
    ) -> anyhow::Result<()> {
        anyhow::bail!(UNSUPPORTED_MESSAGE)
    }

    pub(crate) fn restored_guest_environment(
        &self,
    ) -> Option<std::collections::BTreeMap<String, String>> {
        None
    }
    pub fn checkpoint_compatibility(
        _settings: &VmSettings,
    ) -> anyhow::Result<crate::environment_snapshot::Compatibility> {
        anyhow::bail!(UNSUPPORTED_MESSAGE)
    }
    pub fn with_private_ram(self) -> anyhow::Result<Self> {
        anyhow::bail!("private VM RAM is unsupported on this platform")
    }

    pub fn new(_settings: VmSettings) -> anyhow::Result<Self> {
        anyhow::bail!(UNSUPPORTED_MESSAGE)
    }

    pub fn with_vsock_ports(
        self,
        _ports: std::collections::BTreeMap<u32, std::path::PathBuf>,
    ) -> anyhow::Result<Self> {
        anyhow::bail!(UNSUPPORTED_MESSAGE)
    }

    pub fn settings(&self) -> &VmSettings {
        &self.settings
    }
    pub fn restore(
        _settings: VmSettings,
        _checkpoint: pvisor_core::operation::ExecutionCheckpoint,
        _storage: &std::path::Path,
    ) -> anyhow::Result<(Self, crate::OverlayHint)> {
        anyhow::bail!(UNSUPPORTED_MESSAGE)
    }
}

#[async_trait]
impl RunExecutor for VmExecutor {
    fn descriptor(&self) -> ExecutorPlan {
        ExecutorPlan {
            name: "libkrun-root-overlay-v1".into(),
            kind: ExecutorKind::VirtualMachine,
            isolation: IsolationKind::VirtualMachine,
            capability_plan: CapabilityEnforcementPlan::default(),
            supports_checkpoint: false,
            supports_migration: false,
        }
    }

    fn supports(&self, invocation: &RunInvocation) -> bool {
        matches!(invocation, RunInvocation::Process(_))
    }

    async fn execute(&self, _context: &Session) -> ExecutorOutput {
        ExecutorOutput {
            executor_observations: Default::default(),

            state: RunState::Failed,

            exit_code: None,
            failure: Some(RunFailure {
                kind: RunFailureKind::Spawn,
                message: UNSUPPORTED_MESSAGE.into(),
                retryable: false,
            }),
            output: ProcessOutput::default(),
            value: None,
            metrics: Default::default(),
            artifacts: Vec::new(),
            event_stream_ref: None,
            warnings: Vec::new(),
        }
    }
}

pub fn run_internal_if_requested() -> anyhow::Result<bool> {
    Ok(false)
}
