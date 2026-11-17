use async_trait::async_trait;
use axum::{
    body::Body,
    extract::{ConnectInfo, Request},
    http::StatusCode,
    response::Response,
};
use pvisor_core::{
    PolicyControlController,
    network::{NetworkAccessRule, NetworkConfig, NetworkMode, NetworkPolicy},
};
use pvisor_overlaynet::api::{
    DownloadWait, OverlayProxy, OverlayRequestContext, OverlayServerState, OverlaySink, ProxyEvent,
};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex, atomic::AtomicUsize},
};
use tower::ServiceExt;

#[derive(Clone)]
struct Sink {
    policy: NetworkPolicy,
    events: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl OverlaySink for Sink {
    type RequestContext = ();
    fn request_context(&self, _: &Request) -> anyhow::Result<OverlayRequestContext<()>> {
        Ok(OverlayRequestContext {
            policy: self.policy.clone(),
            run_id: None,
            execution_id: None,
            session_id: "observation".into(),
            sink: (),
        })
    }
    async fn handle(
        &self,
        _: Request,
        _: SocketAddr,
        _: &OverlayRequestContext<()>,
    ) -> anyhow::Result<Response> {
        panic!("transparent forwarding must not dispatch to the sink")
    }
    fn download_wait(
        &self,
        _: &OverlayRequestContext<()>,
    ) -> anyhow::Result<Option<Box<dyn DownloadWait>>> {
        Ok(None)
    }
    fn accepts(&self, _: &Request) -> bool {
        false
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

#[tokio::test]
async fn one_observer_retains_denial_dispatch_response_and_failure_ordering() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().fallback(|| async { StatusCode::CREATED }),
        )
        .await
        .unwrap();
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let policy = NetworkPolicy::compile(&NetworkConfig {
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
    let sink = Sink {
        policy,
        events: events.clone(),
    };
    let router = OverlayServerState::new(
        Arc::new(PolicyControlController),
        sink,
        Arc::new(AtomicUsize::new(0)),
    )
    .into_router();
    async fn get(router: &axum::Router, uri: &str) -> Response {
        let mut request = Request::builder().uri(uri).body(Body::empty()).unwrap();
        request.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:30000".parse::<SocketAddr>().unwrap(),
        ));
        router.clone().oneshot(request).await.unwrap()
    }
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
    upstream.abort();
    let _ = upstream.await;
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
