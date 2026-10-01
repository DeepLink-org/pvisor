//! Intel macOS VM stub.
//!
//! libkrun's macOS backend is supported only on Apple Silicon. Keeping this
//! target out of the libkrun dependency graph lets the host/container CLI and
//! their tests build on Intel macOS while producing a clear VM error.

use crate::config::VmSettings;
use crate::executor::{ExecutorOutput, ExecutorSession, RunExecutor};
use async_trait::async_trait;
use persisting_control::{
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
    pub fn new(_settings: VmSettings) -> anyhow::Result<Self> {
        anyhow::bail!(UNSUPPORTED_MESSAGE)
    }

    pub fn settings(&self) -> &VmSettings {
        &self.settings
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

    async fn execute(&self, _context: &ExecutorSession) -> ExecutorOutput {
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
