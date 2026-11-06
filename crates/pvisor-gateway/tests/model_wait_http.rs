//! Real HTTP delivery barriers. The test lifecycle controls admission; it does
//! not pretend to measure native VM execution or resource density.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::Response;
use bytes::Bytes;
use pvisor_core::ModelCallRequest;
use pvisor_gateway::config::{ModelRoute, ProxyConfig};
use pvisor_gateway::model_wait::{
    INFERENCE_IDLE_HEADER, ModelStreamBuffer, ModelWait, ModelWaitLifecycle,
};
use pvisor_gateway::runtime::in_process::{InProcessCapture, InProcessRuntime};
use pvisor_gateway::sink::CallbackObserver;
use tokio::sync::{Notify, Semaphore, mpsc};
use tokio_stream::wrappers::ReceiverStream;

const LIMIT: Duration = Duration::from_secs(5);
const REPLY: &str = r#"{"id":"reply","object":"chat.completion","model":"test-model","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#;
const FIRST: &str = "data: {\"id\":\"reply\",\"object\":\"chat.completion.chunk\",\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"}}]}\n\n";
const LAST: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

#[derive(Debug)]
struct Admission {
    entered: Notify,
    delivery: Notify,
    cancelled: Notify,
    enter_gate: Semaphore,
    resume_gate: Semaphore,
    entries: AtomicUsize,
    resumes: AtomicUsize,
    cancellations: AtomicUsize,
    fail_enter: bool,
    fail_resume: bool,
    stream_buffer: Option<ModelStreamBuffer>,
    wake: Notify,
    buffering: Notify,
    repeat_batches: bool,
    batches: AtomicUsize,
    drained: Notify,
}

impl Default for Admission {
    fn default() -> Self {
        Self {
            entered: Notify::new(),
            delivery: Notify::new(),
            cancelled: Notify::new(),
            enter_gate: Semaphore::new(0),
            resume_gate: Semaphore::new(0),
            entries: AtomicUsize::new(0),
            resumes: AtomicUsize::new(0),
            cancellations: AtomicUsize::new(0),
            fail_enter: false,
            fail_resume: false,
            stream_buffer: None,
            wake: Notify::new(),
            buffering: Notify::new(),
            repeat_batches: false,
            batches: AtomicUsize::new(0),
            drained: Notify::new(),
        }
    }
}

#[derive(Debug)]
struct Lifecycle(Arc<Admission>);
struct Wait(Arc<Admission>);

impl ModelWaitLifecycle for Lifecycle {
    fn reserve(&self, request: ModelCallRequest) -> anyhow::Result<Box<dyn ModelWait>> {
        assert_eq!(request.execution_id.unwrap().as_str(), "wait-execution");
        assert_eq!(request.client_model, "test-model");
        assert!(!request.call_id.is_empty());
        Ok(Box::new(Wait(self.0.clone())))
    }
}

#[async_trait]
impl ModelWait for Wait {
    fn repeat_stream_batches(&self) -> bool {
        self.0.repeat_batches
    }
    async fn before_batch(&mut self) -> anyhow::Result<()> {
        self.0.delivery.notify_one();
        self.0.resume_gate.acquire().await.unwrap().forget();
        anyhow::ensure!(!self.0.fail_resume, "resume rejected");
        self.0.batches.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn after_batch(&mut self) -> anyhow::Result<()> {
        self.0.drained.notify_one();
        Ok(())
    }
    fn stream_buffer(&self) -> Option<ModelStreamBuffer> {
        self.0.stream_buffer
    }
    async fn wait_for_wake(&mut self) {
        self.0.buffering.notify_one();
        self.0.wake.notified().await;
    }
    async fn enter(&mut self) -> anyhow::Result<()> {
        self.0.entries.fetch_add(1, Ordering::SeqCst);
        self.0.entered.notify_one();
        self.0.enter_gate.acquire().await.unwrap().forget();
        anyhow::ensure!(!self.0.fail_enter, "entry rejected");
        Ok(())
    }
    async fn before_delivery(&mut self) -> anyhow::Result<()> {
        self.0.delivery.notify_one();
        self.0.resume_gate.acquire().await.unwrap().forget();
        anyhow::ensure!(!self.0.fail_resume, "resume rejected");
        self.0.resumes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn cancel(&mut self) {
        self.0.cancellations.fetch_add(1, Ordering::SeqCst);
        self.0.cancelled.notify_one();
    }
}

type SupplierReceiver =
    Arc<tokio::sync::Mutex<Option<mpsc::Receiver<Result<Bytes, std::io::Error>>>>>;

struct Fixture {
    captured: Arc<std::sync::Mutex<Vec<pvisor_core::event::Event>>>,
    proxy: Option<InProcessCapture>,
    upstream: tokio::task::JoinHandle<()>,
    upstream_body: SupplierReceiver,
    body: mpsc::Sender<Result<Bytes, std::io::Error>>,
    received: mpsc::Receiver<(axum::http::HeaderMap, serde_json::Value)>,
    _storage: tempfile::TempDir,
    admission: Arc<Admission>,
}

#[derive(Default)]
struct CaptureGate {
    entered: Notify,
    failed: Notify,
    fail_after_release: bool,
    held: std::sync::atomic::AtomicBool,
    committed: std::sync::Mutex<Option<pvisor_core::event::Event>>,
    released: std::sync::Mutex<bool>,
    wake: std::sync::Condvar,
}

impl CaptureGate {
    fn observe(&self, event: &pvisor_core::event::Event) -> anyhow::Result<()> {
        if event.name() == "llm.request" && !self.held.swap(true, Ordering::SeqCst) {
            *self.committed.lock().unwrap() = Some(event.clone());
            self.entered.notify_one();
            let released = self.released.lock().unwrap();
            let (released, _) = self
                .wake
                .wait_timeout_while(released, LIMIT, |released| !*released)
                .unwrap();
            anyhow::ensure!(*released, "capture observer gate timed out");
            if self.fail_after_release {
                self.failed.notify_one();
                anyhow::bail!("synthetic post-commit observer failure");
            }
        }
        Ok(())
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }
}

struct ReleaseCaptureOnDrop(Arc<CaptureGate>);

impl Drop for ReleaseCaptureOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

impl Fixture {
    async fn new(stream: bool, status: StatusCode, admission: Admission, install: bool) -> Self {
        Self::with_controller(
            stream,
            status,
            admission,
            install,
            Arc::new(pvisor_core::PolicyControlController),
        )
        .await
    }

    async fn with_controller(
        stream: bool,
        status: StatusCode,
        admission: Admission,
        install: bool,
        controller: Arc<dyn pvisor_core::ControlController>,
    ) -> Self {
        Self::with_capture_gate(stream, status, admission, install, controller, None).await
    }

    async fn with_capture_gate(
        stream: bool,
        status: StatusCode,
        admission: Admission,
        install: bool,
        controller: Arc<dyn pvisor_core::ControlController>,
        capture_gate: Option<Arc<CaptureGate>>,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (body, body_rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(8);
        let body_rx = Arc::new(tokio::sync::Mutex::new(Some(body_rx)));
        let upstream_body = body_rx.clone();
        let (received_tx, received) = mpsc::channel(8);
        let app = Router::new().fallback(move |request: Request| {
            let body_rx = body_rx.clone();
            let received = received_tx.clone();
            async move {
                let (parts, body) = request.into_parts();
                let value = serde_json::from_slice(&to_bytes(body, 4096).await.unwrap()).unwrap();
                received.send((parts.headers, value)).await.unwrap();
                Response::builder()
                    .status(status)
                    .header(
                        "content-type",
                        if stream {
                            "text/event-stream"
                        } else {
                            "application/json"
                        },
                    )
                    .body(Body::from_stream(ReceiverStream::new(
                        body_rx.lock().await.take().unwrap(),
                    )))
                    .unwrap()
            }
        });
        let upstream = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let config = ProxyConfig {
            listen: "127.0.0.1:0".into(),
            admin_listen: "127.0.0.1:0".into(),
            agent_id: "wait-test".into(),
            session_header: "x-pvisor-session-id".into(),
            capture_level: Default::default(),
            debug: false,
            network: Default::default(),
            overlay: Default::default(),
            models: vec![ModelRoute {
                name: "test-model".into(),
                upstream: Some(format!("http://{address}/v1")),
                provider: Some("openai".into()),
                api_key: Some("fictitious-key".into()),
                upstream_anthropic: None,
                api_key_env: None,
                forward: None,
            }],
        };
        let storage = tempfile::tempdir().unwrap();
        let admission = Arc::new(admission);
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = captured.clone();
        let proxy = InProcessCapture::start_with_runtime(
            config,
            storage.path().to_owned(),
            Arc::new(CallbackObserver::new(move |event| {
                if let Some(gate) = &capture_gate {
                    gate.observe(event)?;
                }
                observed.lock().unwrap().push(event.clone());
                Ok(())
            })),
            InProcessRuntime {
                controller,
                execution_id: Some("wait-execution".into()),
                model_wait: install
                    .then(|| Arc::new(Lifecycle(admission.clone())) as Arc<dyn ModelWaitLifecycle>),
                ..Default::default()
            },
        )
        .unwrap();
        Self {
            captured,
            proxy: Some(proxy),
            upstream,
            upstream_body,
            body,
            received,
            _storage: storage,
            admission,
        }
    }

    fn request(
        &self,
        stream: bool,
        headers: &[&str],
        model: &str,
    ) -> tokio::task::JoinHandle<reqwest::Response> {
        self.request_path("/v1/chat/completions", serde_json::json!({"model":model,"stream":stream,"messages":[{"role":"user","content":"hello"}]}), headers)
    }

    fn request_path(
        &self,
        path: &str,
        body: serde_json::Value,
        headers: &[&str],
    ) -> tokio::task::JoinHandle<reqwest::Response> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(LIMIT)
            .build()
            .unwrap();
        let mut request = client
            .post(format!(
                "http://{}{path}",
                self.proxy.as_ref().unwrap().listen
            ))
            .json(&body);
        for header in headers {
            request = request.header(INFERENCE_IDLE_HEADER, *header);
        }
        tokio::spawn(async move { request.send().await.unwrap() })
    }

    async fn upstream_received(&mut self) {
        let (headers, body) = tokio::time::timeout(LIMIT, self.received.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!headers.contains_key(INFERENCE_IDLE_HEADER));
        assert_eq!(headers["authorization"], "Bearer fictitious-key");
        assert_eq!(body["model"], "test-model");
    }

    async fn chunk(&self, text: &'static str) {
        self.body
            .send(Ok(Bytes::from_static(text.as_bytes())))
            .await
            .unwrap();
    }

    fn finish_body(&mut self) {
        drop(std::mem::replace(&mut self.body, mpsc::channel(1).0));
    }

    async fn next_reply(&mut self) {
        let (sender, receiver) = mpsc::channel(8);
        assert!(self.upstream_body.lock().await.replace(receiver).is_none());
        self.body = sender;
    }

    fn stop(&mut self) {
        self.proxy.take().unwrap().shutdown().unwrap();
    }
}

#[tokio::test]
async fn post_commit_observer_failure_preserves_http_capture_and_later_calls() {
    use pvisor_gateway::dead_letter::{
        read_dead_letter_entries, read_trajectory_dead_letter_entries, trajectory_dead_letter_path,
    };
    use pvisor_gateway::record::CaptureRecord;
    use pvisor_journal::api::{Journal, JournalStore};

    for diagnostic_writable in [true, false] {
        let gate = Arc::new(CaptureGate {
            fail_after_release: true,
            ..Default::default()
        });
        let mut fixture = Fixture::with_capture_gate(
            false,
            StatusCode::OK,
            Admission::default(),
            true,
            Arc::new(pvisor_core::PolicyControlController),
            Some(gate.clone()),
        )
        .await;
        let _release = ReleaseCaptureOnDrop(gate.clone());
        let diagnostic = trajectory_dead_letter_path(fixture._storage.path());
        if !diagnostic_writable {
            // An owned temporary-directory conflict, not a host filesystem fault.
            std::fs::create_dir(&diagnostic).unwrap();
        }
        fixture.admission.enter_gate.add_permits(2);
        fixture.admission.resume_gate.add_permits(2);
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(LIMIT)
            .build()
            .unwrap();
        let address = fixture.proxy.as_ref().unwrap().listen.clone();
        for call in 0..2 {
            if call == 1 {
                tokio::time::timeout(LIMIT, fixture.next_reply())
                    .await
                    .unwrap();
            }
            let request = client
                .post(format!("http://{address}/v1/chat/completions"))
                .header("x-pvisor-session-id", "observer-recovery")
                .bearer_auth("synthetic-client-key")
                .json(&serde_json::json!({
                    "model": "test-model",
                    "api_key": "wire-only-observer-marker",
                    "messages": [{"role": "user", "content": "observer-visible-dialogue"}]
                }));
            let response = tokio::spawn(async move { request.send().await.unwrap() });
            if call == 0 {
                notified(&gate.entered).await;
                gate.release();
                notified(&gate.failed).await;
            }
            fixture.upstream_received().await;
            fixture.chunk(REPLY).await;
            fixture.finish_body();
            let response = tokio::time::timeout(LIMIT, response)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                tokio::time::timeout(LIMIT, response.text())
                    .await
                    .unwrap()
                    .unwrap(),
                REPLY
            );
        }
        let proxy = fixture.proxy.take().unwrap();
        tokio::time::timeout(LIMIT, tokio::task::spawn_blocking(move || proxy.shutdown()))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        fixture.upstream.abort();
        assert!((&mut fixture.upstream).await.unwrap_err().is_cancelled());
        assert_eq!(fixture.admission.entries.load(Ordering::SeqCst), 2);
        assert_eq!(fixture.admission.resumes.load(Ordering::SeqCst), 2);
        assert_eq!(fixture.admission.cancellations.load(Ordering::SeqCst), 0);
        let records =
            Journal::read(&fixture._storage.path().join(".capture/events.trace.jsonl")).unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| record.event.name())
                .collect::<Vec<_>>(),
            ["llm.request", "llm.response", "llm.request", "llm.response"]
        );
        assert_eq!(
            records[0].event.id,
            gate.committed.lock().unwrap().as_ref().unwrap().id
        );
        for (request, response) in [(0, 1), (2, 3)] {
            assert_eq!(
                records[response].event.caused_by,
                vec![records[request].event.id.clone()]
            );
        }
        let observed = fixture.captured.lock().unwrap();
        assert_eq!(
            observed
                .iter()
                .map(|event| event.id.clone())
                .collect::<Vec<_>>(),
            records
                .iter()
                .skip(1)
                .map(|record| record.event.id.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            read_dead_letter_entries(fixture._storage.path())
                .unwrap()
                .is_empty()
        );
        let retained = CaptureRecord::from_event(&records[0].event, 0).unwrap();
        assert_eq!(
            retained.payload["user_content"],
            "observer-visible-dialogue"
        );
        assert!(retained.payload.get("body").is_none());
        assert!(retained.payload.get("llm_request").is_none());
        assert!(retained.payload["http"].get("request_body").is_none());
        let journal_json = serde_json::to_string(&records).unwrap();
        assert!(!journal_json.contains("synthetic-client-key"));
        assert!(!journal_json.contains("wire-only-observer-marker"));
        if diagnostic_writable {
            let entries = read_trajectory_dead_letter_entries(fixture._storage.path()).unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].error, "synthetic post-commit observer failure");
            assert_eq!(entries[0].session_id, "observer-recovery");
            assert_eq!(
                serde_json::to_value(&entries[0].records).unwrap(),
                serde_json::json!([records[0].event])
            );
        } else {
            assert!(
                diagnostic.is_dir(),
                "diagnostic failure must not rewrite its destination"
            );
        }
    }
}

#[tokio::test]
async fn http_disconnect_preserves_accepted_capture_behind_observer_gate() {
    use pvisor_journal::api::{Journal, JournalStore};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let gate = Arc::new(CaptureGate::default());
    let mut fixture = Fixture::with_capture_gate(
        false,
        StatusCode::OK,
        Admission::default(),
        true,
        Arc::new(pvisor_core::PolicyControlController),
        Some(gate.clone()),
    )
    .await;
    // Drop before the fixture so assertion failures cannot strand its observer.
    let _release = ReleaseCaptureOnDrop(gate.clone());
    let address = fixture.proxy.as_ref().unwrap().listen.clone();
    let body = r#"{"model":"test-model","messages":[{"role":"user","content":"hello"}]}"#;
    let mut socket = tokio::time::timeout(LIMIT, tokio::net::TcpStream::connect(&address))
        .await
        .unwrap()
        .unwrap();
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nx-pvisor-session-id: accepted-capture\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    tokio::time::timeout(LIMIT, socket.write_all(request.as_bytes()))
        .await
        .unwrap()
        .unwrap();
    notified(&gate.entered).await;
    notified(&fixture.admission.entered).await;
    let journal_path = fixture._storage.path().join(".capture/events.trace.jsonl");
    // Observers run after a Journal receipt; do not reopen the active writer.
    let committed = gate.committed.lock().unwrap().clone().unwrap();
    assert_eq!(committed.name(), "llm.request");
    assert!(fixture.captured.lock().unwrap().is_empty());
    assert!(fixture.received.try_recv().is_err());
    // The HTTP handler is waiting at lifecycle entry while the accepted capture
    // owner is blocked after commit. Disconnect must cancel only the HTTP wait.
    drop(socket);
    notified(&fixture.admission.cancelled).await;
    assert_eq!(fixture.admission.cancellations.load(Ordering::SeqCst), 1);

    fixture.admission.enter_gate.add_permits(1);
    fixture.admission.resume_gate.add_permits(1);
    let healthy = reqwest::Client::builder()
        .no_proxy()
        .timeout(LIMIT)
        .build()
        .unwrap()
        .post(format!("http://{address}/v1/chat/completions"))
        .header("x-pvisor-session-id", "accepted-capture")
        .header("content-type", "application/json")
        .body(body);
    let healthy = tokio::spawn(async move { healthy.send().await.unwrap() });
    fixture.upstream_received().await;
    fixture.chunk(REPLY).await;
    fixture.finish_body();
    let healthy = tokio::time::timeout(LIMIT, healthy).await.unwrap().unwrap();
    assert_eq!(healthy.status(), StatusCode::OK);
    assert_eq!(
        tokio::time::timeout(LIMIT, healthy.text())
            .await
            .unwrap()
            .unwrap(),
        REPLY
    );
    // Healthy HTTP completion is not a receipt for capture queued behind the
    // first observer. No cancellation/response can bypass that story owner.
    assert!(fixture.captured.lock().unwrap().is_empty());
    // Establish a fourth and final loopback connection. Its closure witnesses
    // shutdown reaching transport teardown, rather than merely spawning a task.
    let mut admin = tokio::time::timeout(
        LIMIT,
        tokio::net::TcpStream::connect(&fixture.proxy.as_ref().unwrap().admin_listen),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(
        LIMIT,
        admin.write_all(b"GET /admin/status HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    )
    .await
    .unwrap()
    .unwrap();
    let mut first_byte = [0];
    tokio::time::timeout(LIMIT, admin.read_exact(&mut first_byte))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_byte, *b"H");
    let proxy = fixture.proxy.take().unwrap();
    let stopping = tokio::task::spawn_blocking(move || proxy.shutdown());
    let mut remaining = Vec::new();
    let closed = tokio::time::timeout(LIMIT, (&mut admin).take(4096).read_to_end(&mut remaining))
        .await
        .unwrap();
    if let Err(error) = closed {
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
    }
    assert!(
        remaining.len() < 4096,
        "must observe closure, not a read cap"
    );
    drop(admin);
    assert!(
        !stopping.is_finished(),
        "shutdown must await accepted capture"
    );
    gate.release();
    tokio::time::timeout(LIMIT, stopping)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    fixture.upstream.abort();
    assert!((&mut fixture.upstream).await.unwrap_err().is_cancelled());
    assert_eq!(fixture.admission.entries.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.admission.resumes.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.admission.cancellations.load(Ordering::SeqCst), 1);
    let records = Journal::read(&journal_path).unwrap();
    assert_eq!(
        records
            .iter()
            .map(|record| record.event.name())
            .collect::<Vec<_>>(),
        [
            "llm.request",
            "llm.call.cancelled",
            "llm.request",
            "llm.response"
        ]
    );
    assert_eq!(records[0].event.id, committed.id);
    for (request, terminal) in [(0, 1), (2, 3)] {
        assert_eq!(
            records[terminal].event.caused_by,
            vec![records[request].event.id.clone()]
        );
    }
    let observed = fixture.captured.lock().unwrap();
    assert_eq!(
        observed
            .iter()
            .map(|event| event.id.clone())
            .collect::<Vec<_>>(),
        records
            .iter()
            .map(|record| record.event.id.clone())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn malformed_request_upload_is_bad_request_without_capture_and_recovers() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut fixture = Fixture::new(false, StatusCode::OK, Admission::default(), true).await;
    let address = fixture.proxy.as_ref().unwrap().listen.clone();
    let mut socket = tokio::time::timeout(LIMIT, tokio::net::TcpStream::connect(&address))
        .await
        .unwrap()
        .unwrap();
    // A valid first chunk followed by an invalid chunk size fails the actual
    // request body reader, rather than the JSON parser or a response stream.
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\nnot-a-size\r\n"
    );
    tokio::time::timeout(LIMIT, socket.write_all(request.as_bytes()))
        .await
        .unwrap()
        .unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(LIMIT, (&mut socket).take(4096).read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    drop(socket);
    // Finish owned tasks before asserting the failure classification.
    assert!(fixture.received.try_recv().is_err());
    assert_eq!(fixture.admission.entries.load(Ordering::SeqCst), 0);
    assert!(fixture.captured.lock().unwrap().is_empty());
    fixture.admission.enter_gate.add_permits(1);
    fixture.admission.resume_gate.add_permits(1);
    let healthy = fixture.request(false, &[], "test-model");
    fixture.upstream_received().await;
    fixture.chunk(REPLY).await;
    fixture.finish_body();
    let healthy = tokio::time::timeout(LIMIT, healthy).await.unwrap().unwrap();
    assert_eq!(healthy.status(), StatusCode::OK);
    assert_eq!(
        tokio::time::timeout(LIMIT, healthy.text())
            .await
            .unwrap()
            .unwrap(),
        REPLY
    );
    let proxy = fixture.proxy.take().unwrap();
    tokio::time::timeout(LIMIT, tokio::task::spawn_blocking(move || proxy.shutdown()))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    fixture.upstream.abort();
    assert!((&mut fixture.upstream).await.unwrap_err().is_cancelled());
    assert_eq!(fixture.admission.entries.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.admission.resumes.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.admission.cancellations.load(Ordering::SeqCst), 0);
    let events = fixture.captured.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.name() == "llm.response")
            .count(),
        1
    );
    assert!(
        !events
            .iter()
            .any(|event| event.name() == "llm.call.cancelled")
    );
    assert!(
        response.starts_with(b"HTTP/1.1 400 "),
        "body read failure must not be reported as oversized: {}",
        String::from_utf8_lossy(&response)
    );
}

#[tokio::test]
async fn repeated_stream_capture_requires_protocol_terminal_even_after_done() {
    for valid in [false, true] {
        let mut fixture = Fixture::new(
            true,
            StatusCode::OK,
            Admission {
                repeat_batches: true,
                stream_buffer: Some(ModelStreamBuffer {
                    max_bytes: 32,
                    max_wait: Duration::from_millis(100),
                }),
                ..Default::default()
            },
            true,
        )
        .await;
        fixture.admission.enter_gate.add_permits(1);
        fixture.admission.resume_gate.add_permits(20);
        let response = fixture.request(true, &[], "test-model");
        fixture.upstream_received().await;
        fixture.chunk(FIRST).await;
        fixture
            .chunk(if valid { LAST } else { "data: [DONE]\n\n" })
            .await;
        fixture.finish_body();
        let response = tokio::time::timeout(LIMIT, response)
            .await
            .unwrap()
            .unwrap();
        let body = tokio::time::timeout(LIMIT, response.text()).await.unwrap();
        if valid {
            assert_eq!(body.unwrap(), format!("{FIRST}{LAST}"));
        }
        tokio::time::timeout(LIMIT, async {
            loop {
                let finished = fixture
                    .captured
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| matches!(e.name(), "llm.response.stream" | "llm.call.cancelled"));
                if finished {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let events = fixture.captured.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.name() == "llm.response.stream")
                .count(),
            usize::from(valid)
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.name() == "llm.call.cancelled")
                .count(),
            usize::from(!valid)
        );
    }
}

#[tokio::test]
async fn partial_supplier_stream_failure_never_commits_a_completed_response() {
    for (install, repeat) in [(false, false), (true, false), (true, true)] {
        for transport_error in [false, true] {
            let mut fixture = Fixture::new(
                true,
                StatusCode::OK,
                Admission {
                    repeat_batches: repeat,
                    stream_buffer: Some(ModelStreamBuffer {
                        max_bytes: 1,
                        max_wait: Duration::from_millis(100),
                    }),
                    ..Default::default()
                },
                install,
            )
            .await;
            fixture.admission.enter_gate.add_permits(1);
            fixture.admission.resume_gate.add_permits(20);
            let response = fixture.request(true, &[], "test-model");
            fixture.upstream_received().await;
            fixture.chunk(FIRST).await;
            let mut response = tokio::time::timeout(LIMIT, response)
                .await
                .unwrap()
                .unwrap();
            // Force an actual delivered prefix before the supplier fails. Without
            // this handshake Hyper can discard queued bytes with the body error.
            receive_first(&mut response).await;
            if transport_error {
                fixture
                    .body
                    .send(Err(std::io::Error::other("injected mid-stream failure")))
                    .await
                    .unwrap();
            } else {
                fixture.chunk("data: {\"choices\":[").await;
                fixture.finish_body();
            }
            assert!(
                tokio::time::timeout(LIMIT, response.text())
                    .await
                    .unwrap()
                    .is_err(),
                "install={install}, repeat={repeat}, transport_error={transport_error}"
            );
            tokio::time::timeout(LIMIT, async {
                loop {
                    if fixture
                        .captured
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|e| e.name() == "llm.call.cancelled")
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            fixture.stop();
            {
                let events = fixture.captured.lock().unwrap();
                assert_eq!(
                    events
                        .iter()
                        .filter(|e| e.name() == "llm.call.cancelled")
                        .count(),
                    1
                );
                assert!(!events.iter().any(|e| e.name() == "llm.response.stream"));
            }
            fixture.upstream.abort();
            assert!((&mut fixture.upstream).await.unwrap_err().is_cancelled());
        }
    }
}

#[tokio::test]
async fn disconnect_after_stream_prefix_cancels_repeated_wait_and_supplier_read() {
    for blocked_resume in [false, true] {
        disconnect_after_prefix(blocked_resume).await;
    }
}

async fn disconnect_after_prefix(blocked_resume: bool) {
    let mut fixture = Fixture::new(
        true,
        StatusCode::OK,
        Admission {
            repeat_batches: true,
            stream_buffer: Some(ModelStreamBuffer {
                max_bytes: 1,
                max_wait: Duration::from_secs(1),
            }),
            ..Default::default()
        },
        true,
    )
    .await;
    fixture.admission.enter_gate.add_permits(1);
    fixture.admission.resume_gate.add_permits(1);
    let response = fixture.request(true, &[], "test-model");
    fixture.upstream_received().await;
    fixture.chunk(FIRST).await;
    let mut response = tokio::time::timeout(LIMIT, response)
        .await
        .unwrap()
        .unwrap();
    receive_first(&mut response).await;
    notified(&fixture.admission.delivery).await;
    notified(&fixture.admission.drained).await;
    if blocked_resume {
        // Put the next batch inside its native resume callback, not merely
        // inside the supplier read, before closing the downstream connection.
        fixture.chunk(FIRST).await;
        notified(&fixture.admission.delivery).await;
    }
    drop(response);
    notified(&fixture.admission.cancelled).await;
    // No EOF or supplier error is sent: dropping the client must release the
    // upstream body on its own, not wait for the read-inactivity timeout.
    tokio::time::timeout(LIMIT, fixture.body.closed())
        .await
        .unwrap();
    fixture.stop();
    assert_eq!(fixture.admission.cancellations.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.admission.resumes.load(Ordering::SeqCst), 0);
    {
        let events = fixture.captured.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.name() == "llm.call.cancelled")
                .count(),
            1
        );
        assert!(!events.iter().any(|e| e.name() == "llm.response.stream"));
    }
    fixture.upstream.abort();
    assert!((&mut fixture.upstream).await.unwrap_err().is_cancelled());
}

async fn receive_first(response: &mut reqwest::Response) {
    let prefix = tokio::time::timeout(LIMIT, async {
        let mut prefix = Vec::new();
        while prefix.len() < FIRST.len() {
            prefix.extend(response.chunk().await.unwrap().unwrap());
            assert!(prefix.len() <= FIRST.len());
        }
        prefix
    })
    .await
    .unwrap();
    assert_eq!(prefix, FIRST.as_bytes());
}

#[tokio::test]
async fn messages_bridge_tool_stream_interruptions_never_fabricate_success() {
    const TOOL_FIRST: &str = concat!(
        "data: {\"id\":\"tool-reply\",\"model\":\"test-model\",\"choices\":[",
        "{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",",
        "\"type\":\"function\",\"function\":{\"name\":\"shell\",\"arguments\":\"{\\\"cmd\\\":\"}}]}}]}\n\n"
    );
    const TOOL_LAST: &str = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,",
        "\"function\":{\"arguments\":\"\\\"pwd\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    for (install, repeat) in [(false, false), (true, false), (true, true)] {
        for outcome in [
            "truncated",
            "done_without_finish",
            "transport_error",
            "terminal_then_error",
            "disconnect",
            "complete",
        ] {
            let mut fixture = Fixture::new(
                true,
                StatusCode::OK,
                Admission {
                    repeat_batches: repeat,
                    stream_buffer: Some(ModelStreamBuffer {
                        max_bytes: 1,
                        max_wait: Duration::from_millis(100),
                    }),
                    ..Default::default()
                },
                install,
            )
            .await;
            let mut request = fixture.request_path(
                "/v1/messages",
                serde_json::json!({
                    "model":"test-model", "stream":true, "max_tokens":64,
                    "messages":[{"role":"user","content":"run pwd"}],
                    "tools":[{"name":"shell","input_schema":{
                        "type":"object","properties":{"cmd":{"type":"string"}}
                    }}]
                }),
                &[],
            );
            fixture.admission.enter_gate.add_permits(1);
            fixture.upstream_received().await;
            fixture.chunk(TOOL_FIRST).await;
            if install {
                notified(&fixture.admission.delivery).await;
                assert!(
                    tokio::time::timeout(Duration::from_millis(25), &mut request)
                        .await
                        .is_err(),
                    "headers escaped the initial resume barrier"
                );
                fixture.admission.resume_gate.add_permits(20);
            }
            let mut response = tokio::time::timeout(LIMIT, request).await.unwrap().unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let mut wire = Vec::new();
            tokio::time::timeout(LIMIT, async {
                while !message_events(&wire)
                    .iter()
                    .any(|event| event["delta"]["partial_json"].as_str() == Some("{\"cmd\":"))
                {
                    wire.extend(response.chunk().await.unwrap().unwrap());
                    assert!(wire.len() < 4096);
                }
            })
            .await
            .unwrap();
            let prefix = message_events(&wire);
            assert!(prefix.iter().any(|event| {
                event["content_block"]["type"] == "tool_use"
                    && event["content_block"]["id"] == "call-1"
                    && event["content_block"]["name"] == "shell"
            }));
            assert_no_message_terminal(&prefix);

            if outcome == "disconnect" {
                drop(response);
                if install && repeat {
                    notified(&fixture.admission.cancelled).await;
                }
                tokio::time::timeout(LIMIT, fixture.body.closed())
                    .await
                    .unwrap();
            } else {
                match outcome {
                    "truncated" => {
                        fixture.chunk("data: {\"choices\":[").await;
                        fixture.finish_body();
                    }
                    "done_without_finish" => {
                        fixture.chunk("data: [DONE]\n\n").await;
                        fixture.finish_body();
                    }
                    "transport_error" => fixture
                        .body
                        .send(Err(std::io::Error::other("injected tool stream failure")))
                        .await
                        .unwrap(),
                    "terminal_then_error" => {
                        fixture.chunk(TOOL_LAST).await;
                        tokio::time::timeout(LIMIT, async {
                            while !message_events(&wire).iter().any(|event| {
                                event["delta"]["partial_json"].as_str() == Some("\"pwd\"}")
                            }) {
                                wire.extend(response.chunk().await.unwrap().unwrap());
                                assert!(wire.len() < 4096);
                            }
                        })
                        .await
                        .unwrap();
                        // A supplier finish_reason/[DONE] does not substitute for
                        // clean HTTP EOF. The renderer must still withhold success.
                        assert_no_message_terminal(&message_events(&wire));
                        fixture
                            .body
                            .send(Err(std::io::Error::other(
                                "failure after protocol terminal",
                            )))
                            .await
                            .unwrap();
                    }
                    "complete" => {
                        fixture.chunk(TOOL_LAST).await;
                        fixture.finish_body();
                    }
                    _ => unreachable!(),
                }
                let failed = tokio::time::timeout(LIMIT, async {
                    loop {
                        match response.chunk().await {
                            Ok(Some(chunk)) => {
                                wire.extend(chunk);
                                assert!(wire.len() < 4096);
                            }
                            Ok(None) => break false,
                            Err(_) => break true,
                        }
                    }
                })
                .await
                .unwrap();
                assert_eq!(failed, outcome != "complete", "{outcome}");
                let events = message_events(&wire);
                if outcome == "complete" {
                    for kind in ["content_block_stop", "message_delta", "message_stop"] {
                        assert_eq!(
                            events.iter().filter(|event| event["type"] == kind).count(),
                            1,
                            "{kind}: {events:?}"
                        );
                    }
                    let arguments: String = events
                        .iter()
                        .filter_map(|event| event["delta"]["partial_json"].as_str())
                        .collect();
                    assert_eq!(arguments, "{\"cmd\":\"pwd\"}");
                    assert!(
                        events
                            .iter()
                            .any(|event| event["delta"]["stop_reason"] == "tool_use")
                    );
                } else {
                    assert_no_message_terminal(&events);
                }
            }
            // Shutdown drains accepted capture work before checking exact totals.
            tokio::time::timeout(LIMIT, async {
                loop {
                    if fixture.captured.lock().unwrap().iter().any(|event| {
                        matches!(event.name(), "llm.call.cancelled" | "llm.response.stream")
                    }) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            fixture.stop();
            let complete = outcome == "complete";
            {
                let events = fixture.captured.lock().unwrap();
                for (kind, expected) in [
                    ("llm.response.stream", usize::from(complete)),
                    ("llm.call.cancelled", usize::from(!complete)),
                ] {
                    assert_eq!(
                        events.iter().filter(|event| event.name() == kind).count(),
                        expected,
                        "install={install}, repeat={repeat}, outcome={outcome}, kind={kind}"
                    );
                }
            }
            let cancelled = install
                && repeat
                && matches!(
                    outcome,
                    "transport_error" | "terminal_then_error" | "disconnect"
                );
            assert_eq!(
                fixture.admission.entries.load(Ordering::SeqCst),
                usize::from(install)
            );
            assert_eq!(
                fixture.admission.batches.load(Ordering::SeqCst) > 0,
                install && repeat
            );
            assert_eq!(
                fixture.admission.cancellations.load(Ordering::SeqCst),
                usize::from(cancelled)
            );
            assert_eq!(
                fixture.admission.resumes.load(Ordering::SeqCst),
                usize::from(install && !cancelled)
            );
            fixture.upstream.abort();
            assert!((&mut fixture.upstream).await.unwrap_err().is_cancelled());
        }
    }
}

fn message_events(wire: &[u8]) -> Vec<serde_json::Value> {
    std::str::from_utf8(wire)
        .unwrap()
        .split("\n\n")
        .filter_map(|frame| frame.lines().find_map(|line| line.strip_prefix("data: ")))
        .filter_map(|data| serde_json::from_str(data).ok())
        .collect()
}

fn assert_no_message_terminal(events: &[serde_json::Value]) {
    assert!(!events.iter().any(|event| matches!(
        event["type"].as_str(),
        Some("content_block_stop" | "message_delta" | "message_stop")
    )));
}

#[tokio::test]
async fn socket_disconnect_before_headers_cancels_pending_handler_without_shutdown() {
    use tokio::io::AsyncWriteExt;

    for repeat in [false, true] {
        for phase in ["entry", "buffering", "resume"] {
            let mut fixture = Fixture::new(
                true,
                StatusCode::OK,
                Admission {
                    repeat_batches: repeat,
                    stream_buffer: Some(ModelStreamBuffer {
                        max_bytes: 1,
                        max_wait: Duration::from_secs(30),
                    }),
                    ..Default::default()
                },
                true,
            )
            .await;
            let address = fixture.proxy.as_ref().unwrap().listen.clone();
            let mut socket = tokio::time::timeout(LIMIT, tokio::net::TcpStream::connect(&address))
                .await
                .unwrap()
                .unwrap();
            let body = r#"{"model":"test-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#;
            let request = format!(
                "POST /v1/chat/completions HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            tokio::time::timeout(LIMIT, socket.write_all(request.as_bytes()))
                .await
                .unwrap()
                .unwrap();
            notified(&fixture.admission.entered).await;
            if phase != "entry" {
                fixture.admission.enter_gate.add_permits(1);
                fixture.upstream_received().await;
                // This notification comes from read_prefix's polled wake future,
                // so supplier receipt alone cannot masquerade as active buffering.
                notified(&fixture.admission.buffering).await;
                if phase == "resume" {
                    fixture.chunk(FIRST).await;
                    notified(&fixture.admission.delivery).await;
                }
            }
            // Drop the actual TCP connection, not merely a request JoinHandle.
            // Entry/resume gates remain closed and no supplier EOF/error is sent.
            drop(socket);
            notified(&fixture.admission.cancelled).await;
            if phase == "entry" {
                assert!(fixture.received.try_recv().is_err());
            } else {
                tokio::time::timeout(LIMIT, fixture.body.closed())
                    .await
                    .unwrap();
            }
            tokio::time::timeout(LIMIT, async {
                loop {
                    if fixture
                        .captured
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|event| event.name() == "llm.call.cancelled")
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            // Cancellation and supplier release above must happen while Gateway
            // is still running. Only now perform separately bounded teardown.
            let proxy = fixture.proxy.take().unwrap();
            tokio::time::timeout(LIMIT, tokio::task::spawn_blocking(move || proxy.shutdown()))
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(fixture.admission.entries.load(Ordering::SeqCst), 1);
            assert_eq!(fixture.admission.cancellations.load(Ordering::SeqCst), 1);
            assert_eq!(fixture.admission.resumes.load(Ordering::SeqCst), 0);
            assert_eq!(fixture.admission.batches.load(Ordering::SeqCst), 0);
            {
                let events = fixture.captured.lock().unwrap();
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| event.name() == "llm.call.cancelled")
                        .count(),
                    1,
                    "repeat={repeat}, phase={phase}"
                );
                assert!(
                    !events.iter().any(|event| matches!(
                        event.name(),
                        "llm.response" | "llm.response.stream"
                    ))
                );
            }
            fixture.upstream.abort();
            assert!((&mut fixture.upstream).await.unwrap_err().is_cancelled());
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(proxy) = self.proxy.take() {
            proxy.shutdown().unwrap();
        }
        self.upstream.abort();
    }
}

async fn notified(notify: &Notify) {
    tokio::time::timeout(LIMIT, notify.notified())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_waits_for_first_body_then_resume_before_headers_and_preserves_every_chunk() {
    let mut fixture = Fixture::new(true, StatusCode::OK, Admission::default(), true).await;
    let mut response = fixture.request(true, &[], "test-model");
    notified(&fixture.admission.entered).await;
    assert!(fixture.received.try_recv().is_err()); // No supplier dispatch before pause confirmation.
    fixture.admission.enter_gate.add_permits(1);
    fixture.upstream_received().await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            fixture.admission.delivery.notified()
        )
        .await
        .is_err()
    );
    fixture.chunk(FIRST).await;
    notified(&fixture.admission.delivery).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut response)
            .await
            .is_err()
    );
    fixture.admission.resume_gate.add_permits(1);
    let mut response = tokio::time::timeout(LIMIT, response)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let first = response.chunk().await.unwrap().unwrap();
    assert_eq!(first, FIRST);
    // The remaining stream must stay live, rather than being eagerly buffered.
    fixture.chunk(LAST).await;
    fixture.finish_body();
    assert_eq!(response.chunk().await.unwrap().unwrap(), LAST);
    assert!(response.chunk().await.unwrap().is_none());
    fixture.stop();
    assert_eq!(fixture.admission.resumes.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.admission.cancellations.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn buffered_success_and_supplier_error_hold_the_barrier_until_body_eof() {
    for status in [StatusCode::OK, StatusCode::TOO_MANY_REQUESTS] {
        let mut fixture = Fixture::new(false, status, Admission::default(), true).await;
        let mut response = fixture.request(false, &[], "test-model");
        notified(&fixture.admission.entered).await;
        fixture.admission.enter_gate.add_permits(1);
        fixture.upstream_received().await;
        fixture
            .chunk(if status.is_success() {
                REPLY
            } else {
                r#"{"error":{"message":"retry later"}}"#
            })
            .await;
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                fixture.admission.delivery.notified()
            )
            .await
            .is_err()
        );
        fixture.finish_body();
        notified(&fixture.admission.delivery).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut response)
                .await
                .is_err()
        );
        fixture.admission.resume_gate.add_permits(1);
        let response = tokio::time::timeout(LIMIT, response)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), status);
        let bytes = response.bytes().await.unwrap();
        assert!(
            std::str::from_utf8(&bytes)
                .unwrap()
                .contains(if status.is_success() {
                    "ok"
                } else {
                    "retry later"
                })
        );
        assert_eq!(fixture.admission.cancellations.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn absent_host_lifecycle_keeps_forwarding_regardless_of_legacy_header() {
    for headers in [vec![], vec!["false"], vec!["true"]] {
        let mut fixture = Fixture::new(true, StatusCode::OK, Admission::default(), false).await;
        let response = fixture.request(true, &headers, "test-model");
        fixture.upstream_received().await;
        // Without an active inference-wait lifecycle, headers do not wait for tokens.
        let mut response = tokio::time::timeout(LIMIT, response)
            .await
            .unwrap()
            .unwrap();
        fixture.chunk(FIRST).await;
        assert_eq!(response.chunk().await.unwrap().unwrap(), FIRST);
        fixture.chunk(LAST).await;
        fixture.finish_body();
        assert_eq!(response.chunk().await.unwrap().unwrap(), LAST);
        assert!(response.chunk().await.unwrap().is_none());
        assert_eq!(fixture.admission.entries.load(Ordering::SeqCst), 0);
        fixture.stop();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unauthorized_calls_never_reserve_or_reach_supplier() {
    let mut fixture = Fixture::new(true, StatusCode::OK, Admission::default(), true).await;
    for (headers, model, status) in [
        (vec![], "forbidden-model", StatusCode::BAD_GATEWAY),
        (vec!["false"], "forbidden-model", StatusCode::BAD_GATEWAY),
    ] {
        assert_eq!(
            fixture
                .request(true, &headers, model)
                .await
                .unwrap()
                .status(),
            status
        );
    }
    assert!(fixture.received.try_recv().is_err());
    assert_eq!(fixture.admission.entries.load(Ordering::SeqCst), 0);
    #[derive(Debug)]
    struct DenyModel;
    impl pvisor_core::ControlController for DenyModel {
        fn authorize(
            &self,
            request: pvisor_core::ControlRequest<'_>,
        ) -> pvisor_core::ControlTransition {
            match request {
                pvisor_core::ControlRequest::Model { .. } => {
                    pvisor_core::ControlTransition::denied(
                        pvisor_core::ControlReason::ModelNotAllowed,
                    )
                }
                request => pvisor_core::PolicyControlController.authorize(request),
            }
        }
    }
    let mut denied = Fixture::with_controller(
        true,
        StatusCode::OK,
        Admission::default(),
        true,
        Arc::new(DenyModel),
    )
    .await;
    assert_eq!(
        denied
            .request(true, &[], "test-model")
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert!(denied.received.try_recv().is_err());
    assert_eq!(denied.admission.entries.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_once_during_entry_upstream_wait_and_resume_admission() {
    for phase in 0..3 {
        let mut fixture = Fixture::new(true, StatusCode::OK, Admission::default(), true).await;
        let response = fixture.request(true, &[], "test-model");
        notified(&fixture.admission.entered).await;
        if phase > 0 {
            fixture.admission.enter_gate.add_permits(1);
            fixture.upstream_received().await;
        }
        if phase > 1 {
            fixture.chunk(FIRST).await;
            notified(&fixture.admission.delivery).await;
        }
        fixture.stop();
        notified(&fixture.admission.cancelled).await;
        assert_eq!(fixture.admission.cancellations.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.admission.resumes.load(Ordering::SeqCst), 0);
        assert!(response.await.unwrap().status().is_server_error());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_entry_or_resume_never_delivers_supplier_reply_and_cancels_once() {
    for fail_enter in [true, false] {
        let mut fixture = Fixture::new(
            true,
            StatusCode::OK,
            Admission {
                fail_enter,
                fail_resume: !fail_enter,
                ..Default::default()
            },
            true,
        )
        .await;
        let response = fixture.request(true, &[], "test-model");
        notified(&fixture.admission.entered).await;
        fixture.admission.enter_gate.add_permits(1);
        if !fail_enter {
            fixture.upstream_received().await;
            fixture.chunk(FIRST).await;
            notified(&fixture.admission.delivery).await;
            fixture.admission.resume_gate.add_permits(1);
        }
        let response = tokio::time::timeout(LIMIT, response)
            .await
            .unwrap()
            .unwrap();
        assert!(response.status().is_server_error());
        assert!(
            !response
                .text()
                .await
                .unwrap()
                .contains("chat.completion.chunk")
        );
        notified(&fixture.admission.cancelled).await;
        assert_eq!(fixture.admission.cancellations.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.admission.resumes.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supplier_connect_or_body_failure_reacquires_admission_before_gateway_error() {
    for connect_failure in [true, false] {
        let mut fixture = Fixture::new(false, StatusCode::OK, Admission::default(), true).await;
        if connect_failure {
            fixture.upstream.abort();
            assert!((&mut fixture.upstream).await.unwrap_err().is_cancelled());
        }
        let mut response = fixture.request(false, &[], "test-model");
        notified(&fixture.admission.entered).await;
        fixture.admission.enter_gate.add_permits(1);
        if !connect_failure {
            fixture.upstream_received().await;
            fixture.chunk(REPLY).await;
            fixture
                .body
                .send(Err(std::io::Error::other("injected supplier failure")))
                .await
                .unwrap();
        }
        notified(&fixture.admission.delivery).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut response)
                .await
                .is_err()
        );
        fixture.admission.resume_gate.add_permits(1);
        let response = tokio::time::timeout(LIMIT, response)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(!response.text().await.unwrap().contains("chat.completion\""));
        assert_eq!(fixture.admission.resumes.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.admission.cancellations.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridged_messages_stream_and_buffered_reply_preserve_the_delivery_barrier() {
    for stream in [false, true] {
        let mut fixture = Fixture::new(stream, StatusCode::OK, Admission::default(), true).await;
        let mut response = fixture.request_path(
            "/v1/messages",
            serde_json::json!({
                "model":"test-model", "stream":stream, "max_tokens":64,
                "messages":[{"role":"user","content":"hello"}]
            }),
            &[],
        );
        notified(&fixture.admission.entered).await;
        fixture.admission.enter_gate.add_permits(1);
        fixture.upstream_received().await;
        fixture.chunk(if stream { FIRST } else { REPLY }).await;
        if !stream {
            fixture.finish_body();
        }
        notified(&fixture.admission.delivery).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut response)
                .await
                .is_err()
        );
        fixture.admission.resume_gate.add_permits(1);
        let response = tokio::time::timeout(LIMIT, response)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        if stream {
            fixture.chunk(LAST).await;
            fixture.finish_body();
        }
        let text = response.text().await.unwrap();
        if stream {
            for event in ["message_start", "content_block_delta", "message_stop"] {
                assert!(text.contains(event), "{text}");
            }
        } else {
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["type"], "message");
            assert_eq!(value["content"][0]["text"], "ok");
        }
        assert_eq!(fixture.admission.resumes.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.admission.cancellations.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_observation_ignores_absent_false_invalid_and_duplicate_legacy_headers() {
    for headers in [vec![], vec!["false"], vec!["yes"], vec!["true", "true"]] {
        let mut fixture = Fixture::new(false, StatusCode::OK, Admission::default(), true).await;
        let response = fixture.request(false, &headers, "test-model");
        notified(&fixture.admission.entered).await;
        fixture.admission.enter_gate.add_permits(1);
        fixture.upstream_received().await;
        fixture.chunk(REPLY).await;
        fixture.finish_body();
        notified(&fixture.admission.delivery).await;
        assert!(!response.is_finished());
        fixture.admission.resume_gate.add_permits(1);
        assert_eq!(response.await.unwrap().status(), StatusCode::OK);
        assert_eq!(fixture.admission.entries.load(Ordering::SeqCst), 1);
    }
}

fn buffered_admission(max_bytes: usize, max_wait: Duration) -> Admission {
    Admission {
        stream_buffer: Some(ModelStreamBuffer {
            max_bytes,
            max_wait,
        }),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn buffered_stream_eof_and_size_limit_still_wait_for_resume_and_preserve_bytes() {
    for size_limit in [false, true] {
        let admission =
            buffered_admission(if size_limit { 5 } else { 4096 }, Duration::from_secs(1));
        let mut fixture = Fixture::new(true, StatusCode::OK, admission, true).await;
        let response = fixture.request(true, &[], "test-model");
        notified(&fixture.admission.entered).await;
        fixture.admission.enter_gate.add_permits(1);
        fixture.upstream_received().await;
        fixture.chunk(FIRST).await;
        if !size_limit {
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(30),
                    fixture.admission.delivery.notified()
                )
                .await
                .is_err()
            );
            fixture.chunk(LAST).await;
            fixture.finish_body();
        }
        notified(&fixture.admission.delivery).await;
        assert!(!response.is_finished(), "headers exposed before resume ACK");
        fixture.admission.resume_gate.add_permits(1);
        if size_limit {
            fixture.chunk(LAST).await;
            fixture.finish_body();
        }
        let response = response.await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), format!("{FIRST}{LAST}"));
        assert_eq!(fixture.admission.resumes.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn buffered_stream_timeout_or_external_wake_delivers_before_upstream_eof() {
    for external in [false, true] {
        let admission = buffered_admission(
            4096,
            if external {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(100)
            },
        );
        let mut fixture = Fixture::new(true, StatusCode::OK, admission, true).await;
        let response = fixture.request(true, &[], "test-model");
        notified(&fixture.admission.entered).await;
        fixture.admission.enter_gate.add_permits(1);
        fixture.upstream_received().await;
        // Even silence before the first byte cannot keep the prefix wait alive.
        if external {
            fixture.admission.wake.notify_one();
        }
        notified(&fixture.admission.delivery).await;
        assert!(!response.is_finished());
        fixture.admission.resume_gate.add_permits(1);
        let response = response.await.unwrap();
        fixture.chunk(FIRST).await;
        fixture.chunk(LAST).await;
        fixture.finish_body();
        assert_eq!(response.text().await.unwrap(), format!("{FIRST}{LAST}"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_during_prefix_buffering_cancels_owned_wait_once() {
    let admission = buffered_admission(4096, Duration::from_secs(2));
    let mut fixture = Fixture::new(true, StatusCode::OK, admission, true).await;
    let response = fixture.request(true, &[], "test-model");
    notified(&fixture.admission.entered).await;
    fixture.admission.enter_gate.add_permits(1);
    fixture.upstream_received().await;
    fixture.chunk(FIRST).await;
    fixture.stop();
    notified(&fixture.admission.cancelled).await;
    assert!(response.await.unwrap().status().is_server_error());
    assert_eq!(fixture.admission.cancellations.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.admission.resumes.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unmarked_stream_has_multiple_resume_barriers_and_one_final_retirement() {
    let mut admission = buffered_admission(FIRST.len(), Duration::from_secs(2));
    admission.repeat_batches = true;
    let mut fixture = Fixture::new(true, StatusCode::OK, admission, true).await;
    let response = fixture.request(true, &[], "test-model");
    notified(&fixture.admission.entered).await;
    fixture.admission.enter_gate.add_permits(1);
    fixture.upstream_received().await;
    fixture.chunk(FIRST).await;
    notified(&fixture.admission.delivery).await;
    assert!(!response.is_finished());
    fixture.admission.resume_gate.add_permits(1);
    let response = response.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = tokio::spawn(async move { response.text().await.unwrap() });
    notified(&fixture.admission.drained).await;
    assert_eq!(fixture.admission.resumes.load(Ordering::SeqCst), 0);
    fixture.chunk(LAST).await;
    fixture.finish_body();
    notified(&fixture.admission.delivery).await;
    assert!(!body.is_finished());
    fixture.admission.resume_gate.add_permits(1); // Second batch resume.
    notified(&fixture.admission.delivery).await; // EOF after final batch drain.
    assert_eq!(fixture.admission.batches.load(Ordering::SeqCst), 2);
    fixture.admission.resume_gate.add_permits(1);
    assert_eq!(body.await.unwrap(), format!("{FIRST}{LAST}"));
    assert_eq!(fixture.admission.entries.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.admission.resumes.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.admission.cancellations.load(Ordering::SeqCst), 0);
}
