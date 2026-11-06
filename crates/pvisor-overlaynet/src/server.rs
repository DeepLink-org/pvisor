//! Generic explicit-proxy server and request dispatch.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::Router;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use http_body_util::BodyExt;
use pvisor_core::ControlController;
use pvisor_core::{NetworkAccessRequest, NetworkTransport, RunId};

use crate::bandwidth::{BandwidthRegistry, BandwidthSession, throttle_body};
use crate::forward::{
    handle_connect_authorized, is_forward_proxy_request, parse_connect_target,
    transparent_forward_authorized,
};
use crate::interception::InterceptionMetrics;
use crate::policy::{DenyReason, NetworkPolicy, forbidden_response};
use crate::resolver::{AuthorizedTarget, TargetAuthorizationError};
const MAX_ACTIVE_REQUESTS: usize = 128;
#[derive(Clone)]
pub struct OverlayRequestContext<T> {
    pub policy: NetworkPolicy,
    pub run_id: Option<String>,
    pub execution_id: Option<String>,
    pub session_id: String,
    pub sink: T,
}

/// Optional Host lifecycle for a policy-authorized GET or CONNECT downstream.
/// Waiting is a hint, not guest-idle proof. Implementations own fencing and
/// native resume, including cancellation during either asynchronous method.
// async_trait adds must_use to an already must-use boxed future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait DownloadWait: Send {
    /// Begin/rearm upstream waiting; must not await a future guest pause.
    async fn waiting(&mut self) -> anyhow::Result<()>;
    /// Resume and acknowledge native cleanup before headers/data/errors escape.
    async fn ready(&mut self) -> anyhow::Result<()>;
    /// Nonblocking cleanup on stream/handler drop, including in-flight calls.
    fn cancel(&mut self);
}
pub(crate) struct DownloadGuard(pub(crate) Box<dyn DownloadWait>);
impl Drop for DownloadGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Application sink attached to the generic OverlayNet proxy data plane.
///
/// A sink decides which absolute-URI requests it consumes. Requests it does
/// not accept remain transparent proxy traffic. Relative gateway requests are
/// always delivered to the configured sink. OverlayNet is independent of any
/// concrete sink; an implementation may also compose several downstream sinks.
#[async_trait]
pub trait OverlaySink: Clone + Send + Sync + 'static {
    type RequestContext: Clone + Send + Sync + 'static;

    fn request_context(
        &self,
        request: &Request,
    ) -> anyhow::Result<OverlayRequestContext<Self::RequestContext>>;

    // async_trait adds #[must_use] to the already must-use boxed future.
    #[allow(clippy::double_must_use)]
    async fn handle(
        &self,
        request: Request,
        peer: SocketAddr,
        context: &OverlayRequestContext<Self::RequestContext>,
    ) -> anyhow::Result<Response>;

    /// Called after authorization for forwarded HTTP GET or CONNECT downstream.
    /// CONNECT bytes stay opaque; a transport wait is not a semantic model hint.
    /// Defaults to normal forwarding. Context comes from the bound sink.
    fn download_wait(
        &self,
        _context: &OverlayRequestContext<Self::RequestContext>,
    ) -> anyhow::Result<Option<Box<dyn DownloadWait>>> {
        Ok(None)
    }

    fn accepts(&self, _request: &Request) -> bool {
        false
    }

    fn on_denied(
        &self,
        _context: &OverlayRequestContext<Self::RequestContext>,
        _host: &str,
        _reason: &DenyReason,
    ) {
    }

    fn on_dispatch(
        &self,
        _context: &OverlayRequestContext<Self::RequestContext>,
        _request: &Request,
        _target: &str,
    ) {
    }

    fn on_proxy_result(
        &self,
        _context: &OverlayRequestContext<Self::RequestContext>,
        _target: &str,
        _status: StatusCode,
    ) {
    }

    fn on_proxy_error(
        &self,
        _context: &OverlayRequestContext<Self::RequestContext>,
        _target: &str,
        _error: &str,
    ) {
    }
}

#[derive(Clone)]
pub struct OverlayServerState<S> {
    control_controller: Arc<dyn ControlController>,
    sink: S,
    active_requests: Arc<AtomicUsize>,
    interception_metrics: InterceptionMetrics,
    bandwidth_registry: BandwidthRegistry,
    request_slots: Arc<tokio::sync::Semaphore>,
}

impl<S> OverlayServerState<S>
where
    S: OverlaySink,
{
    pub fn new(
        control_controller: Arc<dyn ControlController>,
        sink: S,
        active_requests: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            control_controller,
            sink,
            active_requests,
            interception_metrics: InterceptionMetrics::default(),
            bandwidth_registry: BandwidthRegistry::default(),
            request_slots: Arc::new(tokio::sync::Semaphore::new(MAX_ACTIVE_REQUESTS)),
        }
    }

    pub fn with_interception_metrics(mut self, metrics: InterceptionMetrics) -> Self {
        self.interception_metrics = metrics;
        self
    }

    pub fn with_bandwidth_registry(mut self, registry: BandwidthRegistry) -> Self {
        self.bandwidth_registry = registry;
        self
    }
}

pub fn build_router<S>(state: OverlayServerState<S>) -> Router
where
    S: OverlaySink,
{
    Router::new()
        .fallback(any(proxy_handler::<S>))
        .with_state(state)
}

async fn proxy_handler<S>(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<OverlayServerState<S>>,
    mut request: Request,
) -> Response
where
    S: OverlaySink,
{
    let Ok(permit) = Arc::clone(&state.request_slots).try_acquire_owned() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "proxy concurrency limit reached",
        )
            .into_response();
    };
    let mut guard = ActiveRequestGuard::new(Arc::clone(&state.active_requests));
    guard._permit = Some(permit);
    let guard = if request.method() == Method::CONNECT {
        request.extensions_mut().insert(Arc::new(guard));
        None
    } else {
        Some(guard)
    };
    state.interception_metrics.request_seen();
    let response = dispatch(&state, request, peer).await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            state.interception_metrics.failure();
            tracing::warn!("overlaynet proxy error: {error:#}");
            (
                StatusCode::BAD_GATEWAY,
                format!("pvisor-overlaynet: {error:#}"),
            )
                .into_response()
        }
    };
    if let Some(guard) = guard {
        let (parts, body) = response.into_parts();
        Response::from_parts(
            parts,
            axum::body::Body::new(body.map_frame(move |frame| {
                let _ = &guard;
                frame
            })),
        )
    } else {
        response
    }
}

async fn dispatch<S>(
    state: &OverlayServerState<S>,
    request: Request,
    peer: SocketAddr,
) -> anyhow::Result<Response>
where
    S: OverlaySink,
{
    let context = state.sink.request_context(&request)?;

    if request.method() == Method::CONNECT {
        state.interception_metrics.connect_request();
        let authority = request
            .uri()
            .authority()
            .map(|authority| authority.to_string())
            .unwrap_or_default();
        let target = match parse_connect_target(&authority) {
            Ok(target) => target,
            Err(error) => {
                state.interception_metrics.failure();
                return Ok((
                    StatusCode::BAD_REQUEST,
                    format!("pvisor-overlaynet: {error}"),
                )
                    .into_response());
            }
        };
        let host = target.host.clone();
        let port = target.port;
        let authorized = match authorize(
            state,
            &context,
            &host,
            Some(target.port),
            NetworkTransport::TcpTunnel,
        )
        .await
        {
            Ok(target) => target,
            Err(TargetAuthorizationError::Denied(reason)) => {
                state.interception_metrics.policy_denied();
                state.interception_metrics.target_denied(
                    &host,
                    target.port,
                    NetworkTransport::TcpTunnel,
                );
                state.sink.on_denied(&context, &host, &reason);
                let (status, message) = forbidden_response(&host, &reason);
                return Ok((status, message).into_response());
            }
            Err(TargetAuthorizationError::Resolve(error)) => {
                state.interception_metrics.target_failed(
                    &host,
                    target.port,
                    NetworkTransport::TcpTunnel,
                );
                return Err(error);
            }
        };
        state.interception_metrics.policy_allowed();
        state
            .interception_metrics
            .target_allowed(&host, target.port, NetworkTransport::TcpTunnel);
        state.sink.on_dispatch(&context, &request, "connect");
        let bandwidth =
            bandwidth_session(state, &context, &host, Some(target.port), Some(&authorized)).await;
        let wait = state.sink.download_wait(&context)?.map(DownloadGuard);
        let response =
            match handle_connect_authorized(request, target, &authorized, bandwidth, wait).await {
                Ok(response) => response,
                Err(error) => {
                    state.interception_metrics.target_failed(
                        &host,
                        port,
                        NetworkTransport::TcpTunnel,
                    );
                    state
                        .sink
                        .on_proxy_error(&context, &authority, &format!("{error:#}"));
                    return Err(error);
                }
            };
        state
            .sink
            .on_proxy_result(&context, &authority, response.status());
        return Ok(response);
    }

    if is_forward_proxy_request(request.method(), request.uri()) {
        state.interception_metrics.absolute_http_request();
        // http::Uri admits authorities that the forwarding client's URL parser
        // rejects (for example an overflowing port). Validate before deriving
        // default ports or allowing a sink to consume the absolute request.
        if let Err(error) = reqwest::Url::parse(&request.uri().to_string()) {
            state.interception_metrics.failure();
            return Ok((
                StatusCode::BAD_REQUEST,
                format!("pvisor-overlaynet: invalid absolute proxy URL: {error}"),
            )
                .into_response());
        }
        // URI authorities retain IPv6 brackets; policy controllers and the
        // tuple resolver take a bare host, just as in the CONNECT path.
        let host = request
            .uri()
            .host()
            .map(|host| {
                host.strip_prefix('[')
                    .and_then(|host| host.strip_suffix(']'))
                    .unwrap_or(host)
                    .to_string()
            })
            .unwrap_or_default();
        let transport = if request.uri().scheme_str() == Some("https") {
            NetworkTransport::Https
        } else {
            NetworkTransport::Http
        };
        let port = request
            .uri()
            .port_u16()
            .unwrap_or(if transport == NetworkTransport::Https {
                443
            } else {
                80
            });
        let authorized = match authorize(state, &context, &host, Some(port), transport).await {
            Ok(target) => target,
            Err(TargetAuthorizationError::Denied(reason)) => {
                state.interception_metrics.policy_denied();
                state
                    .interception_metrics
                    .target_denied(&host, port, transport);
                state.sink.on_denied(&context, &host, &reason);
                let (status, message) = forbidden_response(&host, &reason);
                return Ok((status, message).into_response());
            }
            Err(TargetAuthorizationError::Resolve(error)) => {
                state
                    .interception_metrics
                    .target_failed(&host, port, transport);
                return Err(error);
            }
        };
        state.interception_metrics.policy_allowed();
        state
            .interception_metrics
            .target_allowed(&host, port, transport);
        let bandwidth =
            bandwidth_session(state, &context, &host, Some(port), Some(&authorized)).await;
        if state.sink.accepts(&request) {
            state.interception_metrics.sink_request();
            state.sink.on_dispatch(&context, &request, "sink");
            let request = throttle_request(request, bandwidth.clone());
            let response = state
                .sink
                .handle(request, peer, &context)
                .await
                .inspect_err(|_| {
                    state
                        .interception_metrics
                        .target_failed(&host, port, transport);
                })?;
            return Ok(throttle_response(response, bandwidth));
        }
        state.sink.on_dispatch(&context, &request, "forward");
        let target = request.uri().to_string();
        let wait = if request.method() == Method::GET {
            state.sink.download_wait(&context)?.map(DownloadGuard)
        } else {
            None
        };
        let response =
            match transparent_forward_authorized(request, &authorized, bandwidth, wait).await {
                Ok(response) => response.into_response(),
                Err(error) => {
                    state
                        .interception_metrics
                        .target_failed(&host, port, transport);
                    state
                        .sink
                        .on_proxy_error(&context, &target, &format!("{error:#}"));
                    return Err(error);
                }
            };
        state
            .sink
            .on_proxy_result(&context, &target, response.status());
        return Ok(response);
    }

    state.interception_metrics.sink_request();
    state.sink.on_dispatch(&context, &request, "sink");
    let bandwidth = bandwidth_session(state, &context, "", None, None).await;
    let request = throttle_request(request, bandwidth.clone());
    let response = state.sink.handle(request, peer, &context).await?;
    Ok(throttle_response(response, bandwidth))
}

async fn bandwidth_session<S>(
    state: &OverlayServerState<S>,
    context: &OverlayRequestContext<S::RequestContext>,
    host: &str,
    port: Option<u16>,
    authorized: Option<&AuthorizedTarget>,
) -> BandwidthSession
where
    S: OverlaySink,
{
    state
        .bandwidth_registry
        .session(context.policy.matching_limits(
            host,
            port,
            authorized.map_or(&[], |target| target.addresses.as_slice()),
        ))
        .await
}

fn throttle_request(request: Request, bandwidth: BandwidthSession) -> Request {
    let (parts, body) = request.into_parts();
    Request::from_parts(parts, throttle_body(body, bandwidth))
}

fn throttle_response(response: Response, bandwidth: BandwidthSession) -> Response {
    let (parts, body) = response.into_parts();
    Response::from_parts(parts, throttle_body(body, bandwidth))
}

async fn authorize<S>(
    state: &OverlayServerState<S>,
    context: &OverlayRequestContext<S::RequestContext>,
    host: &str,
    port: Option<u16>,
    transport: NetworkTransport,
) -> Result<crate::resolver::AuthorizedTarget, TargetAuthorizationError>
where
    S: OverlaySink,
{
    // A host DNS/TUN connector may resolve an allowed hostname to a fake
    // 198.18/15 address. Apply the same narrowly scoped alias handling used
    // by the VM egress path; IP literals and explicit address denies still
    // remain subject to the ordinary policy.
    crate::resolver::authorize_target_with_policy(
        state.control_controller.as_ref(),
        &context.policy,
        NetworkAccessRequest {
            run_id: context.run_id.clone().map(RunId),
            execution_id: context.execution_id.clone().map(pvisor_core::ExecutionId),
            host: host.to_string(),
            port,
            transport,
            resolved_ip: None,
        },
        crate::resolver::ResolvedAddressPolicy::HostConnectorAliases,
    )
    .await
}

pub(crate) struct ActiveRequestGuard {
    counter: Arc<AtomicUsize>,
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl ActiveRequestGuard {
    fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self {
            counter,
            _permit: None,
        }
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{NetworkConfig, NetworkMode};
    use pvisor_core::PolicyControlController;

    struct FailedResume;

    #[async_trait]
    impl DownloadWait for FailedResume {
        async fn waiting(&mut self) -> anyhow::Result<()> {
            Ok(())
        }

        async fn ready(&mut self) -> anyhow::Result<()> {
            anyhow::bail!("native resume failed")
        }

        fn cancel(&mut self) {}
    }

    #[tokio::test]
    async fn connect_resume_failure_cancels_backpressured_upload_and_returns_admission() {
        connect_forwarding_ownership(true).await;
    }

    #[tokio::test]
    async fn connect_half_close_drains_backpressured_response_and_returns_admission() {
        connect_forwarding_ownership(false).await;
    }

    #[tokio::test]
    async fn http2_connect_upstream_half_close_preserves_upload_and_admission() {
        use hyper_util::rt::{TokioExecutor, TokioIo};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::time::{Duration, timeout};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let counter = Arc::new(AtomicUsize::new(0));
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let service_counter = counter.clone();
        let service_slots = slots.clone();
        let service =
            hyper::service::service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                let counter = service_counter.clone();
                let slots = service_slots.clone();
                async move {
                    let mut request = request.map(axum::body::Body::new);
                    let mut guard = ActiveRequestGuard::new(counter);
                    guard._permit = Some(slots.try_acquire_owned().unwrap());
                    request.extensions_mut().insert(Arc::new(guard));
                    handle_connect_authorized(
                        request,
                        parse_connect_target(&address.to_string()).unwrap(),
                        &AuthorizedTarget {
                            host: address.ip().to_string(),
                            addresses: vec![address],
                        },
                        BandwidthRegistry::default().session(vec![]).await,
                        None,
                    )
                    .await
                    .map_err(std::io::Error::other)
                }
            });
        let (client, server) = tokio::io::duplex(1024);
        let server_task = tokio::spawn(async move {
            hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(server), service)
                .await
        });
        let (mut sender, connection) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(client))
                .await
                .unwrap();
        let client_task = tokio::spawn(connection);
        let request = Request::builder()
            .method(Method::CONNECT)
            .uri(address.to_string())
            .body(axum::body::Body::empty())
            .unwrap();
        let mut response = timeout(Duration::from_secs(2), sender.send_request(request))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let upgraded = timeout(Duration::from_secs(2), hyper::upgrade::on(&mut response))
            .await
            .unwrap()
            .unwrap();
        // Hyper's HTTP/2 CONNECT body is empty; the tunnel belongs to the
        // stream-level upgrade, not the response body.
        drop(response);
        let mut tunnel = TokioIo::new(upgraded);
        let (mut upstream, _) = timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        upstream.write_all(b"accepted response").await.unwrap();
        upstream.shutdown().await.unwrap();
        let mut downloaded = Vec::new();
        timeout(Duration::from_secs(2), tunnel.read_to_end(&mut downloaded))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(downloaded, b"accepted response");
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        assert_eq!(slots.available_permits(), 0);
        // Response EOF is only a half-close. Upload is still usable after
        // the accepted response has been drained.
        tunnel
            .write_all(b"upload after response EOF")
            .await
            .unwrap();
        tunnel.shutdown().await.unwrap();
        let mut uploaded = Vec::new();
        timeout(Duration::from_secs(2), upstream.read_to_end(&mut uploaded))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(uploaded, b"upload after response EOF");
        timeout(Duration::from_secs(2), async {
            while counter.load(Ordering::Relaxed) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(slots.available_permits(), 1);
        drop(tunnel);
        drop(sender);
        client_task.abort();
        let _ = client_task.await;
        server_task.abort();
        let _ = server_task.await;
    }

    async fn connect_forwarding_ownership(fail_resume: bool) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::time::{Duration, timeout};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let counter = Arc::new(AtomicUsize::new(0));
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let bandwidth = BandwidthRegistry::default()
            .session(if fail_resume {
                vec![crate::policy::NetworkBandwidthLimit {
                    host: None,
                    port: None,
                    bytes_per_second: 1,
                }]
            } else {
                vec![]
            })
            .await;
        let (mut client, server) = tokio::io::duplex(1024);
        let service_counter = counter.clone();
        let service_slots = slots.clone();
        let service =
            hyper::service::service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                let counter = service_counter.clone();
                let slots = service_slots.clone();
                let bandwidth = bandwidth.clone();
                async move {
                    let mut request = request.map(axum::body::Body::new);
                    let mut guard = ActiveRequestGuard::new(counter);
                    guard._permit = Some(slots.try_acquire_owned().unwrap());
                    request.extensions_mut().insert(Arc::new(guard));
                    let target = parse_connect_target(&address.to_string()).unwrap();
                    let authorized = AuthorizedTarget {
                        host: address.ip().to_string(),
                        addresses: vec![address],
                    };
                    handle_connect_authorized(
                        request,
                        target,
                        &authorized,
                        bandwidth,
                        fail_resume.then(|| DownloadGuard(Box::new(FailedResume))),
                    )
                    .await
                    .map_err(std::io::Error::other)
                }
            });
        let connection = tokio::spawn(async move {
            hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(server), service)
                .with_upgrades()
                .await
        });
        client
            .write_all(format!("CONNECT {address} HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let (mut upstream, _) = timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut headers = Vec::new();
        timeout(Duration::from_secs(2), async {
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(client.read_u8().await.unwrap());
            }
        })
        .await
        .unwrap();
        assert!(headers.starts_with(b"HTTP/1.1 200"));
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        assert_eq!(slots.available_permits(), 0);
        client.write_all(&[b'u'; 64]).await.unwrap();
        if fail_resume {
            // This accepted upload cannot finish its one-byte/second throttle
            // within the deadline. A downstream resume failure is terminal,
            // unlike successful EOF, and must cancel this companion copy.
            upstream.write_all(b"accepted response").await.unwrap();
        } else {
            client.shutdown().await.unwrap();
            let mut upload = Vec::new();
            timeout(Duration::from_secs(2), upstream.read_to_end(&mut upload))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(upload, [b'u'; 64]);
            // Upstream accepted the response before closing its write half.
            // It exceeds the client's 1 KiB buffer: successful upload EOF
            // must not discard a download waiting for the client to drain.
            let response = vec![b'r'; 32 * 1024];
            upstream.write_all(&response).await.unwrap();
            upstream.shutdown().await.unwrap();
            assert_eq!(counter.load(Ordering::Relaxed), 1);
            assert_eq!(slots.available_permits(), 0);
            let mut downloaded = Vec::new();
            timeout(Duration::from_secs(2), client.read_to_end(&mut downloaded))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(downloaded, response);
        }
        let released = timeout(Duration::from_secs(2), async {
            while counter.load(Ordering::Relaxed) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        // Always close our peers, including on the failing reproduction.
        drop(client);
        drop(upstream);
        timeout(Duration::from_secs(2), connection)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            released.is_ok(),
            "completed or failed tunnel retained admission"
        );
        assert_eq!(slots.available_permits(), 1);
    }

    #[tokio::test]
    async fn overload_is_rejected_and_response_body_drop_returns_the_slot() {
        let state = OverlayServerState::new(
            Arc::new(PolicyControlController),
            PolicySink,
            Arc::new(AtomicUsize::new(0)),
        );
        let occupied = Arc::clone(&state.request_slots)
            .acquire_many_owned(MAX_ACTIVE_REQUESTS as u32)
            .await
            .unwrap();
        let request = || {
            Request::builder()
                .uri("http://example.com/")
                .body(axum::body::Body::empty())
                .unwrap()
        };
        let peer = "127.0.0.1:40000".parse().unwrap();
        let response = proxy_handler(ConnectInfo(peer), State(state.clone()), request()).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(state.active_requests.load(Ordering::Relaxed), 0);
        drop(occupied);
        let response = proxy_handler(ConnectInfo(peer), State(state.clone()), request()).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            state.request_slots.available_permits(),
            MAX_ACTIVE_REQUESTS - 1
        );
        drop(response);
        assert_eq!(state.request_slots.available_permits(), MAX_ACTIVE_REQUESTS);
        assert_eq!(state.active_requests.load(Ordering::Relaxed), 0);
    }

    #[derive(Clone)]
    struct EventSink;

    #[async_trait]
    impl OverlaySink for EventSink {
        type RequestContext = ();

        fn request_context(
            &self,
            _request: &Request,
        ) -> anyhow::Result<OverlayRequestContext<Self::RequestContext>> {
            unreachable!("selection does not require a request context")
        }

        async fn handle(
            &self,
            _request: Request,
            _peer: SocketAddr,
            _context: &OverlayRequestContext<Self::RequestContext>,
        ) -> anyhow::Result<Response> {
            unreachable!("selection does not dispatch the request")
        }

        fn accepts(&self, request: &Request) -> bool {
            request.uri().path().starts_with("/events/")
        }
    }

    #[test]
    fn response_body_owns_the_active_request_guard() {
        let counter = Arc::new(AtomicUsize::new(0));
        let guard = ActiveRequestGuard::new(Arc::clone(&counter));
        let body = axum::body::Body::new(axum::body::Body::empty().map_frame(move |frame| {
            let _ = &guard;
            frame
        }));
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        drop(body);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn active_request_guard_decrements_on_drop() {
        let counter = Arc::new(AtomicUsize::new(0));
        {
            let _guard = ActiveRequestGuard::new(Arc::clone(&counter));
            assert_eq!(counter.load(Ordering::Relaxed), 1);
        }
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn overlaynet_accepts_non_gateway_sink_implementations() {
        let accepted = Request::builder()
            .uri("http://collector.example/events/agent")
            .body(axum::body::Body::empty())
            .expect("valid request");
        let forwarded = Request::builder()
            .uri("http://collector.example/health")
            .body(axum::body::Body::empty())
            .expect("valid request");

        assert!(EventSink.accepts(&accepted));
        assert!(!EventSink.accepts(&forwarded));
    }

    #[derive(Clone)]
    struct PolicySink;

    #[async_trait]
    impl OverlaySink for PolicySink {
        type RequestContext = ();

        fn request_context(
            &self,
            _request: &Request,
        ) -> anyhow::Result<OverlayRequestContext<Self::RequestContext>> {
            Ok(OverlayRequestContext {
                policy: NetworkPolicy::compile(&NetworkConfig {
                    capability: None,
                    mode: NetworkMode::NoNetwork,
                    allowed_hosts: Vec::new(),
                    rules: Vec::new(),
                    deny_rules: Vec::new(),
                    limits: Vec::new(),
                })?,
                run_id: Some("run-1".into()),
                execution_id: Some("execution-1".into()),
                session_id: "session-1".into(),
                sink: (),
            })
        }

        async fn handle(
            &self,
            _request: Request,
            _peer: SocketAddr,
            _context: &OverlayRequestContext<Self::RequestContext>,
        ) -> anyhow::Result<Response> {
            unreachable!("denied requests do not reach the sink")
        }
    }

    #[test]
    fn denied_proxy_requests_update_interception_metrics() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let metrics = InterceptionMetrics::default();
            let state = OverlayServerState::new(
                Arc::new(PolicyControlController),
                PolicySink,
                Arc::new(AtomicUsize::new(0)),
            )
            .with_interception_metrics(metrics.clone());
            let request = Request::builder()
                .method(Method::CONNECT)
                .uri("example.com:443")
                .body(axum::body::Body::empty())
                .unwrap();

            metrics.request_seen();
            let response = dispatch(&state, request, "127.0.0.1:40000".parse().unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            let snapshot = metrics.snapshot();
            assert_eq!(snapshot.requests_seen, 1);
            assert_eq!(snapshot.connect_requests, 1);
            assert_eq!(snapshot.policy_denied, 1);
            assert_eq!(snapshot.policy_allowed, 0);
            assert_eq!(snapshot.targets["TCP example.com:443"].denied, 1);
        });
    }

    #[tokio::test]
    async fn ipv6_connect_preserves_the_parsed_host_through_authorization() {
        let metrics = InterceptionMetrics::default();
        let state = OverlayServerState::new(
            Arc::new(PolicyControlController),
            PolicySink,
            Arc::new(AtomicUsize::new(0)),
        )
        .with_interception_metrics(metrics.clone());
        for authority in ["[::1]:443", "[2001:db8::10]:8443"] {
            let request = Request::builder()
                .method(Method::CONNECT)
                .uri(authority)
                .body(axum::body::Body::empty())
                .unwrap();
            let response = dispatch(&state, request, "127.0.0.1:40000".parse().unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        let targets = metrics.snapshot().targets;
        assert!(targets.keys().any(|key| key.contains("::1")), "{targets:?}");
        assert!(
            targets.keys().any(|key| key.contains("2001:db8::10")),
            "{targets:?}"
        );
        assert_eq!(targets.len(), 2);
    }

    #[derive(Clone)]
    struct FailingContextSink;

    #[async_trait]
    impl OverlaySink for FailingContextSink {
        type RequestContext = ();

        fn request_context(
            &self,
            _request: &Request,
        ) -> anyhow::Result<OverlayRequestContext<Self::RequestContext>> {
            anyhow::bail!("synthetic context failure")
        }

        async fn handle(
            &self,
            _request: Request,
            _peer: SocketAddr,
            _context: &OverlayRequestContext<Self::RequestContext>,
        ) -> anyhow::Result<Response> {
            unreachable!()
        }
    }

    #[test]
    fn handler_turns_internal_failures_into_502_and_releases_active_count() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let active = Arc::new(AtomicUsize::new(0));
            let metrics = InterceptionMetrics::default();
            let state = OverlayServerState::new(
                Arc::new(PolicyControlController),
                FailingContextSink,
                Arc::clone(&active),
            )
            .with_interception_metrics(metrics.clone());
            let request = Request::builder()
                .uri("/events")
                .body(axum::body::Body::empty())
                .unwrap();
            let response = proxy_handler(
                ConnectInfo("127.0.0.1:40000".parse().unwrap()),
                State(state),
                request,
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            assert_eq!(active.load(Ordering::Relaxed), 1);
            drop(response);
            assert_eq!(active.load(Ordering::Relaxed), 0);
            assert_eq!(metrics.snapshot().requests_seen, 1);
            assert_eq!(metrics.snapshot().failures, 1);
        });
    }

    #[test]
    fn malformed_connect_bracket_suffix_is_400_before_authorization() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let metrics = InterceptionMetrics::default();
            let state = OverlayServerState::new(
                Arc::new(PolicyControlController),
                PolicySink,
                Arc::new(AtomicUsize::new(0)),
            )
            .with_interception_metrics(metrics.clone());
            for authority in ["[::1]suffix:8443", "[2001:db8::1]443"] {
                let request = Request::builder()
                    .method(Method::CONNECT)
                    .uri(authority)
                    .body(axum::body::Body::empty())
                    .unwrap();
                let response = dispatch(&state, request, "127.0.0.1:40000".parse().unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{authority}");
            }
            let snapshot = metrics.snapshot();
            assert_eq!(snapshot.failures, 2);
            assert_eq!(snapshot.policy_allowed, 0);
            assert_eq!(snapshot.policy_denied, 0);
        });
    }

    #[test]
    fn absolute_ipv6_uses_a_bare_ip_through_authorization_and_forwarding() {
        #[derive(Clone)]
        struct LiteralSink {
            port: u16,
            deny: bool,
        }

        #[async_trait]
        impl OverlaySink for LiteralSink {
            type RequestContext = ();

            fn request_context(
                &self,
                _request: &Request,
            ) -> anyhow::Result<OverlayRequestContext<()>> {
                let rule = crate::policy::NetworkAccessRule {
                    host: "::1".into(),
                    ports: vec![self.port],
                    transports: vec![NetworkTransport::Http],
                    allow_private_ips: false,
                };
                Ok(OverlayRequestContext {
                    policy: NetworkPolicy::compile(&NetworkConfig {
                        mode: NetworkMode::Allowlist,
                        rules: vec![rule.clone()],
                        deny_rules: if self.deny { vec![rule] } else { Vec::new() },
                        ..NetworkConfig::default()
                    })?,
                    run_id: None,
                    execution_id: None,
                    session_id: "ipv6-literal-regression".into(),
                    sink: (),
                })
            }

            async fn handle(
                &self,
                _request: Request,
                _peer: SocketAddr,
                _context: &OverlayRequestContext<()>,
            ) -> anyhow::Result<Response> {
                unreachable!("literal traffic must use transparent forwarding")
            }
        }

        struct LiteralController(u16);
        impl ControlController for LiteralController {
            fn authorize(
                &self,
                request: pvisor_core::ControlRequest<'_>,
            ) -> pvisor_core::ControlTransition {
                if let pvisor_core::ControlRequest::Network { request, .. } = &request {
                    // Fail before resolution if URI brackets leaked into the
                    // logical host. This also prevents accidental DNS in the repro.
                    assert_eq!(
                        request.host.parse::<std::net::IpAddr>().unwrap(),
                        "::1".parse::<std::net::IpAddr>().unwrap()
                    );
                    assert_eq!(request.port, Some(self.0));
                    assert_eq!(request.transport, NetworkTransport::Http);
                    if let Some(ip) = request.resolved_ip {
                        assert_eq!(ip, "::1".parse::<std::net::IpAddr>().unwrap());
                    }
                }
                PolicyControlController.authorize(request)
            }
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            use tokio::time::{Duration, timeout};

            let listener = tokio::net::TcpListener::bind("[::1]:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let mut server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 4096];
                assert!(socket.read(&mut buffer).await.unwrap() > 0);
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await
                    .unwrap();
            });
            for deny in [false, true] {
                let state = OverlayServerState::new(
                    Arc::new(LiteralController(port)),
                    LiteralSink { port, deny },
                    Arc::new(AtomicUsize::new(0)),
                );
                let request = Request::builder()
                    .uri(format!("http://[0:0:0:0:0:0:0:1]:{port}/"))
                    .header("host", "other.invalid")
                    .body(axum::body::Body::empty())
                    .unwrap();
                let response = timeout(
                    Duration::from_secs(3),
                    dispatch(&state, request, "[::1]:40000".parse().unwrap()),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(
                    response.status(),
                    if deny {
                        StatusCode::FORBIDDEN
                    } else {
                        StatusCode::OK
                    }
                );
                if !deny {
                    let body = timeout(Duration::from_secs(3), response.into_body().collect())
                        .await
                        .unwrap()
                        .unwrap()
                        .to_bytes();
                    assert_eq!(body, "ok");
                }
            }
            let result = timeout(Duration::from_secs(3), &mut server).await;
            server.abort();
            result.unwrap().unwrap();
        });
    }

    #[test]
    fn malformed_absolute_url_cannot_dispatch_to_an_accepting_sink() {
        #[derive(Clone)]
        struct LocalSink;

        #[async_trait]
        impl OverlaySink for LocalSink {
            type RequestContext = ();

            fn request_context(
                &self,
                _request: &Request,
            ) -> anyhow::Result<OverlayRequestContext<()>> {
                Ok(OverlayRequestContext {
                    policy: NetworkPolicy::compile(&NetworkConfig {
                        mode: NetworkMode::Allowlist,
                        rules: vec![crate::policy::NetworkAccessRule {
                            host: "127.0.0.1".into(),
                            ports: vec![80],
                            transports: vec![NetworkTransport::Http],
                            allow_private_ips: true,
                        }],
                        ..NetworkConfig::default()
                    })?,
                    run_id: None,
                    execution_id: None,
                    session_id: "absolute-url-regression".into(),
                    sink: (),
                })
            }

            fn accepts(&self, _request: &Request) -> bool {
                true
            }

            async fn handle(
                &self,
                _request: Request,
                _peer: SocketAddr,
                _context: &OverlayRequestContext<()>,
            ) -> anyhow::Result<Response> {
                Ok(StatusCode::OK.into_response())
            }
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let metrics = InterceptionMetrics::default();
            let state = OverlayServerState::new(
                Arc::new(PolicyControlController),
                LocalSink,
                Arc::new(AtomicUsize::new(0)),
            )
            .with_interception_metrics(metrics.clone());
            // Numeric IP resolution is entirely in process: no DNS or sockets.
            for (uri, status) in [
                ("http://127.0.0.1:65536/", StatusCode::BAD_REQUEST),
                ("http://127.0.0.1:00080/", StatusCode::OK),
            ] {
                let request = Request::builder()
                    .uri(uri)
                    .header("host", "other.invalid:443")
                    .body(axum::body::Body::empty())
                    .unwrap();
                let response = dispatch(&state, request, "127.0.0.1:40000".parse().unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), status, "{uri}");
            }
            let snapshot = metrics.snapshot();
            assert_eq!(snapshot.policy_allowed, 1);
            assert_eq!(snapshot.targets["HTTP 127.0.0.1:80"].allowed, 1);
        });
    }

    #[test]
    fn malformed_absolute_url_is_400_before_authorization() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let metrics = InterceptionMetrics::default();
            let state = OverlayServerState::new(
                Arc::new(PolicyControlController),
                PolicySink,
                Arc::new(AtomicUsize::new(0)),
            )
            .with_interception_metrics(metrics.clone());
            let mut malformed = vec![
                "http://example.invalid:65536/".to_string(),
                "https://example.invalid:99999999999999999999/".to_string(),
                "http://[::1]suffix:8080/".to_string(),
            ];
            let mut seed = 0x5552_4c32_u32;
            for _ in 0..32 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let port = 65_536_u64 + u64::from(seed);
                malformed.push(format!("http://example.invalid:{port}/"));
            }
            for uri in &malformed {
                let request = Request::builder()
                    .uri(uri.as_str())
                    .body(axum::body::Body::empty())
                    .unwrap();
                let response = dispatch(&state, request, "127.0.0.1:40000".parse().unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            }
            let snapshot = metrics.snapshot();
            assert_eq!(snapshot.failures, malformed.len() as u64);
            assert_eq!(snapshot.policy_allowed, 0);
            assert_eq!(snapshot.policy_denied, 0);
            assert!(snapshot.targets.is_empty());
            // Legal URL spellings still proceed to policy, which denies here
            // before resolution. Host headers never select the target.
            for uri in [
                "HTTP://EXAMPLE.invalid/",
                "https://example.invalid:443/",
                "http://example.invalid:/",
                "http://example.invalid:00080/",
                "http://user%40name:pass@example.invalid/",
                "http://[2001:0db8::1]:8080/",
                "http://2130706433/",
                "http://example.invalid/a%20b",
            ] {
                let request = Request::builder()
                    .uri(uri)
                    .header("host", "other.invalid:443")
                    .body(axum::body::Body::empty())
                    .unwrap();
                let response = dispatch(&state, request, "127.0.0.1:40000".parse().unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
            }
            assert_eq!(metrics.snapshot().policy_denied, 8);
        });
    }

    #[test]
    fn malformed_connect_is_400_and_never_reaches_policy() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let metrics = InterceptionMetrics::default();
            let state = OverlayServerState::new(
                Arc::new(PolicyControlController),
                PolicySink,
                Arc::new(AtomicUsize::new(0)),
            )
            .with_interception_metrics(metrics.clone());
            let request = Request::builder()
                .method(Method::CONNECT)
                .uri("example.com:65536")
                .body(axum::body::Body::empty())
                .unwrap();
            let response = dispatch(&state, request, "127.0.0.1:40000".parse().unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let snapshot = metrics.snapshot();
            assert_eq!(snapshot.connect_requests, 1);
            assert_eq!(snapshot.failures, 1);
            assert_eq!(snapshot.policy_allowed, 0);
            assert_eq!(snapshot.policy_denied, 0);
        });
    }
}
