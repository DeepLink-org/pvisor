//! Native framed-control tests: real Attempt ownership with scripted runner ACKs.
use super::*;
use crate::runtime::run::model_wait::NativeLifecycle;
use crate::{NativeModelWait, PVisor, RunExecutor, RunHandle, Session};
use pvisor_core::{ExecutorKind, ModelCallRequest, RunInvocation, RunSpec, RunState};
use pvisor_gateway::model_wait::{ModelWait, ModelWaitLifecycle};
use tokio::sync::oneshot;

const LIMIT: Duration = Duration::from_secs(5);

struct ScriptedVm {
    runner: std::sync::Mutex<Option<oneshot::Sender<UnixStream>>>,
    backing: PathBuf,
}

#[async_trait::async_trait]
impl RunExecutor for ScriptedVm {
    fn descriptor(&self) -> pvisor_core::ExecutorPlan {
        let mut plan = crate::ProcessExecutor::default().descriptor();
        plan.kind = ExecutorKind::VirtualMachine;
        plan.isolation = pvisor_core::IsolationKind::VirtualMachine;
        plan
    }
    fn supports(&self, _: &RunInvocation) -> bool {
        true
    }
    async fn execute(&self, session: &Session) -> crate::ExecutorOutput {
        let (host, runner) = UnixStream::pair().unwrap();
        session
            .vm_control
            .attach(host, RamBacking::create(Some(&self.backing)).unwrap())
            .await;
        session.transition(RunState::Running, None).await;
        self.runner
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(runner)
            .unwrap();
        session.cancellation().cancelled().await;
        session.vm_control.detach().await;
        crate::ExecutorOutput {
            state: RunState::Cancelled,
            exit_code: None,
            failure: None,
            output: Default::default(),
            value: None,
            metrics: Default::default(),
            artifacts: vec![],
            event_stream_ref: None,
            warnings: vec![],
            executor_observations: Default::default(),
        }
    }
}

struct Fixture {
    handle: RunHandle,
    runner: UnixStream,
    lifecycle: NativeLifecycle,
    _storage: tempfile::TempDir,
}

impl Fixture {
    async fn new(mode: NativeModelWait) -> Self {
        let storage = tempfile::tempdir().unwrap();
        let (sender, receiver) = oneshot::channel();
        let handle = PVisor::builder()
            .executors(vec![Arc::new(ScriptedVm {
                runner: std::sync::Mutex::new(Some(sender)),
                backing: storage.path().join("ram"),
            })])
            .network(crate::NetworkDriverConfig {
                mode: crate::OverlayNetMode::Off,
                ..Default::default()
            })
            .instance_control(false)
            .build()
            .run(RunSpec::process("model-wait-test", "test", "/bin/true"))
            .await
            .unwrap();
        let runner = tokio::time::timeout(LIMIT, receiver)
            .await
            .unwrap()
            .unwrap();
        let lifecycle = NativeLifecycle::new(handle.controls(), mode);
        Self {
            handle,
            runner,
            lifecycle,
            _storage: storage,
        }
    }

    fn request(&self) -> ModelCallRequest {
        ModelCallRequest {
            run_id: Some(self.handle.run_id().clone()),
            attempt_id: Some(self.handle.attempt_id().clone()),
            call_id: "call".into(),
            client_model: "test".into(),
            upstream_model: "test".into(),
            provider: "openai".into(),
            protocol: "chat.completions".into(),
            upstream_host: "localhost".into(),
        }
    }

    fn reserve(&self) -> Box<dyn ModelWait> {
        self.lifecycle.reserve(self.request()).unwrap()
    }

    async fn operation(&mut self) -> OperationKind {
        tokio::time::timeout(LIMIT, async {
            let size = self.runner.read_u32().await.unwrap() as usize;
            assert!(size <= MAX_FRAME);
            let mut bytes = vec![0; size];
            self.runner.read_exact(&mut bytes).await.unwrap();
            serde_json::from_slice(&bytes).unwrap()
        })
        .await
        .unwrap()
    }

    async fn ack(&mut self, state: VmState) {
        let bytes = serde_json::to_vec(&ControlReply {
            state: Some(state),
            error: None,
            capture: None,
            checkpoint: None,
            memory: (state == VmState::Offloaded).then(|| VmMemory {
                backing_file: "runner-path-replaced-by-owner".into(),
                backed_bytes: 4096,
                resident_before_bytes: Some(4096),
                resident_after_bytes: Some(0),
            }),
        })
        .unwrap();
        self.runner.write_u32(bytes.len() as u32).await.unwrap();
        self.runner.write_all(&bytes).await.unwrap();
    }

    async fn enter(&mut self) -> Box<dyn ModelWait> {
        let mut wait = self.reserve();
        let task = tokio::spawn(async move {
            wait.enter().await.unwrap();
            wait
        });
        let operation = self.operation().await;
        let state = match operation {
            OperationKind::RunPause => VmState::Paused,
            OperationKind::RunOffload { file: None } => VmState::Offloaded,
            _ => panic!("unexpected {operation:?}"),
        };
        assert!(!task.is_finished(), "entry must wait for native ACK");
        self.ack(state).await;
        tokio::time::timeout(LIMIT, task).await.unwrap().unwrap()
    }

    async fn manual(&mut self, kind: OperationKind, state: VmState) {
        let controls = self.handle.controls();
        let expected = kind.clone();
        let task = tokio::spawn(async move { controls.control(kind).await });
        assert_eq!(self.operation().await, expected);
        self.ack(state).await;
        tokio::time::timeout(LIMIT, task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    async fn no_operation(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(30), self.runner.read_u32())
                .await
                .is_err()
        );
    }

    async fn finish(self) {
        self.handle.cancel();
        tokio::time::timeout(LIMIT, self.handle.wait())
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn native_model_wait_pause_and_offload_wait_for_ack_before_delivery() {
    for mode in [NativeModelWait::Pause, NativeModelWait::Offload] {
        let mut fixture = Fixture::new(mode).await;
        let mut wait = fixture.enter().await;
        assert_eq!(fixture.handle.status().state, RunState::Suspended);
        let task = tokio::spawn(async move { wait.before_delivery().await });
        assert_eq!(fixture.operation().await, OperationKind::RunResume);
        assert!(!task.is_finished());
        fixture.ack(VmState::Running).await;
        tokio::time::timeout(LIMIT, task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(fixture.handle.status().state, RunState::Running);
        fixture.no_operation().await;
        drop(fixture.reserve());
        fixture.finish().await;
    }
}

#[tokio::test]
async fn native_model_wait_manual_pause_owns_resume_and_delivery() {
    let mut fixture = Fixture::new(NativeModelWait::Pause).await;
    let mut wait = fixture.enter().await;
    fixture
        .manual(OperationKind::RunPause, VmState::Paused)
        .await;
    let task = tokio::spawn(async move { wait.before_delivery().await });
    fixture.no_operation().await;
    assert!(!task.is_finished());
    fixture
        .manual(OperationKind::RunResume, VmState::Running)
        .await;
    tokio::time::timeout(LIMIT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    fixture.no_operation().await;
    fixture.finish().await;
}

#[tokio::test]
async fn native_model_wait_drop_during_entry_retains_reservation_until_cleanup_ack() {
    let mut fixture = Fixture::new(NativeModelWait::Offload).await;
    let mut wait = fixture.reserve();
    let task = tokio::spawn(async move { wait.enter().await });
    assert_eq!(
        fixture.operation().await,
        OperationKind::RunOffload { file: None }
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(fixture.lifecycle.reserve(fixture.request()).is_err());
    fixture.ack(VmState::Offloaded).await;
    assert_eq!(fixture.operation().await, OperationKind::RunResume);
    assert!(fixture.lifecycle.reserve(fixture.request()).is_err());
    fixture.ack(VmState::Running).await;
    tokio::time::timeout(LIMIT, async {
        loop {
            if let Ok(wait) = fixture.lifecycle.reserve(fixture.request()) {
                drop(wait);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fixture.finish().await;
}

#[tokio::test]
async fn native_model_wait_cancel_never_releases_manual_pause() {
    let mut fixture = Fixture::new(NativeModelWait::Pause).await;
    let mut wait = fixture.enter().await;
    fixture
        .manual(OperationKind::RunOffload { file: None }, VmState::Offloaded)
        .await;
    wait.cancel();
    wait.cancel();
    drop(wait);
    fixture.no_operation().await;
    assert_eq!(fixture.handle.status().state, RunState::Suspended);
    fixture.finish().await;
}

#[tokio::test]
async fn native_model_wait_rejects_stale_identity_parallel_waits_and_capture() {
    let mut fixture = Fixture::new(NativeModelWait::Pause).await;
    let mut stale = fixture.request();
    stale.attempt_id = Some("old-attempt".into());
    assert!(fixture.lifecycle.reserve(stale).is_err());
    let mut missing = fixture.request();
    missing.run_id = None;
    assert!(fixture.lifecycle.reserve(missing).is_err());
    let wait = fixture.reserve();
    assert!(fixture.lifecycle.reserve(fixture.request()).is_err());
    for kind in [
        OperationKind::RunCheckpoint {
            request_id: "capture".into(),
            ram_storage: pvisor_core::operation::SnapshotRamStorage::Raw,
        },
        OperationKind::RunSuspend {
            request_id: "suspend".into(),
            ram_storage: pvisor_core::operation::SnapshotRamStorage::Raw,
        },
    ] {
        assert!(
            fixture
                .handle
                .control(kind)
                .await
                .unwrap_err()
                .to_string()
                .contains("active native model wait")
        );
    }
    drop(wait);
    fixture.no_operation().await;
    fixture.finish().await;
}

#[tokio::test]
async fn native_model_wait_manual_control_between_reserve_and_enter_fences_entry() {
    let mut fixture = Fixture::new(NativeModelWait::Pause).await;
    let mut wait = fixture.reserve();
    fixture
        .manual(OperationKind::RunResume, VmState::Running)
        .await;
    assert!(
        wait.enter()
            .await
            .unwrap_err()
            .to_string()
            .contains("superseded")
    );
    drop(wait);
    fixture.no_operation().await;
    fixture.finish().await;
}

#[tokio::test]
async fn native_model_wait_attempt_cancel_fences_waiting_delivery() {
    let mut fixture = Fixture::new(NativeModelWait::Pause).await;
    let mut wait = fixture.enter().await;
    fixture
        .manual(OperationKind::RunPause, VmState::Paused)
        .await;
    let task = tokio::spawn(async move { wait.before_delivery().await });
    fixture.no_operation().await;
    fixture.handle.cancel();
    assert!(
        tokio::time::timeout(LIMIT, task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(fixture.lifecycle.reserve(fixture.request()).is_err());
    fixture.finish().await;
}

#[tokio::test]
async fn native_model_wait_invalid_native_ack_cancels_attempt() {
    let mut fixture = Fixture::new(NativeModelWait::Pause).await;
    let mut wait = fixture.reserve();
    let task = tokio::spawn(async move { wait.enter().await });
    assert_eq!(fixture.operation().await, OperationKind::RunPause);
    fixture.ack(VmState::Running).await;
    assert!(
        tokio::time::timeout(LIMIT, task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    fixture.finish().await;
}

#[tokio::test]
async fn native_model_wait_cannot_acquire_an_existing_manual_pause() {
    let mut fixture = Fixture::new(NativeModelWait::Pause).await;
    fixture
        .manual(OperationKind::RunPause, VmState::Paused)
        .await;
    let mut wait = fixture.reserve();
    assert!(
        wait.enter()
            .await
            .unwrap_err()
            .to_string()
            .contains("manually paused")
    );
    drop(wait);
    fixture.no_operation().await;
    assert_eq!(fixture.handle.status().state, RunState::Suspended);
    fixture.finish().await;
}

#[tokio::test]
async fn native_model_wait_drop_during_resume_keeps_native_exchange_owned() {
    let mut fixture = Fixture::new(NativeModelWait::Pause).await;
    let mut wait = fixture.enter().await;
    let task = tokio::spawn(async move { wait.before_delivery().await });
    assert_eq!(fixture.operation().await, OperationKind::RunResume);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(fixture.lifecycle.reserve(fixture.request()).is_err());
    fixture.ack(VmState::Running).await;
    fixture.no_operation().await;
    assert_eq!(fixture.handle.status().state, RunState::Running);
    // A later wait has fresh ownership; cleanup from the first cannot release it.
    let mut next = fixture.enter().await;
    next.cancel();
    assert_eq!(fixture.operation().await, OperationKind::RunResume);
    fixture.ack(VmState::Running).await;
    drop(next);
    fixture.finish().await;
}

#[tokio::test]
async fn native_model_wait_failed_cleanup_cancels_instead_of_orphaning_pause() {
    let mut fixture = Fixture::new(NativeModelWait::Pause).await;
    let wait = fixture.enter().await;
    drop(wait);
    assert_eq!(fixture.operation().await, OperationKind::RunResume);
    fixture.ack(VmState::Paused).await;
    tokio::time::timeout(LIMIT, fixture.handle.cancellation().cancelled())
        .await
        .unwrap();
    fixture.finish().await;
}

#[tokio::test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
async fn native_model_wait_driver_configuration_rejects_disabled_and_custom_lifecycles() {
    let fixture = Fixture::new(NativeModelWait::Pause).await;
    let config =
        pvisor_gateway::config::ProxyConfig::from_toml_str("listen = '127.0.0.1:0'\nmodels = []")
            .unwrap();
    let ordinary = crate::GatewayDriverConfig::new(config);
    assert_eq!(ordinary.native_model_wait, None);
    let native = ordinary.native_model_wait(NativeModelWait::Pause);
    let bind = |config| {
        crate::runtime::RuntimeSupervisorBuilder::new()
            .gateway(config)
            .build()
            .bind_model_wait(fixture.handle.controls())
    };
    assert!(bind(native.clone()).is_ok());
    assert!(bind(native.clone().gateway_enabled(false)).is_err());
    assert!(
        bind(native.model_wait(Arc::new(NativeLifecycle::new(
            fixture.handle.controls(),
            NativeModelWait::Pause,
        ))))
        .is_err()
    );
    fixture.finish().await;
}
