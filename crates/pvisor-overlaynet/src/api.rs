//! OverlayNet public contracts for explicit HTTP proxying and framed VM networking.
//!
//! Core owns policy definitions; import them from `pvisor_core::network` until its
//! own API migration. Proxy interception is cooperative; VM IPv4 TCP/DNS egress
//! is non-bypassable only when installed as the workload's network path. General
//! UDP/IPv6/ICMP egress, QUIC and inbound forwarding are unsupported and fail closed.
//! Explicit private peer links separately support IPv4 TCP/UDP/ICMP and targeted ARP.
//! Owners retain private state. Cloneable handles share accounting or policy;
//! owned snapshots/configurations are values, not installation or completion proofs.

use async_trait::async_trait;
use axum::{Router, extract::Request, http::StatusCode, response::Response};
use pvisor_core::{
    ControlController,
    network::{DenyReason, NetworkPolicy, ResolvedAddressPolicy},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{Arc, atomic::AtomicUsize},
};
use tokio::io::{AsyncRead, AsyncWrite};

pub use crate::bandwidth::BandwidthRegistry;
pub use crate::egress::EgressRuntime;
pub use crate::interception::InterceptionMetrics;
pub use crate::server::OverlayServerState;
pub use crate::vm::VmNetwork;

/// Default guest router address; peer configurations may select another /24.
pub const ROUTER_IPV4: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
/// Default guest address when no peer membership is configured.
pub const GUEST_IPV4: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);

/// Driver identity recorded independently of requested egress policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InterceptionDriver {
    /// Cooperative HTTP proxy; direct sockets can bypass it.
    ExplicitProxy,
    /// Virtio-net IPv4 TCP/DNS data plane with policy-authorized host connectors.
    VmSmoltcp,
    /// Reserved Linux namespace driver identity; no driver is implemented here.
    LinuxNetns,
    /// Reserved Linux seccomp-notify driver identity; no driver is implemented here.
    LinuxSeccompNotify,
}

/// Whether the recorded driver prevents workload bypass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InterceptionStrength {
    /// Only traffic using the configured proxy is covered.
    Cooperative,
    /// The configured workload network path cannot bypass the driver.
    NonBypassable,
}

/// Serializable coverage description, not an installation receipt. Policy grants remain separate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterceptionProfile {
    /// Recorded driver kind.
    pub driver: InterceptionDriver,
    /// Bypass resistance of the selected path.
    pub strength: InterceptionStrength,
    /// Whether HTTP requests are classified.
    pub http: bool,
    /// Whether outbound TCP connection admission is covered.
    pub tcp_connect: bool,
    /// Whether guest DNS is handled by the driver.
    pub dns: bool,
    /// General UDP egress support; local DNS/DHCP responses do not imply UDP egress.
    pub udp: bool,
    /// Whether descendant traffic stays on the same network path.
    pub inherited_by_children: bool,
    #[serde(default)]
    /// Explicit same-network IPv4 TCP/UDP/ICMP peer coverage; defaults false in old records.
    pub peer_ipv4: bool,
}

/// Owned traffic observation. Default values are zero/empty and old records use those defaults.
/// Concurrent sampling is not a transaction across counters; cooperative bypass traffic is absent.
///
/// Old records retain their counters and default missing fields, including nested targets.
///
/// ```
/// use pvisor_overlaynet::api::{InterceptionSnapshot, TargetCounters};
///
/// let snapshot: InterceptionSnapshot = serde_json::from_str(
///     r#"{"requests_seen":3,"targets":{"HTTP example.com:80":{"denied":2}}}"#,
/// )?;
/// assert_eq!(snapshot.requests_seen, 3);
/// assert_eq!(snapshot.policy_allowed, 0);
/// assert_eq!(snapshot.targets["HTTP example.com:80"], TargetCounters {
///     denied: 2, ..Default::default()
/// });
/// assert_eq!(snapshot.peer_packets_received, 0);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterceptionSnapshot {
    /// Proxy requests observed.
    pub requests_seen: u64,
    /// Proxy policy admissions.
    pub policy_allowed: u64,
    /// Proxy or VM policy denials.
    pub policy_denied: u64,
    /// Proxy CONNECT requests.
    pub connect_requests: u64,
    /// Absolute-URI proxy requests.
    pub absolute_http_requests: u64,
    /// Requests dispatched to the configured sink.
    pub sink_requests: u64,
    /// Observed proxy/VM errors.
    pub failures: u64,
    /// Guest DNS queries observed.
    pub dns_queries: u64,
    /// Guest DNS answers generated.
    pub dns_answers: u64,
    /// VM TCP flows opened.
    pub tcp_flows_opened: u64,
    /// VM TCP flows denied.
    pub tcp_flows_denied: u64,
    /// Failed VM upstream TCP connections.
    pub tcp_connect_failures: u64,
    /// Counted bytes forwarded from the guest.
    pub bytes_guest_to_host: u64,
    /// Counted bytes forwarded toward the guest.
    pub bytes_host_to_guest: u64,
    /// Guest packets rejected as unsupported.
    pub unsupported_packets: u64,
    #[serde(default)]
    /// Peer Ethernet frames accepted for send, including ARP.
    pub peer_packets_sent: u64,
    #[serde(default)]
    /// Validated peer frames queued toward this guest.
    pub peer_packets_received: u64,
    #[serde(default)]
    /// Peer frames dropped for identity, policy, absence or congestion.
    pub peer_packets_dropped: u64,
    /// Currently observed VM TCP flow count.
    pub active_tcp_flows: u64,
    /// Peak observed simultaneous VM TCP flow count.
    pub peak_tcp_flows: u64,
    /// At most 256 normalized destination observations; not a complete access history.
    pub targets: BTreeMap<String, TargetCounters>,
    /// Destination observations omitted or displaced by the bounded target inventory.
    pub target_overflow: u64,
}

/// Owned destination decision counts; default and missing fields are zero.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetCounters {
    /// Observed allowed decisions.
    pub allowed: u64,
    /// Observed denied decisions.
    pub denied: u64,
    /// Observed connection/dispatch failures.
    pub failed: u64,
}

/// Owned attribution attached to VM authorization requests; defaults omit both identities.
#[derive(Debug, Clone, Default)]
pub struct EgressContext {
    /// Optional run identity; does not authorize access.
    pub run_id: Option<String>,
    /// Optional current execution identity; does not authorize access.
    pub execution_id: Option<String>,
}

/// Execution-local explicit Gateway route on the virtual router.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmGatewayRoute {
    /// TCP port exposed on the guest virtual router.
    pub guest_port: u16,
    /// Host listener reached for that internal route.
    pub host: SocketAddr,
}

/// Owned startup inputs, consumed by the network owner. Construct with VmNetworkConfiguration.
/// The caller supplies policy, identity and private peer authority; driver labels do not create grants.
#[derive(Clone)]
pub struct VmNetworkConfig {
    /// Cloneable policy/controller/bandwidth owner; live replacements affect subsequent admissions.
    pub egress: EgressRuntime,
    /// Run/execution attribution for egress authorization.
    pub context: EgressContext,
    /// Optional internal Gateway route; absent by default.
    pub gateway: Option<VmGatewayRoute>,
    /// Positive simultaneous TCP flow limit; default 1024 with two 64 KiB buffers per flow. Zero is rejected before transport adoption.
    pub max_flows: usize,
    /// Shared counters; defaults to a fresh independent counter owner.
    pub metrics: InterceptionMetrics,
    /// Optional private peer membership. Requires unrestricted peer-capable egress; a restricted policy or custom controller is rejected.
    pub peer_network: Option<PeerNetworkConfig>,
}

/// Explicit same-host IPv4 packet membership. The caller allocates the address and must
/// hide the directory from guests. Unix requires an owned canonical 0700 directory;
/// Windows requires inherited caller/SYSTEM-only ACLs. Membership is validated at startup.
/// Peer traffic includes unicast TCP/UDP/ICMP and targeted ARP; it does not grant host egress.
#[derive(Debug, Clone)]
pub struct PeerNetworkConfig {
    /// Existing canonical absolute private network directory; distinct directories isolate membership.
    pub directory: PathBuf,
    /// Usable non-gateway host address in the gateway /24.
    pub address: Ipv4Addr,
    /// Usable router in the same private IPv4 /24 or 192.0.2.0/24.
    pub gateway: Ipv4Addr,
    /// At most 16 nonempty DNS names, each at most 128 bytes with 1–63-byte alphanumeric/dash/underscore labels. Empty disables peer-name DNS.
    pub aliases: Vec<String>,
}

/// Request-local policy and attribution returned by the bound sink. Policies are explicit
/// inputs to admission; session identifiers alone confer no authority.
#[derive(Clone)]
pub struct OverlayRequestContext<T> {
    /// Compiled Core policy snapshot used for this request.
    pub policy: NetworkPolicy,
    /// Optional run identity for control requests.
    pub run_id: Option<String>,
    /// Optional current execution identity for control requests.
    pub execution_id: Option<String>,
    /// Sink correlation identity; not an authorization token.
    pub session_id: String,
    /// Owned sink-specific request context, cloned only under the sink contract.
    pub sink: T,
}

/// Coverage profiles for implemented drivers; no I/O or installation occurs.
///
/// ```
/// use pvisor_overlaynet::api::{
///     InterceptionCoverage, InterceptionDriver, InterceptionProfile, InterceptionStrength,
/// };
///
/// let proxy = InterceptionProfile::explicit_proxy();
/// assert_eq!(proxy.driver, InterceptionDriver::ExplicitProxy);
/// assert_eq!(proxy.strength, InterceptionStrength::Cooperative);
/// assert!(proxy.http && proxy.tcp_connect);
/// assert!(!proxy.dns && !proxy.udp && !proxy.inherited_by_children);
/// let vm = InterceptionProfile::vm_smoltcp();
/// assert_eq!(vm.strength, InterceptionStrength::NonBypassable);
/// assert!(vm.dns && vm.tcp_connect && vm.inherited_by_children);
/// assert!(!vm.http && !vm.udp && !vm.peer_ipv4);
/// ```
pub trait InterceptionCoverage: Sized {
    /// Cooperative HTTP/CONNECT coverage with no general DNS/UDP/child enforcement.
    fn explicit_proxy() -> Self;
    /// Non-bypassable IPv4 TCP/DNS profile, with no HTTP decoding or general UDP.
    fn vm_smoltcp() -> Self;
}

/// Shared traffic-counter observation; cloneable owners refer to the same counters.
pub trait InterceptionObservation {
    /// Return an independent owned snapshot. Counter reads can span concurrent updates;
    /// destination inventory is bounded and traffic bypassing a cooperative proxy is absent.
    fn snapshot(&self) -> InterceptionSnapshot;
}

/// Construction and live policy replacement for an execution's egress owner.
///
/// Peer authority requires an unrestricted compiled policy; constructing it performs no I/O.
///
/// ```
/// use pvisor_core::network::{NetworkConfig, NetworkMode, NetworkPolicy};
/// use pvisor_overlaynet::api::{BandwidthRegistry, EgressControl, EgressRuntime};
///
/// let restricted = NetworkPolicy::compile(&NetworkConfig {
///     mode: NetworkMode::NoNetwork, ..Default::default()
/// })?;
/// assert!(EgressRuntime::for_peer_network(restricted, BandwidthRegistry::default()).is_err());
/// let public = NetworkPolicy::compile(&NetworkConfig::default())?;
/// let egress = EgressRuntime::for_peer_network(public, BandwidthRegistry::default())?;
/// let clone = egress.clone();
/// clone.replace_policy(NetworkPolicy::compile(&NetworkConfig {
///     mode: NetworkMode::NoNetwork, ..Default::default()
/// })?);
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// The replacement is shared by both handles; later peer admissions are rejected.
pub trait EgressControl: Sized {
    /// Construct peer-capable egress using the default Core controller. Reject scoped
    /// grants, denies and bandwidth budgets before publishing an owner. Peer membership
    /// authorizes supported packet traffic; custom controllers are not supported here.
    fn for_peer_network(
        policy: NetworkPolicy,
        bandwidth: BandwidthRegistry,
    ) -> anyhow::Result<Self>;
    /// Construct ordinary egress. The compiled policy is mandatory; the controller may
    /// only narrow it. Registry clones share identical aggregate bandwidth buckets with
    /// proxies. No connection or worker starts; this owner cannot authorize peer membership.
    fn with_bandwidth_registry(
        policy: NetworkPolicy,
        controller: Arc<dyn ControlController>,
        bandwidth: BandwidthRegistry,
    ) -> Self;
    /// Atomically replace the already-compiled policy shared by clones. In-flight
    /// authorizations may finish under the old policy; restrictive replacement closes
    /// the peer packet gate. A poisoned lock ignores replacement and remains fail-closed.
    fn replace_policy(&self, policy: NetworkPolicy);
}

/// Owned VM data-plane configuration and deterministic guest addressing.
///
/// Configuration owns attribution values and shared egress/metrics handles, without starting a worker.
///
/// ```
/// use std::sync::Arc;
/// use pvisor_core::{PolicyControlController, network::{NetworkConfig, NetworkPolicy}};
/// use pvisor_overlaynet::api::{
///     BandwidthRegistry, EgressContext, EgressControl, EgressRuntime,
///     GUEST_IPV4, InterceptionObservation, InterceptionSnapshot,
///     VmNetworkConfig, VmNetworkConfiguration,
/// };
///
/// let egress = EgressRuntime::with_bandwidth_registry(
///     NetworkPolicy::compile(&NetworkConfig::default())?,
///     Arc::new(PolicyControlController), BandwidthRegistry::default(),
/// );
/// let config = VmNetworkConfig::new(egress, EgressContext {
///     run_id: Some("run-1".into()), execution_id: Some("execution-1".into()),
/// });
/// assert_eq!(config.max_flows, 1024);
/// assert!(config.gateway.is_none() && config.peer_network.is_none());
/// assert_eq!(config.context.run_id.as_deref(), Some("run-1"));
/// assert_eq!(config.metrics.snapshot(), InterceptionSnapshot::default());
/// assert_eq!(VmNetworkConfig::guest_mac(GUEST_IPV4), [2, 80, 86, 0, 0, 2]);
/// # Ok::<(), anyhow::Error>(())
/// ```
pub trait VmNetworkConfiguration: Sized {
    /// Use 1024 flows, fresh metrics, no Gateway and no peer membership; no I/O.
    fn new(egress: EgressRuntime, context: EgressContext) -> Self;
    /// Deterministic locally administered MAC; the default address maps to 02:50:56:00:00:02.
    /// This is address derivation, not address allocation or membership admission.
    fn guest_mac(address: Ipv4Addr) -> [u8; 6];
}

/// Lifetime of a framed Ethernet/TCP/DNS worker; declarations are platform-uniform.
///
/// This in-memory transport needs no VM or listener. Invalid configuration is rejected before
/// its factory runs; explicit shutdown joins the worker and releases the adopted endpoint.
///
/// ```
/// use std::sync::Arc;
/// use pvisor_core::{PolicyControlController, network::{NetworkConfig, NetworkPolicy}};
/// use pvisor_overlaynet::api::{
///     BandwidthRegistry, EgressContext, EgressControl, EgressRuntime, InterceptionSnapshot,
///     VmNetwork, VmNetworkConfig, VmNetworkConfiguration, VmNetworkRuntime,
/// };
/// use tokio::io::AsyncReadExt;
///
/// let config = VmNetworkConfig::new(EgressRuntime::with_bandwidth_registry(
///     NetworkPolicy::compile(&NetworkConfig::default())?,
///     Arc::new(PolicyControlController), BandwidthRegistry::default(),
/// ), EgressContext::default());
/// let mut invalid = config.clone();
/// invalid.max_flows = 0;
/// let failure = VmNetwork::start_with_io::<tokio::io::DuplexStream, _>(
///     invalid, || panic!("invalid capacity must not adopt a transport"),
/// );
/// assert!(failure.err().unwrap().to_string().contains("max_flows"));
/// let (host, mut guest) = tokio::io::duplex(4096);
/// let network = VmNetwork::start_with_io(config, move || Ok(host))?;
/// assert_eq!(network.shutdown()?, InterceptionSnapshot::default());
/// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// runtime.block_on(async {
///     let eof = tokio::time::timeout(std::time::Duration::from_secs(3), guest.read(&mut [0; 1])).await??;
///     assert_eq!(eof, 0);
///     Ok::<(), anyhow::Error>(())
/// })?;
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// Shutdown consumes ownership, so the same worker cannot be shut down twice.
///
/// ```compile_fail,E0382
/// use pvisor_overlaynet::api::{VmNetwork, VmNetworkRuntime};
/// fn shutdown_twice(network: VmNetwork) {
///     let _ = network.shutdown();
///     let _ = network.shutdown();
/// }
/// ```
pub trait VmNetworkRuntime: Sized {
    /// Consume configuration and start a dedicated runtime thread. Reject zero flow
    /// capacity and invalid/conflicting peer membership before startup succeeds. The
    /// transport factory runs inside that runtime for Unix/Windows adoption; it must
    /// return promptly and transfers exclusive ownership of its endpoint to the worker.
    /// Frames are four-byte big-endian lengths followed by Ethernet bytes. Keep the
    /// other endpoint and host authority under caller ownership, outside guest shares.
    /// Startup errors release admitted resources and never select another transport.
    fn start_with_io<T, F>(config: VmNetworkConfig, make_stream: F) -> anyhow::Result<Self>
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        F: FnOnce() -> io::Result<T> + Send + 'static;
    /// Consume the owner, cancel and join its worker, then return an owned snapshot.
    /// Worker failures/panics return errors. Drop also cancels/joins but cannot report
    /// driver errors; explicit shutdown is required to observe completion failures.
    fn shutdown(self) -> anyhow::Result<InterceptionSnapshot>;
}

/// Optional lifecycle hook for authorized GET/CONNECT downstream delivery.
/// Implementations fence native pause/resume and tolerate cancellation during awaits.
///
/// A semaphore can model resume acknowledgements in an adapter test. Native adapters
/// must additionally perform the platform pause/resume and cleanup operations.
///
/// ```
/// use std::sync::Arc;
/// use async_trait::async_trait;
/// use futures_util::poll;
/// use pvisor_overlaynet::api::DownloadWait;
/// use tokio::sync::Semaphore;
///
/// struct AcknowledgedWait(Arc<Semaphore>);
/// #[async_trait]
/// impl DownloadWait for AcknowledgedWait {
///     async fn waiting(&mut self) -> anyhow::Result<()> { Ok(()) }
///     async fn ready(&mut self) -> anyhow::Result<()> {
///         self.0.acquire().await?.forget();
///         Ok(())
///     }
///     fn cancel(&mut self) { self.0.close(); }
/// }
/// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// runtime.block_on(async {
///     let acknowledgements = Arc::new(Semaphore::new(0));
///     let mut wait = AcknowledgedWait(acknowledgements.clone());
///     wait.waiting().await?;
///     let mut ready = Box::pin(wait.ready());
///     assert!(poll!(ready.as_mut()).is_pending());
///     acknowledgements.add_permits(1);
///     ready.await?;
///     wait.cancel();
///     assert!(acknowledgements.is_closed());
///     Ok::<(), anyhow::Error>(())
/// })?;
/// # Ok::<(), anyhow::Error>(())
/// ```
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait DownloadWait: Send {
    /// Mark upstream waiting without waiting for a future guest pause. Silence is not idle proof.
    async fn waiting(&mut self) -> anyhow::Result<()>;
    /// Resume/acknowledge native cleanup before headers, bytes, EOF or errors are delivered.
    async fn ready(&mut self) -> anyhow::Result<()>;
    /// Nonblocking cleanup on stream/handler drop, including partially awaited calls.
    fn cancel(&mut self);
}

/// Borrowed proxy observation; valid only during the synchronous callback.
/// Events report admission and transport facts, not completed application effects.
pub enum ProxyEvent<'a> {
    /// Destination admission was denied before dispatch.
    Denied {
        /// Rejected logical host.
        host: &'a str,
        /// Core policy or controller denial.
        reason: &'a DenyReason,
    },
    /// An admitted request is about to enter the selected path.
    Dispatched {
        /// Request borrowed before ownership transfers to forwarding or the sink.
        request: &'a Request,
        /// Selected path: connect, forward or sink.
        target: &'a str,
    },
    /// Proxy response headers are available; the body may still fail.
    Response {
        /// Logical proxy target.
        target: &'a str,
        /// Response status before body delivery.
        status: StatusCode,
    },
    /// Transport or dispatch failed without an implicit retry.
    Failed {
        /// Logical proxy target.
        target: &'a str,
        /// Borrowed diagnostic message.
        error: &'a str,
    },
}

/// Application sink bound to one proxy owner. No method has a default implementation.
/// Decline absolute-URI requests to retain transparent forwarding; relative requests
/// reach handle. Sinks explicitly choose download hooks and diagnostic callbacks.
#[async_trait]
pub trait OverlaySink: Clone + Send + Sync + 'static {
    /// Cloneable request-local data; must not grant authority from correlation fields.
    type RequestContext: Clone + Send + Sync + 'static;
    /// Snapshot policy/attribution before admission. Errors yield a proxy error without dispatch.
    fn request_context(
        &self,
        request: &Request,
    ) -> anyhow::Result<OverlayRequestContext<Self::RequestContext>>;
    /// Consume an admitted request, retaining its body ownership through response handling.
    /// Errors produce a proxy error. Callers must not bypass prior policy with new destinations.
    #[allow(clippy::double_must_use)]
    async fn handle(
        &self,
        request: Request,
        peer: SocketAddr,
        context: &OverlayRequestContext<Self::RequestContext>,
    ) -> anyhow::Result<Response>;
    /// Return an optional native lifecycle hook after destination authorization.
    /// None retains ordinary forwarding; CONNECT remains opaque bytes, not model capture.
    fn download_wait(
        &self,
        context: &OverlayRequestContext<Self::RequestContext>,
    ) -> anyhow::Result<Option<Box<dyn DownloadWait>>>;
    /// Whether to consume an absolute-URI request. False retains transparent forwarding.
    fn accepts(&self, request: &Request) -> bool;
    /// Observe a synchronous proxy decision or outcome. Events cannot authorize
    /// access or retry uncertain effects; response status does not prove body delivery.
    fn observe(&self, context: &OverlayRequestContext<Self::RequestContext>, event: ProxyEvent<'_>);
}

/// Proxy construction and ownership transfer to an Axum router.
///
/// A local sink can handle a relative request without a listener or upstream connection.
/// Absolute proxy requests still require destination authorization before sink selection.
///
/// ```
/// use std::{net::SocketAddr, sync::{Arc, atomic::{AtomicUsize, Ordering}}};
/// use async_trait::async_trait;
/// use axum::{body::Body, extract::{ConnectInfo, Request}, response::Response};
/// use http_body_util::BodyExt;
/// use pvisor_core::{PolicyControlController, network::{NetworkConfig, NetworkMode, NetworkPolicy, ResolvedAddressPolicy}};
/// use pvisor_overlaynet::api::{
///     BandwidthRegistry, DownloadWait, InterceptionMetrics, InterceptionObservation,
///     OverlayProxy, OverlayRequestContext, OverlayServerState, OverlaySink, ProxyEvent,
/// };
/// use tower::ServiceExt;
///
/// #[derive(Clone)]
/// struct HealthSink(NetworkPolicy);
/// #[async_trait]
/// impl OverlaySink for HealthSink {
///     type RequestContext = ();
///     fn request_context(&self, _: &Request) -> anyhow::Result<OverlayRequestContext<()>> {
///         Ok(OverlayRequestContext {
///             policy: self.0.clone(), run_id: None, execution_id: None,
///             session_id: "health".into(), sink: (),
///         })
///     }
///     fn accepts(&self, _: &Request) -> bool { false }
///     async fn handle(&self, _: Request, _: SocketAddr, context: &OverlayRequestContext<()>) -> anyhow::Result<Response> {
///         assert_eq!(context.session_id, "health");
///         Ok(Response::new(Body::from("healthy")))
///     }
///     fn download_wait(&self, _: &OverlayRequestContext<()>) -> anyhow::Result<Option<Box<dyn DownloadWait>>> { Ok(None) }
///     fn observe(&self, _: &OverlayRequestContext<()>, _: ProxyEvent<'_>) {}
/// }
/// let policy = NetworkPolicy::compile(&NetworkConfig { mode: NetworkMode::NoNetwork, ..Default::default() })?;
/// let active = Arc::new(AtomicUsize::new(0));
/// let metrics = InterceptionMetrics::default();
/// let before = metrics.snapshot();
/// let router = OverlayServerState::new(Arc::new(PolicyControlController), HealthSink(policy), active.clone())
///     .with_interception_metrics(metrics.clone())
///     .with_bandwidth_registry(BandwidthRegistry::default())
///     .with_resolved_address_policy(ResolvedAddressPolicy::Strict)
///     .into_router();
/// let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// runtime.block_on(async {
///     let mut request = Request::builder().uri("/health").body(Body::empty())?;
///     request.extensions_mut().insert(ConnectInfo("127.0.0.1:40000".parse::<SocketAddr>()?));
///     let response = router.oneshot(request).await?;
///     assert_eq!(active.load(Ordering::Relaxed), 1);
///     assert_eq!(response.into_body().collect().await?.to_bytes(), "healthy");
///     assert_eq!(active.load(Ordering::Relaxed), 0);
///     Ok::<(), anyhow::Error>(())
/// })?;
/// assert_eq!(before.requests_seen, 0);
/// assert_eq!(metrics.clone().snapshot().requests_seen, 1);
/// assert_eq!(metrics.snapshot().sink_requests, 1);
/// # Ok::<(), anyhow::Error>(())
/// ```
pub trait OverlayProxy<S: OverlaySink>: Sized {
    /// Bind controller, sink and shared active-request counter. Creates 128 admission
    /// slots and fresh accounting/bandwidth owners. No listener starts. The default
    /// resolved-address policy supports host connector aliases; VM callers must select VmEgress.
    fn new(
        control_controller: Arc<dyn ControlController>,
        sink: S,
        active_requests: Arc<AtomicUsize>,
    ) -> Self;
    /// Consume this state and bind address constraints without changing logical grants.
    fn with_resolved_address_policy(self, policy: ResolvedAddressPolicy) -> Self;
    /// Consume this state and share the supplied metrics with its clones and router.
    fn with_interception_metrics(self, metrics: InterceptionMetrics) -> Self;
    /// Consume this state and share aggregate bandwidth scheduling with other owners.
    fn with_bandwidth_registry(self, registry: BandwidthRegistry) -> Self;
    /// Consume this state into a router; caller supplies a listener and SocketAddr
    /// ConnectInfo. Excess requests return 429; response/tunnel ownership retains permits
    /// until completion/drop, and failures release their admission ownership.
    fn into_router(self) -> Router;
}

/// Stateless HTTP header safety on the existing HeaderMap; no mutation or I/O.
///
/// ```
/// use axum::http::HeaderMap;
/// use pvisor_overlaynet::api::ForwardingHeaders;
///
/// let mut headers = HeaderMap::new();
/// headers.insert("connection", "keep-alive, X-Private-Hop".parse()?);
/// headers.insert("upgrade", "h2c, WebSocket".parse()?);
/// assert!(headers.skip_upstream_forward_header("AUTHORIZATION"));
/// assert!(headers.skip_upstream_forward_header("x-private-hop"));
/// assert!(!headers.skip_upstream_forward_header("x-request-id"));
/// assert!(headers.skip_response_header_after_reframing("content-length", false));
/// assert!(!headers.skip_response_header_after_reframing("content-type", false));
/// assert!(headers.skip_response_header_after_reframing("content-type", true));
/// assert!(headers.is_websocket_upgrade());
/// # Ok::<(), axum::http::header::InvalidHeaderValue>(())
/// ```
pub trait ForwardingHeaders {
    /// Filter hop-by-hop/Connection-nominated headers, Host, length, credentials and Expect
    /// before upstream model forwarding. Names and tokens are matched case-insensitively.
    fn skip_upstream_forward_header(&self, name: &str) -> bool;
    /// Remove hop-by-hop/Connection-nominated framing and Content-Length after rebuilding
    /// a body; additionally remove Content-Type/Encoding when its bytes changed.
    fn skip_response_header_after_reframing(&self, name: &str, body_changed: bool) -> bool;
    /// Whether an Upgrade field contains the websocket protocol token, case-insensitively.
    fn is_websocket_upgrade(&self) -> bool;
}
