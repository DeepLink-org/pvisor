//! In-memory public proxy traces: cancellation and live policy snapshots.
//! Reservations are not promised to be refunded on cancellation. Recovery is
//! checked only after the cancelled reservation's original deadline has passed.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::task::Poll;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use axum::http::StatusCode;
use axum::response::Response;
use futures_util::poll;
use http_body_util::BodyExt;
use pvisor_overlaynet::server::build_router;
use pvisor_overlaynet::{
    BandwidthRegistry, NetworkAccessRule, NetworkBandwidthLimit, NetworkConfig, NetworkMode,
    NetworkPolicy, OverlayRequestContext, OverlayServerState, OverlaySink,
};
use tower::ServiceExt;

const PAYLOAD: &[u8] = b"four";

fn policy(host_rate: u64) -> NetworkPolicy {
    NetworkPolicy::compile(&NetworkConfig {
        mode: NetworkMode::Allowlist,
        rules: vec![NetworkAccessRule {
            host: "127.0.0.1".into(),
            ports: vec![80],
            transports: Vec::new(),
            allow_private_ips: true,
        }],
        limits: vec![
            NetworkBandwidthLimit {
                host: None,
                port: None,
                bytes_per_second: 8,
            },
            NetworkBandwidthLimit {
                host: Some("127.0.0.1".into()),
                port: Some(80),
                bytes_per_second: host_rate,
            },
        ],
        ..Default::default()
    })
    .unwrap()
}

#[derive(Clone)]
struct EchoSink(Arc<RwLock<NetworkPolicy>>);

#[async_trait]
impl OverlaySink for EchoSink {
    type RequestContext = ();

    fn request_context(&self, _: &Request) -> anyhow::Result<OverlayRequestContext<()>> {
        Ok(OverlayRequestContext {
            policy: self.0.read().unwrap().clone(),
            run_id: Some("bandwidth-run".into()),
            execution_id: Some("bandwidth-execution".into()),
            session_id: "bandwidth-session".into(),
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
        let bytes = request.into_body().collect().await?.to_bytes();
        assert_eq!(bytes.as_ref(), PAYLOAD);
        Ok(Response::new(Body::from(bytes)))
    }
}

fn proxy(host_rate: u64) -> (Router, EchoSink, Arc<AtomicUsize>) {
    let sink = EchoSink(Arc::new(RwLock::new(policy(host_rate))));
    let active = Arc::new(AtomicUsize::new(0));
    let state = OverlayServerState::new(
        Arc::new(pvisor_core::PolicyControlController),
        sink.clone(),
        active.clone(),
    )
    .with_bandwidth_registry(BandwidthRegistry::default());
    (build_router(state), sink, active)
}

fn request(router: &Router) -> impl Future<Output = Response> + use<> {
    let mut request = Request::builder()
        .method("POST")
        .uri("http://127.0.0.1:80/echo")
        .body(Body::from(PAYLOAD))
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:40000".parse::<SocketAddr>().unwrap(),
    ));
    let router = router.clone();
    async move { router.oneshot(request).await.unwrap() }
}

async fn collect(response: Response) -> Vec<u8> {
    assert_eq!(response.status(), StatusCode::OK);
    response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

// Explicit polling is the reservation handshake; no task scheduling or wall
// clock sleeps decide when a request has entered the limiter. Every wait is
// bounded by at most two virtual seconds, and there are at most two requests.
async fn after<F: Future>(future: F, delay: Duration) -> F::Output {
    let mut future = std::pin::pin!(future);
    assert!(poll!(future.as_mut()).is_pending());
    tokio::time::advance(delay - Duration::from_millis(1)).await;
    assert!(poll!(future.as_mut()).is_pending());
    tokio::time::advance(Duration::from_millis(1)).await;
    match poll!(future) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("budget did not become ready at its declared deadline"),
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_upload_or_download_does_not_poison_later_stacked_budgets() {
    for cancel_download in [false, true] {
        let (router, _, active) = proxy(4);
        if cancel_download {
            let response = after(request(&router), Duration::from_secs(1)).await;
            let mut download = Box::pin(collect(response));
            assert!(poll!(download.as_mut()).is_pending());
            assert_eq!(active.load(Ordering::Relaxed), 1);
            drop(download);
        } else {
            let mut upload = Box::pin(request(&router));
            assert!(poll!(upload.as_mut()).is_pending());
            assert_eq!(active.load(Ordering::Relaxed), 1);
            drop(upload);
        }
        assert_eq!(active.load(Ordering::Relaxed), 0);

        // Do not assume cancellation refunds either bucket. Both original
        // deadlines have expired before observing the next reservation.
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..2 {
            let response = after(request(&router), Duration::from_secs(1)).await;
            assert_eq!(active.load(Ordering::Relaxed), 1);
            let bytes = after(collect(response), Duration::from_secs(1)).await;
            assert_eq!(bytes, PAYLOAD);
            assert_eq!(active.load(Ordering::Relaxed), 0);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn policy_swap_preserves_shared_budget_and_in_flight_snapshot() {
    let (router, sink, active) = proxy(2);
    let mut old = Box::pin(request(&router));
    assert!(poll!(old.as_mut()).is_pending());
    assert_eq!(active.load(Ordering::Relaxed), 1);

    // The old upload reserved global capacity until t=.5 and host capacity
    // until t=2. A new host rule must not reset the unchanged global bucket,
    // nor inherit the old host rule's debt.
    *sink.0.write().unwrap() = policy(8);
    let response = after(request(&router), Duration::from_secs(1)).await;
    assert_eq!(active.load(Ordering::Relaxed), 2);
    assert!(poll!(old.as_mut()).is_pending());
    assert_eq!(
        after(collect(response), Duration::from_millis(500)).await,
        PAYLOAD
    );
    assert_eq!(active.load(Ordering::Relaxed), 1);

    let response = after(old, Duration::from_millis(500)).await;
    // This response retains the old 2 B/s rule, not the replacement 8 B/s.
    assert_eq!(
        after(collect(response), Duration::from_secs(2)).await,
        PAYLOAD
    );
    assert_eq!(active.load(Ordering::Relaxed), 0);
}
