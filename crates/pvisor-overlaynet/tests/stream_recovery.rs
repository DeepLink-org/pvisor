//! In-memory public proxy lifecycle, with no listener or upstream connection.
use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{ConnectInfo, Request},
    http::StatusCode,
    response::Response,
};
use futures_util::{StreamExt, poll, stream};
use http_body_util::BodyExt;
use pvisor_overlaynet::{
    NetworkAccessRule, NetworkConfig, NetworkMode, NetworkPolicy, OverlayRequestContext,
    OverlayServerState, OverlaySink, server::build_router,
};
use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tower::ServiceExt;

#[derive(Clone)]
struct LocalSink(NetworkPolicy);

#[async_trait]
impl OverlaySink for LocalSink {
    type RequestContext = ();

    fn request_context(&self, _: &Request) -> anyhow::Result<OverlayRequestContext<()>> {
        Ok(OverlayRequestContext {
            policy: self.0.clone(),
            run_id: Some("local-stream-recovery".into()),
            execution_id: Some("local-execution".into()),
            session_id: "local-session".into(),
            sink: (),
        })
    }

    fn accepts(&self, _: &Request) -> bool {
        true
    }

    async fn handle(
        &self,
        request: Request,
        _: SocketAddr,
        _: &OverlayRequestContext<()>,
    ) -> anyhow::Result<Response> {
        let body = match request.uri().path() {
            "/fault" => Body::from_stream(
                stream::once(async { Ok::<_, io::Error>(Bytes::from_static(b"prefix")) }).chain(
                    stream::once(async {
                        tokio::time::sleep(Duration::from_secs(90)).await;
                        Err::<Bytes, _>(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "owned delayed body failure",
                        ))
                    }),
                ),
            ),
            "/pending" => Body::from_stream(stream::pending::<Result<Bytes, io::Error>>()),
            "/ok" => Body::from("healthy"),
            path => panic!("unexpected local fixture request: {path}"),
        };
        Ok(Response::new(body))
    }
}

async fn request(router: &Router, path: &str) -> Response {
    let mut request = Request::builder()
        .uri(format!("http://127.0.0.1:80{path}"))
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:40000".parse::<SocketAddr>().unwrap(),
    ));
    router.clone().oneshot(request).await.unwrap()
}

async fn healthy(router: &Router) {
    let response = request(router, "/ok").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "healthy"
    );
}

async fn recovery(real_time: bool) {
    let policy = NetworkPolicy::compile(&NetworkConfig {
        mode: NetworkMode::Allowlist,
        rules: vec![NetworkAccessRule {
            host: "127.0.0.1".into(),
            ports: vec![80],
            transports: vec![],
            allow_private_ips: true,
        }],
        ..Default::default()
    })
    .unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let router = build_router(OverlayServerState::new(
        Arc::new(pvisor_core::PolicyControlController),
        LocalSink(policy),
        active.clone(),
    ));
    let response = request(&router, "/fault").await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    assert_eq!(
        body.frame().await.unwrap().unwrap().into_data().unwrap(),
        "prefix"
    );
    let mut remaining = Box::pin(body.collect());
    assert!(poll!(remaining.as_mut()).is_pending()); // Arm the delayed failure.
    let pending = request(&router, "/pending").await;
    assert_eq!(pending.status(), StatusCode::OK);
    assert_eq!(active.load(Ordering::Relaxed), 2);
    let started = Instant::now();
    for half in 0..2 {
        if real_time {
            tokio::time::sleep(Duration::from_secs(45)).await;
        } else {
            tokio::time::advance(Duration::from_secs(45)).await;
        }
        if half == 0 {
            assert!(poll!(remaining.as_mut()).is_pending());
            assert_eq!(active.load(Ordering::Relaxed), 2);
        }
        // A separate completed request must not cancel either retained body.
        healthy(&router).await;
        assert_eq!(active.load(Ordering::Relaxed), 2);
    }
    drop(pending);
    assert_eq!(active.load(Ordering::Relaxed), 1);
    let error = tokio::time::timeout(Duration::from_secs(2), remaining)
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        error.to_string().contains("owned delayed body failure"),
        "{error}"
    );
    assert_eq!(active.load(Ordering::Relaxed), 0);
    healthy(&router).await;
    assert_eq!(active.load(Ordering::Relaxed), 0);
    // Counters alone would not detect a leaked semaphore permit.
    let mut held = Vec::new();
    for _ in 0..128 {
        let response = request(&router, "/pending").await;
        assert_eq!(response.status(), StatusCode::OK);
        held.push(response);
    }
    assert_eq!(active.load(Ordering::Relaxed), 128);
    assert_eq!(
        request(&router, "/ok").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(held);
    assert_eq!(active.load(Ordering::Relaxed), 0);
    healthy(&router).await;
    assert_eq!(active.load(Ordering::Relaxed), 0);
    if real_time {
        assert!((Duration::from_secs(85)..Duration::from_secs(115)).contains(&started.elapsed()));
    }
}

#[tokio::test(start_paused = true)]
async fn delayed_body_error_and_cancelled_sibling_release_only_their_own_requests() {
    recovery(false).await;
}

#[tokio::test]
#[ignore = "bounded 90-second real-timer stream lifecycle; explicitly select for reliability runs"]
async fn real_delayed_body_error_retains_admission_then_recovers() {
    tokio::time::timeout(Duration::from_secs(120), recovery(true))
        .await
        .unwrap();
}
