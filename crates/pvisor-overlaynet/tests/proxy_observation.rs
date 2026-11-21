use async_trait::async_trait;
use axum::{
    body::Body,
    extract::{ConnectInfo, Request},
    http::StatusCode,
    response::Response,
};
use http_body_util::BodyExt;
use proptest::prelude::*;
use pvisor_core::{
    ControlController, ControlReason, ControlRequest, ControlTransition, PolicyControlController,
    network::{
        NetworkAccessRule, NetworkConfig, NetworkMode, NetworkPolicy, ResolvedAddressPolicy,
    },
};
use pvisor_overlaynet::api::{
    DownloadWait, InterceptionMetrics, InterceptionObservation, InterceptionSnapshot, OverlayProxy,
    OverlayRequestContext, OverlayServerState, OverlaySink, ProxyEvent, TargetCounters,
};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tower::ServiceExt;

#[derive(Clone, Copy)]
enum Behavior {
    Forward,
    Respond,
    ContextFailure,
    HandlerFailure,
    DownloadFailure,
}

#[derive(Clone)]
struct Sink {
    policy: NetworkPolicy,
    events: Arc<Mutex<Vec<String>>>,
    calls: Arc<Mutex<Vec<&'static str>>>,
    behavior: Behavior,
    wait: Option<Arc<WaitGate>>,
}

#[async_trait]
impl OverlaySink for Sink {
    type RequestContext = ();
    fn request_context(&self, _: &Request) -> anyhow::Result<OverlayRequestContext<()>> {
        self.calls.lock().unwrap().push("context");
        anyhow::ensure!(
            !matches!(self.behavior, Behavior::ContextFailure),
            "context failure"
        );
        Ok(OverlayRequestContext {
            policy: self.policy.clone(),
            run_id: Some("observer-run".into()),
            execution_id: Some("observer-execution".into()),
            session_id: "observation".into(),
            sink: (),
        })
    }
    async fn handle(
        &self,
        _: Request,
        peer: SocketAddr,
        context: &OverlayRequestContext<()>,
    ) -> anyhow::Result<Response> {
        self.calls.lock().unwrap().push("handle");
        assert_eq!(peer, "127.0.0.1:30000".parse::<SocketAddr>().unwrap());
        assert_eq!(context.run_id.as_deref(), Some("observer-run"));
        assert_eq!(context.execution_id.as_deref(), Some("observer-execution"));
        assert!(!matches!(
            self.behavior,
            Behavior::Forward | Behavior::DownloadFailure
        ));
        anyhow::ensure!(
            !matches!(self.behavior, Behavior::HandlerFailure),
            "sink failure"
        );
        Ok(Response::builder()
            .status(StatusCode::CREATED)
            .body(Body::from("handled"))
            .unwrap())
    }
    fn download_wait(
        &self,
        _: &OverlayRequestContext<()>,
    ) -> anyhow::Result<Option<Box<dyn DownloadWait>>> {
        self.calls.lock().unwrap().push("download_wait");
        anyhow::ensure!(
            !matches!(self.behavior, Behavior::DownloadFailure),
            "download hook failure"
        );
        Ok(self
            .wait
            .as_ref()
            .map(|gate| Box::new(Wait(gate.clone())) as Box<dyn DownloadWait>))
    }
    fn accepts(&self, _: &Request) -> bool {
        self.calls.lock().unwrap().push("accepts");
        matches!(self.behavior, Behavior::Respond | Behavior::HandlerFailure)
    }
    fn observe(&self, context: &OverlayRequestContext<()>, event: ProxyEvent<'_>) {
        assert_eq!(context.session_id, "observation");
        let value = match event {
            ProxyEvent::Denied { host, reason } => {
                assert!(!reason.as_str().is_empty());
                format!("denied:{host}")
            }
            ProxyEvent::Dispatched { request, target } => {
                assert_eq!(request.method(), "GET");
                format!("dispatch:{target}")
            }
            ProxyEvent::Response { target, status } => {
                assert!(target.starts_with("http://127.0.0.1:"));
                format!("response:{}", status.as_u16())
            }
            ProxyEvent::Failed { target, error } => {
                assert!(target.starts_with("http://127.0.0.1:"));
                assert!(!error.is_empty());
                "failed".into()
            }
        };
        self.events.lock().unwrap().push(value);
    }
}

#[rstest::fixture]
fn sink(
    #[default(Behavior::Forward)] behavior: Behavior,
    #[default(NetworkMode::Allowlist)] mode: NetworkMode,
) -> Sink {
    let policy = NetworkPolicy::compile(&NetworkConfig {
        mode,
        rules: if mode == NetworkMode::Allowlist {
            vec![NetworkAccessRule {
                host: "127.0.0.1".into(),
                ports: vec![80],
                transports: vec![],
                allow_private_ips: true,
            }]
        } else {
            vec![]
        },
        ..Default::default()
    })
    .unwrap();
    Sink {
        policy,
        behavior,
        events: Arc::default(),
        calls: Arc::default(),
        wait: None,
    }
}

async fn get(router: &axum::Router, uri: &str) -> Response {
    let mut request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:30000".parse::<SocketAddr>().unwrap(),
    ));
    router.clone().oneshot(request).await.unwrap()
}

struct Upstream {
    sink: Sink,
    address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[rstest::fixture]
async fn upstream(mut sink: Sink) -> Upstream {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().fallback(|| async { StatusCode::CREATED }),
        )
        .await
        .unwrap();
    });
    sink.policy = NetworkPolicy::compile(&NetworkConfig {
        mode: NetworkMode::Allowlist,
        rules: vec![NetworkAccessRule {
            host: "127.0.0.1".into(),
            ports: vec![address.port()],
            transports: vec![],
            allow_private_ips: true,
        }],
        ..Default::default()
    })
    .unwrap();
    Upstream {
        sink,
        address,
        task,
    }
}

#[rstest::rstest]
#[tokio::test]
async fn one_observer_retains_denial_dispatch_response_and_failure_ordering(
    #[future(awt)] mut upstream: Upstream,
) {
    let sink = upstream.sink.clone();
    let events = sink.events.clone();
    let address = upstream.address;
    let router = OverlayServerState::new(
        Arc::new(PolicyControlController),
        sink,
        Arc::new(AtomicUsize::new(0)),
    )
    .into_router();
    assert_eq!(
        get(&router, "http://denied.invalid/").await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get(&router, &format!("http://{address}/")).await.status(),
        StatusCode::CREATED
    );
    assert_eq!(
        *events.lock().unwrap(),
        ["denied:denied.invalid", "dispatch:forward", "response:201"]
    );
    upstream.task.abort();
    let _ = (&mut upstream.task).await;
    assert_eq!(
        get(&router, &format!("http://{address}/")).await.status(),
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        *events.lock().unwrap(),
        [
            "denied:denied.invalid",
            "dispatch:forward",
            "response:201",
            "dispatch:forward",
            "failed"
        ]
    );
}

#[rstest::rstest]
#[case::context(Behavior::ContextFailure, &["context"], &[], "context failure",
    InterceptionSnapshot { requests_seen: 1, failures: 1, ..Default::default() })]
#[case::sink(Behavior::Respond, &["context", "accepts", "handle"], &["dispatch:sink"], "handled",
    InterceptionSnapshot { requests_seen: 1, policy_allowed: 1, absolute_http_requests: 1, sink_requests: 1,
        targets: [("HTTP 127.0.0.1:80".into(), TargetCounters { allowed: 1, ..Default::default() })].into(), ..Default::default() })]
#[case::handler_failure(Behavior::HandlerFailure, &["context", "accepts", "handle"], &["dispatch:sink"], "sink failure",
    InterceptionSnapshot { requests_seen: 1, policy_allowed: 1, absolute_http_requests: 1, sink_requests: 1, failures: 1,
        targets: [("HTTP 127.0.0.1:80".into(), TargetCounters { allowed: 1, failed: 1, ..Default::default() })].into(), ..Default::default() })]
#[case::download_hook_failure(Behavior::DownloadFailure, &["context", "accepts", "download_wait"], &["dispatch:forward"], "download hook failure",
    InterceptionSnapshot { requests_seen: 1, policy_allowed: 1, absolute_http_requests: 1, failures: 1,
        targets: [("HTTP 127.0.0.1:80".into(), TargetCounters { allowed: 1, ..Default::default() })].into(), ..Default::default() })]
#[tokio::test]
async fn sink_paths_preserve_callbacks_accounting_and_failure_cleanup(
    #[case] behavior: Behavior,
    #[with(behavior)] sink: Sink,
    #[case] calls: &[&str],
    #[case] events: &[&str],
    #[case] body: &str,
    #[case] expected: InterceptionSnapshot,
) {
    let active = Arc::new(AtomicUsize::new(0));
    let metrics = InterceptionMetrics::default();
    let shared = metrics.clone();
    let before = metrics.snapshot();
    let state = OverlayServerState::new(
        Arc::new(PolicyControlController),
        sink.clone(),
        active.clone(),
    )
    .with_interception_metrics(shared.clone());
    let response = get(&state.into_router(), "http://127.0.0.1:80/").await;
    assert_eq!(
        response.status(),
        if matches!(behavior, Behavior::Respond) {
            StatusCode::CREATED
        } else {
            StatusCode::BAD_GATEWAY
        }
    );
    assert_eq!(active.load(Ordering::Relaxed), 1);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    if matches!(behavior, Behavior::Respond) {
        assert_eq!(bytes, body);
    } else {
        assert!(std::str::from_utf8(&bytes).unwrap().contains(body));
    }
    assert_eq!(active.load(Ordering::Relaxed), 0);
    assert_eq!(*sink.calls.lock().unwrap(), calls);
    assert_eq!(*sink.events.lock().unwrap(), events);
    assert_eq!(metrics.snapshot(), expected);
    assert_eq!(shared.snapshot(), expected);
    assert_eq!(before, InterceptionSnapshot::default());
    assert_eq!(InterceptionMetrics::default().snapshot(), before);
}

#[rstest::rstest]
#[case::relative("GET", "/local", StatusCode::CREATED, &["context", "handle"], &["dispatch:sink"])]
#[case::absolute("GET", "http://127.0.0.1:80/", StatusCode::FORBIDDEN, &["context"], &["denied:127.0.0.1"])]
#[case::connect("CONNECT", "denied.invalid:443", StatusCode::FORBIDDEN, &["context"], &["denied:denied.invalid"])]
#[case::invalid_port("GET", "http://127.0.0.1:65536/", StatusCode::BAD_REQUEST, &["context"], &[])]
#[case::invalid_connect("CONNECT", "[::1]suffix:8443", StatusCode::BAD_REQUEST, &["context"], &[])]
#[case::connect_empty_port("CONNECT", "[::1]:", StatusCode::BAD_REQUEST, &["context"], &[])]
#[case::connect_zero_port("CONNECT", "[::1]:0", StatusCode::BAD_REQUEST, &["context"], &[])]
#[case::connect_overflowing_port("CONNECT", "[::1]:65536", StatusCode::BAD_REQUEST, &["context"], &[])]
#[case::connect_missing_colon("CONNECT", "[::1]443", StatusCode::BAD_REQUEST, &["context"], &[])]
#[tokio::test]
async fn classification_and_rejection_precede_sink_selection(
    #[with(Behavior::Respond, NetworkMode::NoNetwork)] sink: Sink,
    #[case] method: &str,
    #[case] uri: &str,
    #[case] status: StatusCode,
    #[case] calls: &[&str],
    #[case] events: &[&str],
) {
    let active = Arc::new(AtomicUsize::new(0));
    let router = OverlayServerState::new(
        Arc::new(PolicyControlController),
        sink.clone(),
        active.clone(),
    )
    .into_router();
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:30000".parse::<SocketAddr>().unwrap(),
    ));
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), status);
    drop(response);
    assert_eq!(active.load(Ordering::Relaxed), 0);
    assert_eq!(*sink.calls.lock().unwrap(), calls);
    assert_eq!(*sink.events.lock().unwrap(), events);
}

#[rstest::rstest]
#[case::below_limit(255, 255, 0)]
#[case::at_limit(256, 256, 0)]
#[case::overflow(257, 256, 1)]
#[tokio::test]
async fn cloned_proxy_owners_share_bounded_destination_accounting(
    #[with(Behavior::Forward, NetworkMode::NoNetwork)] sink: Sink,
    #[case] requests: u64,
    #[case] targets: usize,
    #[case] overflow: u64,
) {
    let metrics = InterceptionMetrics::default();
    let before = metrics.snapshot();
    let active = Arc::new(AtomicUsize::new(0));
    let state = OverlayServerState::new(Arc::new(PolicyControlController), sink, active.clone())
        .with_interception_metrics(metrics.clone());
    let routers = [state.clone().into_router(), state.into_router()];
    for index in 0..requests {
        assert_eq!(
            get(
                &routers[index as usize % 2],
                &format!("http://HOST-{index}.INVALID./")
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
    }
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.requests_seen, requests);
    assert_eq!(snapshot.policy_denied, requests);
    assert_eq!(snapshot.targets.len(), targets);
    assert_eq!(snapshot.target_overflow, overflow);
    assert_eq!(
        snapshot
            .targets
            .values()
            .map(|target| target.denied)
            .sum::<u64>()
            + overflow,
        requests
    );
    assert!(
        snapshot
            .targets
            .keys()
            .all(|target| target.starts_with("HTTP host-") && target.ends_with(".invalid:80"))
    );
    assert_eq!(active.load(Ordering::Relaxed), 0);
    assert_eq!(before, InterceptionSnapshot::default());
}

#[rstest::rstest]
#[case::default(None, StatusCode::CREATED)]
#[case::strict(Some(ResolvedAddressPolicy::Strict), StatusCode::CREATED)]
#[case::host(Some(ResolvedAddressPolicy::HostConnectorAliases), StatusCode::CREATED)]
#[case::vm(Some(ResolvedAddressPolicy::VmEgress), StatusCode::FORBIDDEN)]
#[tokio::test]
async fn resolved_address_constraints_cannot_be_bypassed_by_an_accepting_sink(
    #[with(Behavior::Respond)] mut sink: Sink,
    #[case] resolution: Option<ResolvedAddressPolicy>,
    #[case] status: StatusCode,
) {
    sink.policy = NetworkPolicy::compile(&NetworkConfig {
        mode: NetworkMode::Allowlist,
        rules: vec![NetworkAccessRule {
            host: "198.51.100.1".into(),
            ports: vec![80],
            transports: vec![],
            allow_private_ips: true,
        }],
        ..Default::default()
    })
    .unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let mut state = OverlayServerState::new(
        Arc::new(PolicyControlController),
        sink.clone(),
        active.clone(),
    );
    if let Some(resolution) = resolution {
        state = state.with_resolved_address_policy(resolution);
    }
    let response = get(&state.into_router(), "http://198.51.100.1:80/").await;
    assert_eq!(response.status(), status);
    drop(response);
    assert_eq!(active.load(Ordering::Relaxed), 0);
    assert_eq!(
        sink.calls.lock().unwrap().contains(&"handle"),
        status == StatusCode::CREATED
    );
}

struct Controller {
    deny: bool,
    calls: AtomicUsize,
}

impl ControlController for Controller {
    fn authorize(&self, request: ControlRequest<'_>) -> ControlTransition {
        let ControlRequest::Network { request, .. } = request else {
            panic!("expected network admission")
        };
        assert_eq!(
            request.run_id.as_ref().map(|id| id.0.as_str()),
            Some("observer-run")
        );
        assert_eq!(
            request.execution_id.as_ref().map(|id| id.0.as_str()),
            Some("observer-execution")
        );
        assert_eq!(request.host, "127.0.0.1");
        assert_eq!(request.port, Some(80));
        assert_eq!(request.transport, pvisor_core::NetworkTransport::Http);
        let phase = self.calls.fetch_add(1, Ordering::Relaxed);
        assert_eq!(
            request.resolved_ip,
            if phase == 0 {
                None
            } else {
                Some("127.0.0.1".parse().unwrap())
            }
        );
        if self.deny {
            ControlTransition::denied(ControlReason::NetworkDenied)
        } else {
            ControlTransition::allowed(ControlReason::AmbientNetwork)
        }
    }
}

#[rstest::rstest]
#[case::allowed(NetworkMode::Allowlist, false, StatusCode::CREATED, 2)]
#[case::controller_veto(NetworkMode::Allowlist, true, StatusCode::FORBIDDEN, 1)]
#[case::policy_veto(NetworkMode::NoNetwork, false, StatusCode::FORBIDDEN, 0)]
#[tokio::test]
async fn controllers_receive_attribution_and_only_narrow_policy(
    #[case] _mode: NetworkMode,
    #[with(Behavior::Respond, _mode)] sink: Sink,
    #[case] deny: bool,
    #[case] status: StatusCode,
    #[case] calls: usize,
) {
    let controller = Arc::new(Controller {
        deny,
        calls: AtomicUsize::new(0),
    });
    let active = Arc::new(AtomicUsize::new(0));
    let router = OverlayServerState::new(controller.clone(), sink, active.clone()).into_router();
    let response = get(&router, "http://127.0.0.1:80/").await;
    assert_eq!(response.status(), status);
    drop(response);
    assert_eq!(controller.calls.load(Ordering::Relaxed), calls);
    assert_eq!(active.load(Ordering::Relaxed), 0);
}

struct WaitGate {
    calls: Mutex<Vec<&'static str>>,
    entered: tokio::sync::Notify,
    resume: tokio::sync::Semaphore,
}

#[rstest::fixture]
fn download_gate() -> Arc<WaitGate> {
    Arc::new(WaitGate {
        calls: Mutex::default(),
        entered: tokio::sync::Notify::new(),
        resume: tokio::sync::Semaphore::new(0),
    })
}

struct Wait(Arc<WaitGate>);

#[async_trait]
impl DownloadWait for Wait {
    async fn waiting(&mut self) -> anyhow::Result<()> {
        self.0.calls.lock().unwrap().push("waiting");
        Ok(())
    }
    async fn ready(&mut self) -> anyhow::Result<()> {
        self.0.calls.lock().unwrap().push("ready");
        self.0.entered.notify_one();
        self.0.resume.acquire().await?.forget();
        Ok(())
    }
    fn cancel(&mut self) {
        self.0.calls.lock().unwrap().push("cancel");
    }
}

async fn blocked_at_ready<F: std::future::Future>(
    future: &mut std::pin::Pin<Box<F>>,
    gate: &WaitGate,
) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::select! {
            _ = gate.entered.notified() => {},
            _ = future => panic!("delivery crossed a blocked ready hook"),
        }
    })
    .await
    .unwrap();
}

#[rstest::rstest]
#[case::cancel_headers(true, false)]
#[case::cancel_body(false, true)]
#[case::complete(false, false)]
#[tokio::test]
async fn download_hooks_fence_headers_and_eof_and_cancel_exactly_once(
    #[future(awt)] upstream: Upstream,
    download_gate: Arc<WaitGate>,
    #[case] cancel_headers: bool,
    #[case] cancel_body: bool,
) {
    let mut sink = upstream.sink.clone();
    let address = upstream.address;
    sink.wait = Some(download_gate.clone());
    let active = Arc::new(AtomicUsize::new(0));
    let router = OverlayServerState::new(Arc::new(PolicyControlController), sink, active.clone())
        .into_router();
    let uri = format!("http://{address}/");
    let mut response = Box::pin(get(&router, &uri));
    blocked_at_ready(&mut response, &download_gate).await;
    assert_eq!(active.load(Ordering::Relaxed), 1);
    if cancel_headers {
        drop(response);
    } else {
        download_gate.resume.add_permits(1);
        let response = tokio::time::timeout(std::time::Duration::from_secs(3), response)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let mut body = Box::pin(response.into_body().collect());
        blocked_at_ready(&mut body, &download_gate).await;
        assert_eq!(active.load(Ordering::Relaxed), 1);
        if cancel_body {
            drop(body);
        } else {
            download_gate.resume.add_permits(1);
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(3), body)
                    .await
                    .unwrap()
                    .unwrap()
                    .to_bytes()
                    .is_empty()
            );
        }
    }
    assert_eq!(active.load(Ordering::Relaxed), 0);
    assert_eq!(
        *download_gate.calls.lock().unwrap(),
        if cancel_headers {
            vec!["waiting", "ready", "cancel"]
        } else {
            vec!["waiting", "ready", "waiting", "ready", "cancel"]
        }
    );
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(
            concat!(env!("CARGO_MANIFEST_DIR"), "/proptest-regressions/proxy_observation.txt")
        ))),
        ..ProptestConfig::default()
    })]
    #[test]
    fn generated_destination_traces_preserve_counts_across_cloned_owners(
        destinations in prop::collection::vec(0u16..300, 0..600),
    ) {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let metrics = InterceptionMetrics::default();
            let before = metrics.snapshot();
            let active = Arc::new(AtomicUsize::new(0));
            let state = OverlayServerState::new(Arc::new(PolicyControlController),
                sink(Behavior::Forward, NetworkMode::NoNetwork), active.clone())
                .with_interception_metrics(metrics.clone());
            let routers = [state.clone().into_router(), state.into_router()];
            let mut expected = std::collections::BTreeMap::<String, u64>::new();
            for (step, destination) in destinations.iter().enumerate() {
                let target = format!("HTTP host-{destination}.invalid:80");
                *expected.entry(target).or_default() += 1;
                assert_eq!(get(&routers[step % 2], &format!("http://HOST-{destination}.INVALID./")).await.status(), StatusCode::FORBIDDEN);
                assert_eq!(active.load(Ordering::Relaxed), 0);
            }
            let snapshot = metrics.snapshot();
            assert_eq!(snapshot.requests_seen, destinations.len() as u64);
            assert_eq!(snapshot.policy_denied, destinations.len() as u64);
            assert_eq!(snapshot.targets.len(), expected.len().min(256));
            for (target, counts) in &snapshot.targets {
                assert_eq!(*counts, TargetCounters { denied: expected[target], ..Default::default() });
            }
            assert_eq!(snapshot.targets.values().map(|counts| counts.denied).sum::<u64>() + snapshot.target_overflow, destinations.len() as u64);
            assert_eq!(before, InterceptionSnapshot::default());
        });
    }
}
