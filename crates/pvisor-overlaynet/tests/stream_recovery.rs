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
use proptest::prelude::*;
use pvisor_core::network::{NetworkAccessRule, NetworkConfig, NetworkMode, NetworkPolicy};
use pvisor_overlaynet::api::OverlayProxy;
use pvisor_overlaynet::api::{
    InterceptionMetrics, InterceptionObservation, OverlayRequestContext, OverlayServerState,
    OverlaySink,
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

    fn download_wait(
        &self,
        _context: &pvisor_overlaynet::api::OverlayRequestContext<Self::RequestContext>,
    ) -> anyhow::Result<Option<Box<dyn pvisor_overlaynet::api::DownloadWait>>> {
        Ok(None)
    }

    fn observe(
        &self,
        _context: &pvisor_overlaynet::api::OverlayRequestContext<Self::RequestContext>,
        _event: pvisor_overlaynet::api::ProxyEvent<'_>,
    ) {
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

fn policy() -> NetworkPolicy {
    NetworkPolicy::compile(&NetworkConfig {
        mode: NetworkMode::Allowlist,
        rules: vec![NetworkAccessRule {
            host: "127.0.0.1".into(),
            ports: vec![80],
            transports: vec![],
            allow_private_ips: true,
        }],
        ..Default::default()
    })
    .unwrap()
}

#[rstest::fixture]
fn proxy() -> (Router, Arc<AtomicUsize>, InterceptionMetrics) {
    let active = Arc::new(AtomicUsize::new(0));
    let metrics = InterceptionMetrics::default();
    let router = OverlayServerState::new(
        Arc::new(pvisor_core::PolicyControlController),
        LocalSink(policy()),
        active.clone(),
    )
    .with_interception_metrics(metrics.clone())
    .into_router();
    (router, active, metrics)
}

async fn recovery(real_time: bool) {
    let (router, active, _) = proxy();
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

#[rstest::rstest]
#[tokio::test(start_paused = true)]
async fn delayed_body_error_and_cancelled_sibling_release_only_their_own_requests() {
    recovery(false).await;
}

#[derive(Clone, Copy, Debug)]
enum Action {
    Hold,
    Cancel(usize),
    Healthy,
    Fail,
}

async fn assert_body_lifetimes(
    (router, active, metrics): (Router, Arc<AtomicUsize>, InterceptionMetrics),
    actions: &[Action],
) {
    let mut held = Vec::new();
    let mut requests = 0;
    for (step, action) in actions.iter().enumerate() {
        match *action {
            Action::Hold => {
                let response = request(&router, "/pending").await;
                assert_eq!(response.status(), StatusCode::OK);
                held.push(response);
                requests += 1;
            }
            Action::Cancel(index) => {
                if !held.is_empty() {
                    assert!(index < held.len(), "step={step}, actions={actions:?}");
                    drop(held.swap_remove(index));
                }
            }
            Action::Healthy => {
                healthy(&router).await;
                requests += 1;
            }
            Action::Fail => {
                let response = request(&router, "/fault").await;
                assert_eq!(response.status(), StatusCode::OK);
                let mut body = response.into_body();
                assert_eq!(
                    body.frame().await.unwrap().unwrap().into_data().unwrap(),
                    "prefix"
                );
                let mut remaining = Box::pin(body.collect());
                assert!(poll!(remaining.as_mut()).is_pending());
                assert_eq!(active.load(Ordering::Relaxed), held.len() + 1);
                tokio::time::advance(Duration::from_secs(90)).await;
                let error = tokio::time::timeout(Duration::from_secs(2), remaining)
                    .await
                    .unwrap()
                    .unwrap_err();
                assert!(error.to_string().contains("owned delayed body failure"));
                requests += 1;
            }
        }
        assert_eq!(
            active.load(Ordering::Relaxed),
            held.len(),
            "step={step}, actions={actions:?}"
        );
        assert_eq!(
            metrics.snapshot().requests_seen,
            requests,
            "step={step}, actions={actions:?}"
        );
    }
    drop(held);
    assert_eq!(active.load(Ordering::Relaxed), 0);
    healthy(&router).await;
    assert_eq!(active.load(Ordering::Relaxed), 0);
}

#[rstest::rstest]
#[tokio::test(start_paused = true)]
async fn short_body_lifetime_sequences_keep_independent_admission(
    proxy: (Router, Arc<AtomicUsize>, InterceptionMetrics),
    #[values(Action::Hold, Action::Cancel(0), Action::Healthy, Action::Fail)] first: Action,
    #[values(Action::Hold, Action::Cancel(0), Action::Healthy, Action::Fail)] second: Action,
    #[values(Action::Hold, Action::Cancel(0), Action::Healthy, Action::Fail)] third: Action,
) {
    assert_body_lifetimes(proxy, &[first, second, third]).await;
}

#[rstest::rstest]
#[case::empty(&[])]
#[case::independent_siblings(&[Action::Hold, Action::Hold, Action::Cancel(0), Action::Healthy, Action::Cancel(0)])]
#[case::reverse_cancellation(&[Action::Hold, Action::Hold, Action::Hold, Action::Cancel(2), Action::Healthy, Action::Cancel(1), Action::Cancel(0)])]
#[case::failure_after_cancel(&[Action::Hold, Action::Cancel(0), Action::Fail, Action::Healthy])]
#[case::repeated_failures(&[Action::Hold, Action::Fail, Action::Fail, Action::Healthy, Action::Cancel(0)])]
#[case::retained_bodies(&[Action::Hold; 31])]
#[tokio::test(start_paused = true)]
async fn body_lifetime_regressions_restore_service(
    proxy: (Router, Arc<AtomicUsize>, InterceptionMetrics),
    #[case] actions: &[Action],
) {
    assert_body_lifetimes(proxy, actions).await;
}

#[tokio::test]
#[ignore = "bounded 90-second real-timer stream lifecycle; explicitly select for reliability runs"]
async fn real_delayed_body_error_retains_admission_then_recovers() {
    tokio::time::timeout(Duration::from_secs(120), recovery(true))
        .await
        .unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(
            concat!(env!("CARGO_MANIFEST_DIR"), "/proptest-regressions/stream_recovery.txt")
        ))),
        ..ProptestConfig::default()
    })]
    #[test]
    fn generated_body_lifetimes_preserve_independent_admission(
        actions in prop::collection::vec(prop_oneof![
            Just(Action::Hold), (0usize..128).prop_map(Action::Cancel),
            Just(Action::Healthy), Just(Action::Fail),
        ], 0..64),
    ) {
        let mut actions = actions;
        let mut held = 0;
        for action in &mut actions {
            match action {
                Action::Hold => held += 1,
                Action::Cancel(index) if held > 0 => { *index %= held; held -= 1; }
                _ => {}
            }
        }
        tokio::runtime::Builder::new_current_thread().enable_all().start_paused(true).build().unwrap()
            .block_on(async { assert_body_lifetimes(proxy(), &actions).await });
    }
}

#[tokio::test(start_paused = true)]
async fn generated_body_ownership_sequences_preserve_all_admission_slots() {
    let first = std::env::var("PVISOR_CHAOS_FIRST_SEED")
        .map(|value| value.parse::<u64>().unwrap())
        .unwrap_or(2026111800);
    let count = std::env::var("PVISOR_CHAOS_SEED_COUNT")
        .map(|value| value.parse::<u64>().unwrap())
        .unwrap_or(16);
    assert!((1..=4096).contains(&count));
    let (router, active, _) = proxy();
    for seed in first..first.checked_add(count).unwrap() {
        let mut random = seed;
        let mut held = Vec::new();
        for step in 0..256 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            match random % 5 {
                0 | 1 => {
                    let response = request(&router, "/pending").await;
                    if held.len() == 128 {
                        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
                    } else {
                        assert_eq!(response.status(), StatusCode::OK);
                        held.push(response);
                    }
                }
                2 if !held.is_empty() => {
                    drop(held.swap_remove(random as usize % held.len()));
                }
                3 if held.len() < 128 => {
                    let response = request(&router, "/fault").await;
                    assert_eq!(response.status(), StatusCode::OK);
                    let mut body = response.into_body();
                    assert_eq!(
                        body.frame().await.unwrap().unwrap().into_data().unwrap(),
                        "prefix"
                    );
                    let mut terminal = Box::pin(body.collect());
                    assert!(poll!(terminal.as_mut()).is_pending());
                    assert_eq!(active.load(Ordering::Relaxed), held.len() + 1);
                    tokio::time::advance(Duration::from_secs(90)).await;
                    let error = terminal.await.unwrap_err();
                    assert!(error.to_string().contains("owned delayed body failure"));
                }
                _ if held.len() == 128 => {
                    assert_eq!(
                        request(&router, "/ok").await.status(),
                        StatusCode::TOO_MANY_REQUESTS
                    );
                }
                _ => healthy(&router).await,
            }
            assert_eq!(
                active.load(Ordering::Relaxed),
                held.len(),
                "seed={seed} step={step}"
            );
            if step % 32 == 31 {
                while held.len() < 128 {
                    let response = request(&router, "/pending").await;
                    assert_eq!(response.status(), StatusCode::OK);
                    held.push(response);
                }
                assert_eq!(
                    request(&router, "/ok").await.status(),
                    StatusCode::TOO_MANY_REQUESTS
                );
                held.truncate((random % 64) as usize);
                healthy(&router).await;
                assert_eq!(active.load(Ordering::Relaxed), held.len());
            }
        }
        drop(held);
        healthy(&router).await;
        assert_eq!(active.load(Ordering::Relaxed), 0, "seed={seed}");
    }
}
