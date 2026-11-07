//! Managed launch contracts exercised as an embedding caller, without a VM.
use async_trait::async_trait;
use pvisor::session_service::SessionService;
use pvisor::{
    ExecutionContext, ExecutorOutput, PVisor, ProcessExecutor, RunConfig, RunExecutor,
    RunExecutorKind, VmSettings,
};
use pvisor_core::{ExecutorKind, ExecutorPlan, IsolationKind, RunInvocation, RunSpec, RunState};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct SelectedExecutor {
    selections: Arc<AtomicUsize>,
    vm: Option<VmSettings>,
}
#[async_trait]
impl RunExecutor for SelectedExecutor {
    fn descriptor(&self) -> ExecutorPlan {
        let mut plan = ProcessExecutor::default().descriptor();
        if self.vm.is_some() {
            plan.kind = ExecutorKind::VirtualMachine;
            plan.isolation = IsolationKind::VirtualMachine;
        }
        plan
    }
    fn supports(&self, _: &RunInvocation) -> bool {
        self.selections.fetch_add(1, Ordering::SeqCst) == 0
    }
    fn continuation_settings(&self) -> Option<(VmSettings, pvisor::features::FeatureSettings)> {
        self.vm
            .clone()
            .map(|settings| (settings, Default::default()))
    }
    async fn execute(&self, context: &ExecutionContext) -> ExecutorOutput {
        ProcessExecutor::default().execute(context).await
    }
}
fn runtime(
    stage: &std::path::Path,
    selections: &Arc<AtomicUsize>,
    vm: Option<VmSettings>,
) -> PVisor {
    PVisor::builder()
        .storage(stage)
        .instance_control(false)
        .network(pvisor::NetworkDriverConfig::new(
            pvisor::OverlayNetMode::Off,
            Default::default(),
        ))
        .executors(vec![Arc::new(SelectedExecutor {
            selections: selections.clone(),
            vm,
        })])
        .build()
}

#[tokio::test]
async fn admitted_executor_is_selected_once_and_plan_has_no_startup_effects() {
    let root = tempfile::tempdir().unwrap();
    let stage = root.path().join("stage");
    let selections = Arc::new(AtomicUsize::new(0));
    let visor = runtime(&stage, &selections, None);
    let mut spec = RunSpec::process("admitted-session", "test", "/bin/sh");
    let RunInvocation::Process(process) = &mut spec.invocation;
    process.args = vec!["-c".into(), "exit 7".into()];
    spec.runtime.timeout_ms = Some(10_000);
    let mut unrelated = RunConfig::default();
    unrelated.run.executor = RunExecutorKind::Vm;
    let plan = SessionService::prepare_managed(&visor, spec, unrelated).unwrap();
    assert_eq!(plan.config().run.executor, RunExecutorKind::Host);
    assert!(!stage.exists());
    assert_eq!(plan.spec().runtime.timeout_ms, Some(10_000));
    assert_eq!(plan.config().run.timeout_ms, Some(10_000));
    assert_eq!(selections.load(Ordering::SeqCst), 1);
    let result = plan.start().await.unwrap().wait().await.unwrap();
    assert_eq!(result.state, RunState::Failed);
    assert_eq!(result.exit_code, Some(7));
    assert_eq!(selections.load(Ordering::SeqCst), 1);
}

#[test]
fn vm_mismatched_continuation_is_rejected_before_storage_or_execution() {
    let root = tempfile::tempdir().unwrap();
    let stage = root.path().join("stage");
    let selections = Arc::new(AtomicUsize::new(0));
    let visor = runtime(&stage, &selections, Some(VmSettings::default()));
    let error = SessionService::prepare_managed(
        &visor,
        RunSpec::process("vm-session", "test", "/bin/true"),
        RunConfig::default(),
    )
    .err()
    .expect("Host continuation cannot describe a VM Session");
    assert!(error.to_string().contains("executor=vm"), "{error:#}");
    assert!(!stage.exists());
}

#[test]
fn continuation_captures_selected_executor_and_runtime_settings() {
    let root = tempfile::tempdir().unwrap();
    let stage = root.path().join("stage");
    let selections = Arc::new(AtomicUsize::new(0));
    let actual = VmSettings {
        cpus: 3,
        memory_mib: 512,
        rootfs: Some(root.path().to_owned()),
        ..Default::default()
    };
    let visor = runtime(&stage, &selections, Some(actual.clone()));
    let mut unrelated = RunConfig::default();
    unrelated.run.executor = RunExecutorKind::Vm;
    unrelated.vm.cpus = 1;
    unrelated.vm.library_dir = Some(root.path().join("invalid-caller-library"));
    unrelated.gateway.mode = pvisor::GatewayMode::Capture;
    let mut spec = RunSpec::process("vm-session", "test", "/bin/true");
    let inputs = pvisor::VmExecutionInputs {
        guest_cwd: Some("/work".into()),
        env_unset: vec!["HOME".into()],
        ..Default::default()
    };
    inputs.apply_to(&mut spec).unwrap();
    let plan = SessionService::prepare_managed(&visor, spec, unrelated).unwrap();
    assert_eq!(plan.vm_inputs(), &inputs);
    assert_eq!(plan.config().vm, actual);
    assert_eq!(plan.config().overlaynet.mode, pvisor::OverlayNetMode::Off);
    assert_eq!(plan.config().gateway.mode, pvisor::GatewayMode::Off);
    assert!(!stage.exists());
    drop(plan);
    assert!(!stage.exists());
}
