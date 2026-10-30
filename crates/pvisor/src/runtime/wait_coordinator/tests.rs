use super::*;
use pvisor_core::operation::{VmMemory, VmState};
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::sync::Semaphore;

pub(super) struct FakeControl {
    state: SyncMutex<RunState>,
    calls: watch::Sender<Vec<&'static str>>,
    gate: SyncMutex<Option<(&'static str, Arc<Semaphore>)>>,
    fail: SyncMutex<Option<&'static str>>,
    offload: AtomicBool,
    idle: AtomicBool,
    rearm: AtomicBool,
    idle_offload_allowed: AtomicBool,
    idle_wake: SyncMutex<Option<CancellationToken>>,
    idle_epoch: AtomicU64,
    host_reads: AtomicU64,
}

impl FakeControl {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: SyncMutex::new(RunState::Running),
            calls: watch::channel(Vec::new()).0,
            gate: SyncMutex::new(None),
            fail: SyncMutex::new(None),
            offload: AtomicBool::new(true),
            idle: AtomicBool::new(false),
            rearm: AtomicBool::new(false),
            idle_offload_allowed: AtomicBool::new(true),
            idle_wake: SyncMutex::new(None),
            idle_epoch: AtomicU64::new(0),
            host_reads: AtomicU64::new(0),
        })
    }
    fn block(&self, operation: &'static str) -> Arc<Semaphore> {
        let gate = Arc::new(Semaphore::new(0));
        *self.gate.lock().unwrap() = Some((operation, gate.clone()));
        gate
    }
    async fn called(&self, count: usize) {
        let mut calls = self.calls.subscribe();
        tokio::time::timeout(Duration::from_secs(10), async {
            while calls.borrow_and_update().len() < count {
                calls.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }
    fn recorded(&self) -> Vec<&'static str> {
        self.calls.borrow().clone()
    }
}

#[async_trait]
impl WaitControl for FakeControl {
    fn supports_resume_rearm(&self) -> bool {
        self.rearm.load(Ordering::Acquire)
    }
    async fn idle_resume_rearm(
        &self,
        epoch: u64,
        wake: CancellationToken,
    ) -> anyhow::Result<tokio::sync::oneshot::Receiver<Option<u64>>> {
        assert_eq!(epoch, self.idle_epoch.load(Ordering::Acquire));
        let next = self.idle_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        *self.idle_wake.lock().unwrap() = Some(wake.clone());
        self.calls.send_modify(|calls| calls.push("arm.next"));
        self.execute(OperationKind::RunResume).await?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        tx.send((!wake.is_cancelled()).then_some(next)).unwrap();
        Ok(rx)
    }
    fn pending_host_reads(&self) -> u64 {
        self.host_reads.load(Ordering::Acquire)
    }
    fn state(&self) -> RunState {
        *self.state.lock().unwrap()
    }
    async fn idle_permit(&self, wake: CancellationToken) -> anyhow::Result<Option<u64>> {
        *self.idle_wake.lock().unwrap() = Some(wake);
        Ok(self
            .idle
            .load(Ordering::Relaxed)
            .then(|| self.idle_epoch.fetch_add(1, Ordering::SeqCst) + 1))
    }
    async fn idle_pause(&self, epoch: u64) -> anyhow::Result<bool> {
        assert_eq!(epoch, self.idle_epoch.load(Ordering::SeqCst));
        if !self.idle.load(Ordering::Relaxed) {
            return Ok(false);
        }
        self.execute(OperationKind::RunPause).await?;
        Ok(true)
    }
    async fn idle_offload(&self, epoch: u64) -> anyhow::Result<bool> {
        assert_eq!(epoch, self.idle_epoch.load(Ordering::SeqCst));
        assert_eq!(self.state(), RunState::Suspended);
        if !self.idle_offload_allowed.load(Ordering::Relaxed) {
            return Ok(false);
        }
        self.execute(OperationKind::RunOffload { file: None })
            .await?;
        Ok(true)
    }
    fn can_offload(&self) -> bool {
        self.offload.load(Ordering::Relaxed)
    }
    async fn execute(&self, operation: OperationKind) -> anyhow::Result<Value> {
        let name = operation.name();
        self.calls.send_modify(|calls| calls.push(name));
        let gate = self
            .gate
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(blocked, _)| *blocked == name)
            .map(|(_, gate)| gate.clone());
        if let Some(gate) = gate {
            gate.acquire().await.unwrap().forget();
        }
        anyhow::ensure!(
            *self.fail.lock().unwrap() != Some(name),
            "uncertain native transition"
        );
        let state = match operation {
            OperationKind::RunPause => VmState::Paused,
            OperationKind::RunOffload { .. } => VmState::Offloaded,
            OperationKind::RunResume => VmState::Running,
            _ => anyhow::bail!("unsupported fake operation"),
        };
        *self.state.lock().unwrap() = if state == VmState::Running {
            RunState::Running
        } else {
            RunState::Suspended
        };
        Ok(Value::Vm {
            state,
            memory: (state == VmState::Offloaded).then(|| VmMemory {
                backing_file: "/test/ram".into(),
                backed_bytes: 4096,
                resident_before_bytes: Some(4096),
                resident_after_bytes: Some(0),
            }),
        })
    }
}

fn fixture() -> (Arc<WaitCoordinator>, Arc<FakeControl>) {
    let control = FakeControl::new();
    let coordinator = WaitCoordinator::new(
        control.clone(),
        CancellationToken::new(),
        CooperativeModelWait {
            require_guest_idle: false,
            downloads: false,
            filesystem_reads: false,
            stream_buffer: None,
            rounds: None,
            pause_after: Duration::from_millis(100),
            offload_after: Some(Duration::from_secs(1)),
        },
    );
    (coordinator, control)
}

async fn entered(coordinator: &Arc<WaitCoordinator>) -> Arc<WaitEntry> {
    let entry = coordinator.reserve().unwrap();
    entry.enter();
    tokio::task::yield_now().await;
    entry
}

#[tokio::test(start_paused = true)]
async fn fast_reply_and_cancel_before_entry_never_pause() {
    let (coordinator, control) = fixture();
    let entry = entered(&coordinator).await;
    entry.before_delivery().await.unwrap();
    let cancelled = coordinator.reserve().unwrap();
    cancelled.cancel();
    cancelled.cleanup_completed().await.unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(control.recorded().is_empty());
}

#[tokio::test(start_paused = true)]
async fn early_startup_call_forwards_without_issuing_unready_controls() {
    let (coordinator, control) = fixture();
    *control.state.lock().unwrap() = RunState::Starting;
    let entry = entered(&coordinator).await;
    tokio::time::advance(Duration::from_secs(2)).await;
    entry.before_delivery().await.unwrap();
    assert!(control.recorded().is_empty());
}

#[tokio::test(start_paused = true)]
async fn long_wait_pauses_offloads_and_resumes_before_delivery() {
    let (coordinator, control) = fixture();
    let entry = entered(&coordinator).await;
    control.called(2).await;
    entry.before_delivery().await.unwrap();
    assert_eq!(
        control.recorded(),
        ["run.pause", "run.offload", "run.resume"]
    );
    assert_eq!(control.state(), RunState::Running);
}

#[tokio::test(start_paused = true)]
async fn response_during_offload_waits_for_commit_then_resume_ack() {
    let (coordinator, control) = fixture();
    let gate = control.block("run.offload");
    let entry = entered(&coordinator).await;
    control.called(2).await;
    let delivery = tokio::spawn(async move { entry.before_delivery().await });
    tokio::task::yield_now().await;
    assert!(!delivery.is_finished());
    assert_eq!(control.recorded(), ["run.pause", "run.offload"]);
    let resume = control.block("run.resume");
    gate.add_permits(1);
    control.called(3).await;
    assert!(!delivery.is_finished());
    resume.add_permits(1);
    delivery.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn human_pause_blocks_delivery_until_explicit_resume() {
    let (coordinator, control) = fixture();
    let entry = entered(&coordinator).await;
    control.called(1).await;
    coordinator.control(OperationKind::RunPause).await.unwrap();
    let delivery = tokio::spawn(async move { entry.before_delivery().await });
    tokio::task::yield_now().await;
    assert!(!delivery.is_finished());
    coordinator.control(OperationKind::RunResume).await.unwrap();
    delivery.await.unwrap().unwrap();
    assert_eq!(control.recorded(), ["run.pause", "run.pause", "run.resume"]);
}

#[tokio::test(start_paused = true)]
async fn delivery_rechecks_human_pause_after_early_manual_resume() {
    let (coordinator, control) = fixture();
    let entry = entered(&coordinator).await;
    control.called(1).await;
    coordinator.control(OperationKind::RunResume).await.unwrap();
    entry.cleanup_completed().await.unwrap();
    // The supplier still has not replied: the call slot must remain reserved.
    assert!(coordinator.reserve().is_err());
    coordinator.control(OperationKind::RunPause).await.unwrap();
    let delivery = tokio::spawn(async move { entry.before_delivery().await });
    tokio::task::yield_now().await;
    assert!(!delivery.is_finished());
    coordinator.control(OperationKind::RunResume).await.unwrap();
    delivery.await.unwrap().unwrap();
    assert_eq!(
        control.recorded(),
        ["run.pause", "run.resume", "run.pause", "run.resume"]
    );
}

#[tokio::test(start_paused = true)]
async fn delivery_rechecks_termination_after_early_manual_resume() {
    let (coordinator, control) = fixture();
    let entry = entered(&coordinator).await;
    control.called(1).await;
    coordinator.control(OperationKind::RunResume).await.unwrap();
    entry.cleanup_completed().await.unwrap();
    coordinator.cancellation.cancel();
    assert!(entry.before_delivery().await.is_err());
    assert_eq!(control.recorded(), ["run.pause", "run.resume"]);
}

#[tokio::test(start_paused = true)]
async fn explicit_resume_invalidates_old_timers_and_stale_cancel_cannot_wake_new_wait() {
    let (coordinator, control) = fixture();
    let old = entered(&coordinator).await;
    control.called(1).await;
    coordinator.control(OperationKind::RunResume).await.unwrap();
    old.before_delivery().await.unwrap();
    let next = entered(&coordinator).await;
    old.cancel();
    control.called(4).await;
    next.before_delivery().await.unwrap();
    assert_eq!(
        control.recorded(),
        [
            "run.pause",
            "run.resume",
            "run.pause",
            "run.offload",
            "run.resume"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn cancelled_delivery_future_does_not_strand_paused_vm() {
    let (coordinator, control) = fixture();
    let gate = control.block("run.offload");
    let entry = entered(&coordinator).await;
    control.called(2).await;
    let task_entry = entry.clone();
    let delivery = tokio::spawn(async move { task_entry.before_delivery().await });
    tokio::task::yield_now().await;
    delivery.abort();
    entry.cancel();
    gate.add_permits(1);
    entry.cleanup_completed().await.unwrap();
    assert_eq!(
        control.recorded(),
        ["run.pause", "run.offload", "run.resume"]
    );
}

#[tokio::test(start_paused = true)]
async fn cancellation_during_offload_finishes_transition_without_resuming_attempt() {
    let (coordinator, control) = fixture();
    let gate = control.block("run.offload");
    let entry = entered(&coordinator).await;
    control.called(2).await;
    coordinator.cancellation.cancel();
    gate.add_permits(1);
    assert!(entry.before_delivery().await.is_err());
    assert_eq!(control.recorded(), ["run.pause", "run.offload"]);
    assert!(coordinator.reserve().is_err());
}

#[tokio::test(start_paused = true)]
async fn unsupported_offload_keeps_pause_only_and_overlap_wakes_existing_wait() {
    let (coordinator, control) = fixture();
    control.offload.store(false, Ordering::Relaxed);
    let entry = entered(&coordinator).await;
    control.called(1).await;
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(coordinator.reserve().is_err());
    entry.before_delivery().await.unwrap();
    assert_eq!(control.recorded(), ["run.pause", "run.resume"]);
}

#[tokio::test(start_paused = true)]
async fn uncertain_pause_offload_or_resume_cancels_attempt_without_delivery() {
    for failed in ["run.pause", "run.offload", "run.resume"] {
        let (coordinator, control) = fixture();
        *control.fail.lock().unwrap() = Some(failed);
        let entry = entered(&coordinator).await;
        control
            .called(if failed == "run.pause" { 1 } else { 2 })
            .await;
        assert!(entry.before_delivery().await.is_err());
        assert!(coordinator.cancellation.is_cancelled());
    }
}

#[tokio::test(start_paused = true)]
async fn manual_control_survives_caller_cancellation() {
    let (coordinator, control) = fixture();
    let gate = control.block("run.pause");
    let caller_coordinator = coordinator.clone();
    let caller =
        tokio::spawn(async move { caller_coordinator.control(OperationKind::RunPause).await });
    control.called(1).await;
    caller.abort();
    gate.add_permits(1);
    coordinator.control(OperationKind::RunResume).await.unwrap();
    assert_eq!(control.state(), RunState::Running);
    assert_eq!(control.recorded(), ["run.pause", "run.resume"]);
}

#[tokio::test(start_paused = true)]
async fn snapshot_conflict_does_not_override_wait_ownership() {
    let (coordinator, control) = fixture();
    let entry = entered(&coordinator).await;
    let result = coordinator
        .control(OperationKind::RunCheckpoint {
            request_id: "capture".into(),
            ram_storage: pvisor_core::operation::SnapshotRamStorage::Raw,
        })
        .await;
    assert!(result.is_err());
    assert!(!coordinator.cancellation.is_cancelled());
    entry.before_delivery().await.unwrap();
    assert!(control.recorded().is_empty());
}

#[tokio::test(start_paused = true)]
async fn gateway_adapter_binds_attempt_and_cancel_uses_same_resume_barrier() {
    use crate::runtime::model_wait::NativeModelWait;
    use pvisor_gateway::model_wait::ModelWaitLifecycle;
    let (coordinator, control) = fixture();
    let adapter = NativeModelWait {
        coordinator,
        attempt_id: pvisor_core::AttemptId::new("attempt-a"),
    };
    let mut request = pvisor_core::ModelCallRequest {
        run_id: None,
        attempt_id: Some(pvisor_core::AttemptId::new("attempt-b")),
        call_id: "call".into(),
        client_model: "test".into(),
        upstream_model: "test".into(),
        provider: "openai".into(),
        protocol: "chat_completions".into(),
        upstream_host: "localhost".into(),
    };
    assert!(adapter.reserve(request.clone()).is_err());
    request.attempt_id = Some(pvisor_core::AttemptId::new("attempt-a"));
    let mut wait = adapter.reserve(request).unwrap();
    wait.enter().await.unwrap();
    control.called(1).await;
    wait.cancel();
    control.called(2).await;
    assert!(wait.before_delivery().await.is_err());
    assert_eq!(control.recorded(), ["run.pause", "run.resume"]);
}

#[tokio::test(start_paused = true)]
async fn response_during_pause_waits_for_pause_ack_then_resumes() {
    let (coordinator, control) = fixture();
    let gate = control.block("run.pause");
    let entry = entered(&coordinator).await;
    control.called(1).await;
    let delivery = tokio::spawn(async move { entry.before_delivery().await });
    tokio::task::yield_now().await;
    assert!(!delivery.is_finished());
    gate.add_permits(1);
    delivery.await.unwrap().unwrap();
    assert_eq!(control.recorded(), ["run.pause", "run.resume"]);
}

#[tokio::test(start_paused = true)]
async fn cancellation_unblocks_delivery_held_by_human_pause() {
    let (coordinator, control) = fixture();
    let entry = entered(&coordinator).await;
    coordinator.control(OperationKind::RunPause).await.unwrap();
    coordinator.cancellation.cancel();
    assert!(entry.before_delivery().await.is_err());
    assert_eq!(control.recorded(), ["run.pause"]);
}

#[tokio::test]
async fn actual_gateway_http_delivery_waits_for_native_adapter_resume() {
    use crate::runtime::model_wait::NativeModelWait;
    use axum::{
        Router,
        body::Body,
        http::{HeaderMap, Response},
    };
    use pvisor_gateway::{
        config::ProxyConfig,
        runtime::in_process::{InProcessCapture, InProcessRuntime},
        sink::NoopCaptureObserver,
    };
    for streaming in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let ready = Arc::new(tokio::sync::Notify::new());
        let respond = ready.clone();
        let app = Router::new().fallback(move |headers: HeaderMap| {
            let respond = respond.clone();
            async move {
                assert!(!headers.contains_key("x-pvisor-inference-idle"));
                respond.notified().await;
                let body = if streaming {
                    "data: {\"id\":\"r\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
                } else {
                    "{\"id\":\"r\",\"object\":\"chat.completion\",\"model\":\"test\",\"choices\":[{\"index\":0,\"message\":{\"role\":\"assistant\",\"content\":\"hello\"},\"finish_reason\":\"stop\"}]}"
                };
                Response::builder().header("content-type", if streaming { "text/event-stream" } else { "application/json" })
                    .body(Body::from(body)).unwrap()
            }
        });
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let control = FakeControl::new();
        control.idle.store(true, Ordering::Relaxed);
        let gate = control.block("run.resume");
        let coordinator = WaitCoordinator::new(
            control.clone(),
            CancellationToken::new(),
            CooperativeModelWait {
                require_guest_idle: true,
                downloads: false,
                filesystem_reads: false,
                stream_buffer: None,
                rounds: None,
                pause_after: Duration::from_millis(10),
                offload_after: Some(Duration::from_millis(20)),
            },
        );
        let adapter = Arc::new(NativeModelWait {
            coordinator,
            attempt_id: pvisor_core::AttemptId::new("http-attempt"),
        });
        let config = ProxyConfig::from_toml_str(&format!(
            r#"
listen = "127.0.0.1:0"
admin_listen = "127.0.0.1:0"
[[models]]
name = "test"
upstream = "http://{address}/v1"
provider = "openai"
api_key = "test-key"
"#
        ))
        .unwrap();
        let storage = tempfile::tempdir().unwrap();
        // This test owns the HTTP client. Supply its normal run metadata rather
        // than making the delivery barrier depend on a host-wide lsof scan.
        pvisor_gateway::runtime::run_env::write_run_session(storage.path(), "http-session")
            .unwrap();
        pvisor_gateway::runtime::run_env::write_run_child_info(
            storage.path(),
            std::process::id(),
            &["gateway-resume-test".into()],
        )
        .unwrap();
        let gateway = InProcessCapture::start_with_runtime(
            config,
            storage.path().into(),
            Arc::new(NoopCaptureObserver::new()),
            InProcessRuntime {
                attempt_id: Some("http-attempt".into()),
                model_wait: Some(adapter),
                ..Default::default()
            },
        )
        .unwrap();
        let url = format!("http://{}/v1/chat/completions", gateway.listen);
        let client = tokio::spawn(async move {
            reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(10)).build().unwrap()
                .post(url)
                .json(&serde_json::json!({"model":"test","stream":streaming,"messages":[{"role":"user","content":"hi"}]}))
                .send().await.unwrap()
        });
        control.called(2).await;
        ready.notify_one();
        control.called(3).await;
        assert!(
            !client.is_finished(),
            "Gateway exposed HTTP headers before resume acknowledgement"
        );
        gate.add_permits(1);
        let response = client.await.unwrap();
        assert!(response.status().is_success());
        let body = response
            .text()
            .await
            .unwrap_or_else(|error| panic!("streaming={streaming}: {error:?}"));
        assert!(body.contains("hello"));
        assert_eq!(
            control.recorded(),
            ["run.pause", "run.offload", "run.resume"]
        );
        gateway.shutdown().unwrap();
        server.abort();
    }
}

#[tokio::test]
async fn gateway_runtime_shutdown_cannot_abort_owned_cleanup() {
    let (coordinator, control) = fixture();
    let (entry_tx, entry_rx) = tokio::sync::oneshot::channel();
    let thread = std::thread::spawn(move || {
        let gateway_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        gateway_runtime.block_on(async move {
            let entry = coordinator.reserve().unwrap();
            entry.enter();
            entry_tx.send(entry).ok().unwrap();
        });
        // The per-Gateway runtime is dropped with an outstanding wait.
    });
    let entry = entry_rx.await.unwrap();
    thread.join().unwrap();
    control.called(1).await;
    entry.cancel();
    entry.cleanup_completed().await.unwrap();
    assert_eq!(control.recorded(), ["run.pause", "run.resume"]);
}

fn idle_fixture() -> (Arc<WaitCoordinator>, Arc<FakeControl>) {
    let control = FakeControl::new();
    let coordinator = WaitCoordinator::new(
        control.clone(),
        CancellationToken::new(),
        CooperativeModelWait {
            require_guest_idle: true,
            downloads: false,
            filesystem_reads: false,
            stream_buffer: None,
            rounds: None,
            pause_after: Duration::ZERO,
            offload_after: None,
        },
    );
    (coordinator, control)
}
#[tokio::test]
async fn absent_guest_idle_evidence_never_pauses() {
    let (coordinator, control) = idle_fixture();
    let entry = coordinator.reserve().unwrap();
    entry.enter();
    entry.cleanup_completed().await.unwrap();
    entry.before_delivery().await.unwrap();
    assert!(control.recorded().is_empty());
}
#[tokio::test]
async fn external_wake_resumes_before_model_delivery() {
    let (coordinator, control) = idle_fixture();
    control.idle.store(true, Ordering::Relaxed);
    let entry = coordinator.reserve().unwrap();
    entry.enter();
    control.called(1).await;
    control.idle_wake.lock().unwrap().as_ref().unwrap().cancel();
    entry.cleanup_completed().await.unwrap();
    assert_eq!(control.recorded(), vec!["run.pause", "run.resume"]);
    assert!(
        coordinator.reserve().is_err(),
        "HTTP call still owns serial slot"
    );
    entry.before_delivery().await.unwrap();
}
#[tokio::test]
async fn wake_during_native_idle_pause_is_latched_until_ack() {
    let (coordinator, control) = idle_fixture();
    control.idle.store(true, Ordering::Relaxed);
    let pause = control.block("run.pause");
    let entry = coordinator.reserve().unwrap();
    entry.enter();
    control.called(1).await;
    control.idle_wake.lock().unwrap().as_ref().unwrap().cancel();
    assert_eq!(control.recorded(), vec!["run.pause"]);
    pause.add_permits(1);
    entry.cleanup_completed().await.unwrap();
    assert_eq!(control.recorded(), vec!["run.pause", "run.resume"]);
    entry.before_delivery().await.unwrap();
}

fn idle_offload_fixture() -> (Arc<WaitCoordinator>, Arc<FakeControl>) {
    let control = FakeControl::new();
    control.idle.store(true, Ordering::Relaxed);
    let coordinator = WaitCoordinator::new(
        control.clone(),
        CancellationToken::new(),
        CooperativeModelWait {
            require_guest_idle: true,
            downloads: false,
            filesystem_reads: false,
            stream_buffer: None,
            rounds: None,
            pause_after: Duration::from_millis(10),
            offload_after: Some(Duration::from_millis(30)),
        },
    );
    (coordinator, control)
}

#[tokio::test(start_paused = true)]
async fn guest_idle_wake_during_offload_waits_for_commit_and_resume() {
    for response in [false, true] {
        let (coordinator, control) = idle_offload_fixture();
        let commit = control.block("run.offload");
        let entry = entered(&coordinator).await;
        control.called(2).await;
        let delivery = if response {
            let e = entry.clone();
            Some(tokio::spawn(async move { e.before_delivery().await }))
        } else {
            control.idle_wake.lock().unwrap().as_ref().unwrap().cancel();
            None
        };
        tokio::task::yield_now().await;
        assert_eq!(control.recorded(), ["run.pause", "run.offload"]);
        let resumed = control.block("run.resume");
        commit.add_permits(1);
        control.called(3).await;
        if let Some(delivery) = &delivery {
            assert!(!delivery.is_finished());
        }
        resumed.add_permits(1);
        if let Some(delivery) = delivery {
            delivery.await.unwrap().unwrap();
        } else {
            entry.cleanup_completed().await.unwrap();
            assert!(
                coordinator.reserve().is_err(),
                "model call still outstanding"
            );
            entry.before_delivery().await.unwrap();
        }
        assert_eq!(
            control.recorded(),
            ["run.pause", "run.offload", "run.resume"]
        );
    }
}

#[tokio::test(start_paused = true)]
async fn guest_idle_wake_before_offload_skips_reclaim() {
    let (coordinator, control) = idle_offload_fixture();
    let pause = control.block("run.pause");
    let entry = entered(&coordinator).await;
    control.called(1).await;
    control.idle_wake.lock().unwrap().as_ref().unwrap().cancel();
    pause.add_permits(1);
    entry.cleanup_completed().await.unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    entry.before_delivery().await.unwrap();
    assert_eq!(control.recorded(), ["run.pause", "run.resume"]);
}

#[tokio::test(start_paused = true)]
async fn guest_idle_offload_denial_keeps_pause_until_real_wake_or_delivery() {
    for external in [true, false] {
        let (coordinator, control) = idle_offload_fixture();
        control.idle_offload_allowed.store(false, Ordering::Relaxed);
        let entry = entered(&coordinator).await;
        control.called(1).await;
        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
        assert_eq!(control.recorded(), ["run.pause"]);
        assert_eq!(control.state(), RunState::Suspended);
        assert!(coordinator.state.lock().await.next_offload_at.is_none());
        if external {
            control.idle_wake.lock().unwrap().as_ref().unwrap().cancel();
            entry.cleanup_completed().await.unwrap();
        }
        entry.before_delivery().await.unwrap();
        assert_eq!(control.recorded(), ["run.pause", "run.resume"]);
        assert!(!coordinator.cancellation.is_cancelled());
    }
}

#[tokio::test(start_paused = true)]
async fn guest_idle_offload_needs_evidence_and_supported_backing() {
    for evidence in [false, true] {
        let (coordinator, control) = idle_offload_fixture();
        control.idle.store(evidence, Ordering::Relaxed);
        control.offload.store(false, Ordering::Relaxed);
        let entry = entered(&coordinator).await;
        if evidence {
            control.called(1).await;
            tokio::time::advance(Duration::from_secs(2)).await;
        } else {
            entry.cleanup_completed().await.unwrap();
        }
        entry.before_delivery().await.unwrap();
        assert_eq!(
            control.recorded(),
            if evidence {
                vec!["run.pause", "run.resume"]
            } else {
                vec![]
            }
        );
    }
}

#[tokio::test(start_paused = true)]
async fn guest_idle_offload_failure_or_termination_never_resumes() {
    for fail in [false, true] {
        let (coordinator, control) = idle_offload_fixture();
        let commit = control.block("run.offload");
        let entry = entered(&coordinator).await;
        control.called(2).await;
        if fail {
            *control.fail.lock().unwrap() = Some("run.offload");
        } else {
            coordinator.cancellation.cancel();
        }
        commit.add_permits(1);
        assert!(entry.cleanup_completed().await.is_err());
        assert!(coordinator.cancellation.is_cancelled());
        assert_eq!(control.recorded(), ["run.pause", "run.offload"]);
    }
}

#[tokio::test(start_paused = true)]
async fn manual_pause_after_guest_idle_offload_retains_delivery_ownership() {
    let (coordinator, control) = idle_offload_fixture();
    let entry = entered(&coordinator).await;
    control.called(2).await;
    coordinator.control(OperationKind::RunPause).await.unwrap();
    let delivery = tokio::spawn(async move { entry.before_delivery().await });
    tokio::task::yield_now().await;
    assert!(!delivery.is_finished());
    coordinator.control(OperationKind::RunResume).await.unwrap();
    delivery.await.unwrap().unwrap();
    assert_eq!(
        control.recorded(),
        ["run.pause", "run.offload", "run.pause", "run.resume"]
    );
}

#[test]
fn guest_idle_policy_allows_offload_but_retains_timer_ordering() {
    let mut policy = CooperativeModelWait {
        require_guest_idle: true,
        downloads: false,
        filesystem_reads: false,
        stream_buffer: None,
        rounds: None,
        pause_after: Duration::from_millis(20),
        offload_after: Some(Duration::from_millis(50)),
    };
    policy.validate().unwrap();
    policy.offload_after = Some(Duration::from_millis(10));
    assert!(policy.validate().is_err());
    assert!(CooperativeModelWait::default().offload_after.is_none());
}

#[tokio::test(start_paused = true)]
async fn host_observed_overlap_forwards_after_cleanup_and_disables_future_pauses() {
    use crate::runtime::model_wait::NativeModelWait;
    use pvisor_gateway::model_wait::ModelWaitLifecycle;
    let (coordinator, control) = idle_offload_fixture();
    let adapter = NativeModelWait {
        coordinator: coordinator.clone(),
        attempt_id: pvisor_core::AttemptId::new("host-observed"),
    };
    let request = pvisor_core::ModelCallRequest {
        run_id: None,
        attempt_id: Some(pvisor_core::AttemptId::new("host-observed")),
        call_id: "one".into(),
        client_model: "test".into(),
        upstream_model: "test".into(),
        provider: "openai".into(),
        protocol: "chat_completions".into(),
        upstream_host: "localhost".into(),
    };
    let commit = control.block("run.offload");
    let mut first = adapter.reserve(request.clone()).unwrap();
    first.enter().await.unwrap();
    control.called(2).await;
    let mut second = adapter.reserve(request.clone()).unwrap();
    second.enter().await.unwrap();
    let second_delivery = tokio::spawn(async move { second.before_delivery().await });
    tokio::task::yield_now().await;
    assert!(!second_delivery.is_finished());
    assert_eq!(control.recorded(), ["run.pause", "run.offload"]);
    commit.add_permits(1);
    second_delivery.await.unwrap().unwrap();
    first.before_delivery().await.unwrap();
    let mut third = adapter.reserve(request).unwrap();
    third.enter().await.unwrap();
    assert!(
        coordinator
            .control(OperationKind::RunCheckpoint {
                request_id: "during-passive-wait".into(),
                ram_storage: pvisor_core::operation::SnapshotRamStorage::Raw,
            })
            .await
            .is_err()
    );
    tokio::time::advance(Duration::from_secs(5)).await;
    third.before_delivery().await.unwrap();
    drop(third);
    assert_eq!(coordinator.passive_waits.load(Ordering::Acquire), 0);
    assert_eq!(
        control.recorded(),
        ["run.pause", "run.offload", "run.resume"]
    );
}

#[tokio::test]
async fn passive_host_wait_retains_manual_pause_and_drop_releases_observation() {
    let (coordinator, control) = idle_fixture();
    let first = coordinator.observe().unwrap().unwrap();
    assert!(coordinator.observe().unwrap().is_none());
    first.cancel();
    first.cleanup_completed().await.unwrap();
    coordinator.control(OperationKind::RunPause).await.unwrap();
    let c = coordinator.clone();
    let delivery = tokio::spawn(async move { c.passive_delivery().await });
    tokio::task::yield_now().await;
    assert!(!delivery.is_finished());
    coordinator.control(OperationKind::RunResume).await.unwrap();
    delivery.await.unwrap().unwrap();
    coordinator.finish_passive();
    assert_eq!(control.recorded(), ["run.pause", "run.resume"]);
}

#[tokio::test(start_paused = true)]
async fn stream_wake_is_visible_during_offload_but_delivery_waits_for_publication() {
    let (mut coordinator, control) = idle_offload_fixture();
    Arc::get_mut(&mut coordinator).unwrap().policy.stream_buffer = Some(crate::ModelStreamBuffer {
        max_bytes: 4096,
        max_wait: Duration::from_secs(1),
    });
    let commit = control.block("run.offload");
    let entry = entered(&coordinator).await;
    assert_eq!(entry.stream_buffer().unwrap().max_bytes, 4096);
    control.called(2).await;
    control.idle_wake.lock().unwrap().as_ref().unwrap().cancel();
    entry.wait_for_wake().await;
    let delivery = tokio::spawn(async move { entry.before_delivery().await });
    tokio::task::yield_now().await;
    assert!(!delivery.is_finished());
    assert_eq!(control.recorded(), ["run.pause", "run.offload"]);
    let resume = control.block("run.resume");
    commit.add_permits(1);
    control.called(3).await;
    assert!(!delivery.is_finished());
    resume.add_permits(1);
    delivery.await.unwrap().unwrap();
}

#[test]
fn native_model_wait_validates_stream_buffer_at_host_configuration() {
    let mut policy = CooperativeModelWait::default();
    assert!(policy.stream_buffer.is_none());
    policy.stream_buffer = Some(crate::ModelStreamBuffer {
        max_bytes: 4096,
        max_wait: Duration::from_millis(100),
    });
    policy.validate().unwrap();
    policy.stream_buffer.as_mut().unwrap().max_bytes = 0;
    assert!(policy.validate().is_err());
}

fn rounds_fixture() -> (Arc<WaitCoordinator>, Arc<FakeControl>) {
    let (mut coordinator, control) = idle_offload_fixture();
    Arc::get_mut(&mut coordinator).unwrap().policy.rounds = Some(crate::ModelWaitRounds {
        recheck_after: Duration::from_millis(100),
        offload_cooldown: Duration::from_secs(5),
    });
    (coordinator, control)
}

#[tokio::test(start_paused = true)]
async fn wake_rearms_with_fresh_evidence_and_old_wake_cannot_end_new_round() {
    let (mut coordinator, control) = rounds_fixture();
    Arc::get_mut(&mut coordinator).unwrap().policy.offload_after = None;
    let entry = entered(&coordinator).await;
    control.called(1).await;
    let old_round = entry.current_round();
    let old_wake = control.idle_wake.lock().unwrap().clone().unwrap();
    old_wake.cancel(); // Both timer and external-event notification use this latch.
    old_round.completed().await.unwrap();
    assert_eq!(control.recorded(), ["run.pause", "run.resume"]);
    control.called(3).await;
    assert_eq!(control.idle_epoch.load(Ordering::SeqCst), 2);
    assert!(entry.current_round().generation > old_round.generation);
    old_wake.cancel();
    tokio::task::yield_now().await;
    assert!(!entry.current_round().wake.is_cancelled());
    assert_eq!(control.state(), RunState::Suspended);
    entry.before_delivery().await.unwrap();
    assert_eq!(
        control.recorded(),
        ["run.pause", "run.resume", "run.pause", "run.resume"]
    );
}

#[tokio::test(start_paused = true)]
async fn batch_hold_blocks_rearming_until_drain_then_rechecks() {
    let (mut coordinator, control) = rounds_fixture();
    Arc::get_mut(&mut coordinator).unwrap().policy.offload_after = None;
    let entry = entered(&coordinator).await;
    control.called(1).await;
    let mut cursor = entry.before_batch().await.unwrap();
    tokio::time::advance(Duration::from_secs(10)).await;
    assert_eq!(control.recorded(), ["run.pause", "run.resume"]);
    entry.after_batch();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            entry.wait_for_round_wake(&mut cursor)
        )
        .await
        .is_err()
    );
    assert_eq!(control.idle_epoch.load(Ordering::SeqCst), 1);
    control.called(3).await;
    control.idle_wake.lock().unwrap().as_ref().unwrap().cancel();
    entry.wait_for_round_wake(&mut cursor).await;
    assert_eq!(cursor, 2);
    entry.before_delivery().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn offload_cooldown_spans_rounds_and_requests_without_disabling_pause() {
    let (coordinator, control) = rounds_fixture();
    let first = entered(&coordinator).await;
    control.called(2).await;
    first.current_round().wake.cancel();
    control.called(4).await; // resume, second pause
    tokio::time::advance(Duration::from_millis(40)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        control
            .recorded()
            .iter()
            .filter(|c| **c == "run.offload")
            .count(),
        1
    );
    first.before_delivery().await.unwrap();
    let second = entered(&coordinator).await;
    control.called(6).await; // third pause, new request
    tokio::time::advance(Duration::from_millis(40)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        control
            .recorded()
            .iter()
            .filter(|c| **c == "run.offload")
            .count(),
        1
    );
    second.before_batch().await.unwrap();
    tokio::time::advance(Duration::from_secs(5)).await;
    second.after_batch();
    control.called(9).await; // resume, fourth pause, second offload
    assert_eq!(
        control
            .recorded()
            .iter()
            .filter(|c| **c == "run.offload")
            .count(),
        2
    );
    second.before_delivery().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn stream_batch_waits_for_inflight_offload_and_resume_before_delivery() {
    let (coordinator, control) = rounds_fixture();
    let commit = control.block("run.offload");
    let entry = entered(&coordinator).await;
    control.called(2).await;
    let e = entry.clone();
    let batch = tokio::spawn(async move { e.before_batch().await });
    tokio::task::yield_now().await;
    assert!(!batch.is_finished());
    commit.add_permits(1);
    batch.await.unwrap().unwrap();
    assert_eq!(
        control.recorded(),
        ["run.pause", "run.offload", "run.resume"]
    );
    tokio::time::advance(Duration::from_secs(10)).await;
    assert_eq!(control.recorded().len(), 3);
    entry.cancel();
    entry.cleanup_completed().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn manual_resume_permanently_ends_automatic_rounds_for_this_request() {
    let (coordinator, control) = rounds_fixture();
    let entry = entered(&coordinator).await;
    control.called(2).await;
    coordinator.control(OperationKind::RunResume).await.unwrap();
    entry.cleanup_completed().await.unwrap();
    tokio::time::advance(Duration::from_secs(10)).await;
    assert_eq!(
        control.recorded(),
        ["run.pause", "run.offload", "run.resume"]
    );
    entry.before_delivery().await.unwrap();
}

#[test]
fn repeated_wait_policy_has_bounded_rechecks_and_cooldown() {
    let mut policy = CooperativeModelWait::default();
    policy.validate().unwrap();
    policy.rounds.as_mut().unwrap().recheck_after = Duration::ZERO;
    assert!(policy.validate().is_err());
    policy.rounds = Some(crate::ModelWaitRounds::default());
    policy.rounds.as_mut().unwrap().offload_cooldown = Duration::from_millis(1);
    assert!(policy.validate().is_err());
    policy.rounds = Some(crate::ModelWaitRounds::default());
    policy.rounds.as_mut().unwrap().offload_cooldown = Duration::from_secs(86401);
    assert!(policy.validate().is_err());
}

#[tokio::test(start_paused = true)]
async fn host_filesystem_read_uses_the_same_offload_and_resume_path() {
    let (mut coordinator, control) = rounds_fixture();
    Arc::get_mut(&mut coordinator)
        .unwrap()
        .policy
        .filesystem_reads = true;
    control.host_reads.store(1, Ordering::Release);
    coordinator.start_host_reads();
    control.called(2).await;
    assert_eq!(control.recorded(), ["run.pause", "run.offload"]);
    control.host_reads.store(0, Ordering::Release);
    control.idle_wake.lock().unwrap().as_ref().unwrap().cancel();
    control.called(3).await;
    assert_eq!(control.recorded()[2], "run.resume");
    coordinator.cancellation.cancel();
}

#[tokio::test(start_paused = true)]
async fn download_wait_uses_common_ownership_and_cancels_safely() {
    use crate::runtime::model_wait::NativeModelWait;
    use pvisor_gateway::model_wait::ModelWaitLifecycle;
    let (mut coordinator, control) = rounds_fixture();
    Arc::get_mut(&mut coordinator).unwrap().policy.downloads = true;
    let adapter = NativeModelWait {
        coordinator: coordinator.clone(),
        attempt_id: pvisor_core::AttemptId::new("download-test"),
    };
    let mut download = adapter.reserve_download().unwrap().unwrap();
    download.waiting().await.unwrap();
    control.called(2).await;
    download.ready().await.unwrap();
    assert_eq!(
        control.recorded(),
        ["run.pause", "run.offload", "run.resume"]
    );
    download.waiting().await.unwrap();
    control.called(4).await;
    drop(download);
    control.called(5).await;
    assert_eq!(control.recorded()[4], "run.resume");
    coordinator.cancellation.cancel();
}

#[tokio::test(start_paused = true)]
async fn resume_rearm_precedes_release_without_recheck_gap_and_preserves_cooldown() {
    let (coordinator, control) = rounds_fixture();
    control.rearm.store(true, Ordering::Release);
    let entry = entered(&coordinator).await;
    control.called(2).await;
    let start = Instant::now();
    let old = entry.current_round();
    old.wake.cancel();
    control.called(5).await;
    assert_eq!(
        control.recorded(),
        [
            "run.pause",
            "run.offload",
            "arm.next",
            "run.resume",
            "run.pause"
        ]
    );
    assert!(Instant::now() - start < Duration::from_millis(100));
    assert_eq!(control.idle_epoch.load(Ordering::Acquire), 2);
    old.wake.cancel();
    assert!(!entry.current_round().wake.is_cancelled());
    tokio::time::advance(Duration::from_millis(50)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        control
            .recorded()
            .iter()
            .filter(|c| **c == "run.offload")
            .count(),
        1
    );
    entry.before_delivery().await.unwrap();
    assert_eq!(control.recorded().last(), Some(&"run.resume"));
}

#[tokio::test(start_paused = true)]
async fn stream_delivery_during_resume_rearm_revokes_new_generation_and_completes() {
    let (mut coordinator, control) = rounds_fixture();
    Arc::get_mut(&mut coordinator).unwrap().policy.offload_after = None;
    control.rearm.store(true, Ordering::Release);
    let entry = entered(&coordinator).await;
    control.called(1).await;
    let resume = control.block("run.resume");
    entry.current_round().wake.cancel();
    control.called(3).await;
    let e = entry.clone();
    let batch = tokio::spawn(async move { e.before_batch().await });
    tokio::task::yield_now().await;
    assert!(!batch.is_finished());
    assert!(entry.current_round().wake.is_cancelled());
    resume.add_permits(1);
    batch.await.unwrap().unwrap();
    tokio::time::advance(Duration::from_secs(10)).await;
    assert_eq!(control.recorded(), ["run.pause", "arm.next", "run.resume"]);
    assert_eq!(control.state(), RunState::Running);
    entry.before_delivery().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn failed_resume_rearm_completes_new_round_with_error_and_cancels_attempt() {
    let (mut coordinator, control) = rounds_fixture();
    Arc::get_mut(&mut coordinator).unwrap().policy.offload_after = None;
    control.rearm.store(true, Ordering::Release);
    let entry = entered(&coordinator).await;
    control.called(1).await;
    *control.fail.lock().unwrap() = Some("run.resume");
    entry.current_round().wake.cancel();
    assert!(entry.cleanup_completed().await.is_err());
    assert!(entry.current_round().completed().await.is_err());
    assert!(coordinator.cancellation.is_cancelled());
}
