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
use pvisor_gateway::model_wait::{INFERENCE_IDLE_HEADER, ModelWait, ModelWaitLifecycle};
use pvisor_gateway::runtime::in_process::{InProcessCapture, InProcessRuntime};
use pvisor_gateway::sink::NoopCaptureObserver;
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
        }
    }
}

#[derive(Debug)]
struct Lifecycle(Arc<Admission>);
struct Wait(Arc<Admission>);

impl ModelWaitLifecycle for Lifecycle {
    fn reserve(&self, request: ModelCallRequest) -> anyhow::Result<Box<dyn ModelWait>> {
        assert_eq!(request.attempt_id.unwrap().as_str(), "wait-attempt");
        assert_eq!(request.client_model, "test-model");
        assert!(!request.call_id.is_empty());
        Ok(Box::new(Wait(self.0.clone())))
    }
}

#[async_trait]
impl ModelWait for Wait {
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

struct Fixture {
    proxy: Option<InProcessCapture>,
    upstream: tokio::task::JoinHandle<()>,
    body: mpsc::Sender<Result<Bytes, std::io::Error>>,
    received: mpsc::Receiver<(axum::http::HeaderMap, serde_json::Value)>,
    _storage: tempfile::TempDir,
    admission: Arc<Admission>,
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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (body, body_rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(8);
        let body_rx = Arc::new(tokio::sync::Mutex::new(Some(body_rx)));
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
        let proxy = InProcessCapture::start_with_runtime(
            config,
            storage.path().to_owned(),
            Arc::new(NoopCaptureObserver::new()),
            InProcessRuntime {
                controller,
                attempt_id: Some("wait-attempt".into()),
                model_wait: install
                    .then(|| Arc::new(Lifecycle(admission.clone())) as Arc<dyn ModelWaitLifecycle>),
                ..Default::default()
            },
        )
        .unwrap();
        Self {
            proxy: Some(proxy),
            upstream,
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

    fn stop(&mut self) {
        self.proxy.take().unwrap().shutdown().unwrap();
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
    let mut response = fixture.request(true, &["true"], "test-model");
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
        let mut response = fixture.request(false, &["true"], "test-model");
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
async fn absent_or_false_declarations_and_absent_lifecycle_keep_forwarding() {
    for (headers, install) in [(vec![], true), (vec!["false"], true), (vec!["true"], false)] {
        let mut fixture = Fixture::new(true, StatusCode::OK, Admission::default(), install).await;
        let response = fixture.request(true, &headers, "test-model");
        fixture.upstream_received().await;
        // Legacy response headers do not wait for upstream tokens.
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
async fn malformed_duplicate_and_unauthorized_calls_never_reserve_or_reach_supplier() {
    let mut fixture = Fixture::new(true, StatusCode::OK, Admission::default(), true).await;
    for (headers, model, status) in [
        (vec!["yes"], "test-model", StatusCode::BAD_REQUEST),
        (vec!["true", "true"], "test-model", StatusCode::BAD_REQUEST),
        (vec!["true"], "forbidden-model", StatusCode::BAD_GATEWAY),
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
            .request(true, &["true"], "test-model")
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
        let response = fixture.request(true, &["true"], "test-model");
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
        let response = fixture.request(true, &["true"], "test-model");
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
        let mut response = fixture.request(false, &["true"], "test-model");
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
            &["true"],
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
