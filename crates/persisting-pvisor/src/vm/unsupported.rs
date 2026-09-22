//! Intel macOS VM stub.
//!
//! libkrun's macOS backend is supported only on Apple Silicon. Keeping this
//! target out of the libkrun dependency graph lets the host/container CLI and
//! their tests build on Intel macOS while producing a clear VM error.

use crate::config::VmSettings;
use crate::executor::{AttemptContext, RunExecutor};
use async_trait::async_trait;
use persisting_control::{
    CapabilityEnforcementEvidence, ExecutorDescriptor, ExecutorKind, IsolationKind, ProcessOutput,
    RunFailure, RunFailureKind, RunInvocation, RunResult, RunState,
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
    fn descriptor(&self) -> ExecutorDescriptor {
        ExecutorDescriptor {
            name: "libkrun-root-overlay-v1".into(),
            kind: ExecutorKind::VirtualMachine,
            isolation: IsolationKind::VirtualMachine,
            capability_enforcement: CapabilityEnforcementEvidence::default(),
            supports_checkpoint: false,
            supports_migration: false,
        }
    }

    fn supports(&self, invocation: &RunInvocation) -> bool {
        matches!(invocation, RunInvocation::Process(_))
    }

    async fn execute(&self, context: AttemptContext) -> RunResult {
        let spec = context.spec();
        RunResult {
            run_id: spec.run_id.clone(),
            attempt_id: context.attempt_id().clone(),
            lease_epoch: spec.lease_epoch,
            state: RunState::Failed,
            started_at_unix_ms: crate::util::unix_now_ms(),
            finished_at_unix_ms: crate::util::unix_now_ms(),
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
