//! Transparent IPv4/TCP data plane for libkrun virtio-net.
//!
//! libkrun's unix-stream backend carries one Ethernet frame at a time, prefixed
//! by a four-byte big-endian length. This module terminates that link in
//! smoltcp, serves DHCP and synthetic DNS locally, and opens policy-authorized
//! host TCP streams only after the guest SYN has been observed.

use crate::api::{GUEST_IPV4, ROUTER_IPV4, VmNetworkConfig};
use crate::api::{InterceptionObservation, VmNetworkConfiguration, VmNetworkRuntime};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::thread;
use std::time::Duration as StdDuration;

use anyhow::Context as _;
use pvisor_core::NetworkTransport;
use smoltcp::iface::{Config as InterfaceConfig, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{ChecksumCapabilities, Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::udp::UdpMetadata;
use smoltcp::socket::{tcp, udp};
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::{
    DhcpMessageType, DhcpOption, DhcpPacket, DhcpRepr, EthernetAddress, EthernetFrame,
    EthernetProtocol, HardwareAddress, IpAddress, IpCidr, IpEndpoint, IpProtocol, Ipv4Address,
    Ipv4Packet, Ipv4Repr, TcpControl, TcpPacket, TcpRepr,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::api::PeerNetworkConfig;
use crate::api::{EgressContext, EgressRuntime};
use crate::api::{InterceptionMetrics, InterceptionSnapshot};
use crate::egress::{
    CONNECT_TIMEOUT, EgressError, connect_tcp_addresses, connect_via_ambient_http_proxy,
};

use crate::peer::{BoundPeer, PeerNetwork};

const ROUTER_MAC: EthernetAddress = EthernetAddress([0x02, 0x50, 0x56, 0x00, 0x00, 0x01]);
const FRAME_MTU: usize = 1514;
const MAX_FRAME_LEN: usize = 65_535;
const TCP_BUFFER_BYTES: usize = 64 * 1024;
/// Concurrent TCP flows the backend tracks before refusing new SYNs. Each
/// flow owns two `TCP_BUFFER_BYTES` socket buffers, so the cap is also the
/// worst-case memory bound (1024 flows ≈ 128 MiB per VM, reached only when
/// that many connections are simultaneously tracked).
const DEFAULT_MAX_FLOWS: usize = 1024;
const TCP_CHANNEL_DEPTH: usize = 256;
const FLOW_BUFFER_CHUNKS: usize = TCP_BUFFER_BYTES / (16 * 1024);
const TCP_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Wall-clock budget for one flow to finish its handshake (SYN received,
/// upstream connected, guest ACK). Flows still waiting in `WaitingForSyn`
/// or `Connecting`, or still awaiting the guest ACK, are aborted and their table slot
/// reclaimed, so a burst of short-lived connections cannot pin the table
/// until the idle timeout. This mirrors SYN-proxy handshake deadlines in
/// Envoy/nginx rather than TCP's multi-minute SYN retry ladder.
const TCP_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_POLL_DELAY: StdDuration = StdDuration::from_millis(100);
const SYNTHETIC_DNS_CAPACITY: usize = 4096;
const DNS_MAX_MESSAGE: usize = 4096;
const FRAME_BATCH: usize = 32;

fn router_address(config: &VmNetworkConfig) -> Ipv4Addr {
    config
        .peer_network
        .as_ref()
        .map_or(ROUTER_IPV4, |peer| peer.gateway)
}

impl std::fmt::Debug for VmNetworkConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VmNetworkConfig")
            .field("context", &self.context)
            .field("gateway", &self.gateway)
            .field("max_flows", &self.max_flows)
            .finish_non_exhaustive()
    }
}

/// Execution-scoped smoltcp backend. The returned peer stream is passed to
/// `pvisor_vm::api::VmConfiguration::network`; this side owns the policy and host sockets.
pub struct VmNetwork {
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<anyhow::Result<()>>>,
    metrics: InterceptionMetrics,
}

impl Drop for VmNetwork {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take()
            && thread.thread().id() != thread::current().id()
            && let Err(error) = thread.join()
        {
            tracing::warn!(?error, "VM network backend panicked during drop");
        }
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct FlowKey {
    guest_port: u16,
    destination_addr: Ipv4Addr,
    destination_port: u16,
}

#[derive(Debug, Clone)]
enum FlowDestination {
    Egress { host: String, port: u16 },
    Gateway(SocketAddr),
    Dns,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlowPhase {
    WaitingForSyn,
    Connecting,
    Connected,
    LocalDns,
}

struct Flow {
    generation: u64,
    handle: SocketHandle,
    destination: FlowDestination,
    phase: FlowPhase,
    /// Stack-clock creation time; bounds how long a never-established flow
    /// may hold a table slot (see `TCP_HANDSHAKE_TIMEOUT`).
    created_at: Instant,
    upstream: Option<mpsc::Sender<Vec<u8>>>,
    upstream_task: Option<JoinHandle<()>>,
    inbound: VecDeque<(Vec<u8>, Option<OwnedSemaphorePermit>)>,
    inbound_offset: usize,
    dns_input: Vec<u8>,
    remote_eof: bool,
}

enum FlowEvent {
    Connected {
        key: FlowKey,
        generation: u64,
        sender: mpsc::Sender<Vec<u8>>,
        policy_authorized: bool,
    },
    Data {
        key: FlowKey,
        generation: u64,
        bytes: Vec<u8>,
        permit: OwnedSemaphorePermit,
    },
    Uploaded {
        key: FlowKey,
        generation: u64,
        bytes: usize,
    },
    RemoteEof(FlowKey, u64),
    ConnectFailed {
        key: FlowKey,
        generation: u64,
        denied: bool,
        error: String,
    },
    BridgeFailed {
        key: FlowKey,
        generation: u64,
        error: String,
    },
}

async fn run_backend<T: AsyncRead + AsyncWrite + Unpin>(
    stream: T,
    config: VmNetworkConfig,
    metrics: InterceptionMetrics,
    mut shutdown: oneshot::Receiver<()>,
    mut peer: Option<PeerNetwork>,
    started: &std::sync::mpsc::SyncSender<Result<(), String>>,
) -> anyhow::Result<()> {
    let guest_address = config
        .peer_network
        .as_ref()
        .map_or(GUEST_IPV4, |peer| peer.address);
    let router = router_address(&config);
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut frame_reader = FrameReader::default();
    let mut frame_device = FrameDevice::default();
    let clock = std::time::Instant::now();

    let mut interface_config = InterfaceConfig::new(HardwareAddress::Ethernet(ROUTER_MAC));
    interface_config.random_seed = random_seed();
    let mut interface = Interface::new(interface_config, &mut frame_device, Instant::ZERO);
    let mut address_result = Ok(());
    interface.update_ip_addrs(|addresses| {
        address_result = addresses.push(IpCidr::new(IpAddress::Ipv4(router), 24));
    });
    address_result.map_err(|_| anyhow::anyhow!("VM interface address capacity exhausted"))?;
    interface.set_any_ip(true);
    interface
        .routes_mut()
        .add_default_ipv4_route(router)
        .map_err(|_| anyhow::anyhow!("VM default route capacity exhausted"))?;

    let mut sockets = SocketSet::new(Vec::new());
    let dhcp_handle = sockets.add(udp_socket(4, 8192, 4, 8192, 67)?);
    let dns_handle = sockets.add(udp_socket(16, 65_535, 16, 65_535, 53)?);
    let mut flows = HashMap::<FlowKey, Flow>::new();
    let mut dns = SyntheticDns {
        peers: config.peer_network.clone(),
        ..Default::default()
    };
    let (event_tx, mut event_rx) = mpsc::channel(TCP_CHANNEL_DEPTH);
    let mut poll_delay = Box::pin(tokio::time::sleep(StdDuration::ZERO));
    started
        .send(Ok(()))
        .context("VM network startup receiver disconnected")?;

    'backend: loop {
        // Readiness-based peer I/O does not consume Tokio's cooperative budget.
        tokio::task::consume_budget().await;
        let mut service_stack = false;
        tokio::select! {
            _ = &mut shutdown => break,
            event = event_rx.recv() => {
                if let Some(event) = event {
                    apply_flow_event(event, &mut flows, &mut sockets, &metrics);
                    service_stack = true;
                }
            }
            received = receive_peer(&mut peer), if peer.is_some() => {
                if let Some(frame) = received.context("receive peer packet")? {
                    if config.egress.permits_peer_network() {
                        frame_device.transmit_queue.push_back(frame);
                    } else {
                        metrics.peer_dropped();
                    }
                }
                if let Some(peer) = peer.as_mut() {
                    for _ in 1..FRAME_BATCH {
                        match peer.try_receive() {
                            Ok(Some(frame)) if config.egress.permits_peer_network() => {
                                frame_device.transmit_queue.push_back(frame);
                            }
                            Ok(Some(_)) => metrics.peer_dropped(),
                            Ok(None) => {}
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                            Err(error) => return Err(error).context("receive peer packet batch"),
                        }
                    }
                }
            }
            read = frame_reader.read_more(&mut reader) => {
                match read {
                    Ok(0) => break,
                    Ok(_) => {
                        while let Some(frame) = frame_reader.next_frame()? {
                            if let Some(peer) = peer.as_mut()
                                && crate::peer::reserved_frame(frame, router) {
                                if config.egress.permits_peer_network() {
                                    peer.route(frame);
                                } else {
                                    metrics.peer_dropped();
                                }
                                continue;
                            }
                            if let Some(key) = initial_tcp_syn(frame, guest_address) {
                                ensure_listener(
                                    key,
                                    &mut flows,
                                    &mut sockets,
                                    &dns,
                                    &config,
                                    &metrics,
                                    smol_now(clock),
                                );
                            }
                            if !supported_guest_frame(frame, guest_address) {
                                metrics.unsupported_packet();
                                continue;
                            }
                            service_stack = true;
                            frame_device.receive_queue.push_back(frame.to_vec());
                        }
                        if let Some(peer) = peer.as_mut() { peer.flush(); }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error).context("read VM Ethernet frame"),
                }
            }
            _ = &mut poll_delay => { service_stack = true; }
        }

        if service_stack {
            let now = smol_now(clock);
            let _ = interface.poll(now, &mut frame_device, &mut sockets);
            service_dhcp(&mut sockets, dhcp_handle, guest_address, router);
            service_dns_udp(&mut sockets, dns_handle, &mut dns, &metrics);
            drive_flows(
                &mut sockets,
                &mut flows,
                &mut dns,
                &config,
                &event_tx,
                &metrics,
            );
            let _ = interface.poll(now, &mut frame_device, &mut sockets);
            reap_closed_flows(&mut sockets, &mut flows, &metrics, now);
            let delay = interface
                .poll_delay(smol_now(clock), &sockets)
                .map(|delay| StdDuration::from_micros(delay.total_micros()))
                .unwrap_or(MAX_POLL_DELAY)
                .min(MAX_POLL_DELAY);
            poll_delay
                .as_mut()
                .reset(tokio::time::Instant::now() + delay);
        }

        while !frame_device.transmit_queue.is_empty() {
            let result = tokio::select! {
                biased;
                _ = &mut shutdown => break 'backend,
                result = write_frames(&mut writer, &frame_device.transmit_queue) => result,
            };
            let count = result.context("write VM Ethernet frames")?;
            frame_device.transmit_queue.drain(..count);
        }
    }

    for (_, mut flow) in flows.drain() {
        if let Some(task) = flow.upstream_task.take() {
            task.abort();
        }
        metrics.tcp_flow_closed();
    }
    Ok(())
}

async fn receive_peer(peer: &mut Option<PeerNetwork>) -> io::Result<Option<Vec<u8>>> {
    match peer {
        Some(peer) => peer.receive().await,
        None => std::future::pending().await,
    }
}

fn udp_socket(
    rx_packets: usize,
    rx_bytes: usize,
    tx_packets: usize,
    tx_bytes: usize,
    port: u16,
) -> anyhow::Result<udp::Socket<'static>> {
    let rx = udp::PacketBuffer::new(
        vec![udp::PacketMetadata::EMPTY; rx_packets],
        vec![0; rx_bytes],
    );
    let tx = udp::PacketBuffer::new(
        vec![udp::PacketMetadata::EMPTY; tx_packets],
        vec![0; tx_bytes],
    );
    let mut socket = udp::Socket::new(rx, tx);
    socket
        .bind(port)
        .with_context(|| format!("bind VM UDP service port {port}"))?;
    Ok(socket)
}

fn random_seed() -> u64 {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (timestamp.as_nanos() as u64) ^ (std::process::id() as u64).rotate_left(17)
}

fn smol_now(clock: std::time::Instant) -> Instant {
    Instant::from_micros(clock.elapsed().as_micros().min(i64::MAX as u128) as i64)
}

fn next_flow_generation() -> Option<u64> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    allocate_flow_generation(&NEXT)
}

fn allocate_flow_generation(next: &std::sync::atomic::AtomicU64) -> Option<u64> {
    next.try_update(
        std::sync::atomic::Ordering::Relaxed,
        std::sync::atomic::Ordering::Relaxed,
        |value| value.checked_add(1),
    )
    .ok()
}

fn ensure_listener(
    key: FlowKey,
    flows: &mut HashMap<FlowKey, Flow>,
    sockets: &mut SocketSet<'static>,
    dns: &SyntheticDns,
    config: &VmNetworkConfig,
    metrics: &InterceptionMetrics,
    now: Instant,
) {
    if flows.contains_key(&key) {
        return;
    }
    if flows.len() >= config.max_flows {
        metrics.tcp_flow_denied();
        metrics.target_denied(
            &key.destination_addr.to_string(),
            key.destination_port,
            NetworkTransport::TcpTunnel,
        );
        warn_flow_table_full(flows.len(), config.max_flows);
        return;
    }
    let destination = if key.destination_port == 53 {
        FlowDestination::Dns
    } else if key.destination_addr == router_address(config) {
        match config
            .gateway
            .filter(|gateway| gateway.guest_port == key.destination_port)
        {
            Some(gateway) => FlowDestination::Gateway(gateway.host),
            None => {
                metrics.tcp_flow_denied();
                return;
            }
        }
    } else if synthetic_address(key.destination_addr) {
        match dns.hostname(key.destination_addr) {
            Some(host) => FlowDestination::Egress {
                host: host.to_owned(),
                port: key.destination_port,
            },
            None => {
                metrics.tcp_flow_denied();
                metrics.target_denied(
                    &key.destination_addr.to_string(),
                    key.destination_port,
                    NetworkTransport::TcpTunnel,
                );
                return;
            }
        }
    } else if blocked_literal_destination(key.destination_addr) {
        metrics.tcp_flow_denied();
        metrics.target_denied(
            &key.destination_addr.to_string(),
            key.destination_port,
            NetworkTransport::TcpTunnel,
        );
        return;
    } else {
        FlowDestination::Egress {
            host: key.destination_addr.to_string(),
            port: key.destination_port,
        }
    };
    let Some(generation) = next_flow_generation() else {
        metrics.tcp_flow_denied();
        return;
    };

    let mut socket = tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0; TCP_BUFFER_BYTES]),
        tcp::SocketBuffer::new(vec![0; TCP_BUFFER_BYTES]),
    );
    socket.set_timeout(Some(TCP_IDLE_TIMEOUT));
    let local = IpEndpoint::new(IpAddress::Ipv4(key.destination_addr), key.destination_port);
    if socket.listen(local).is_err() {
        metrics.tcp_flow_denied();
        return;
    }
    let local_dns = matches!(destination, FlowDestination::Dns);
    socket.pause_synack(!local_dns);
    let handle = sockets.add(socket);
    flows.insert(
        key,
        Flow {
            generation,
            handle,
            destination,
            phase: if local_dns {
                FlowPhase::LocalDns
            } else {
                FlowPhase::WaitingForSyn
            },
            created_at: now,
            upstream: None,
            upstream_task: None,
            inbound: VecDeque::new(),
            inbound_offset: 0,
            dns_input: Vec::new(),
            remote_eof: false,
        },
    );
    metrics.tcp_flow_opened();
}

fn drive_flows(
    sockets: &mut SocketSet<'static>,
    flows: &mut HashMap<FlowKey, Flow>,
    dns: &mut SyntheticDns,
    config: &VmNetworkConfig,
    event_tx: &mpsc::Sender<FlowEvent>,
    metrics: &InterceptionMetrics,
) {
    let keys = flows.keys().copied().collect::<Vec<_>>();
    for key in keys {
        let Some(flow) = flows.get_mut(&key) else {
            continue;
        };
        let socket = sockets.get_mut::<tcp::Socket>(flow.handle);

        if flow.phase == FlowPhase::WaitingForSyn && socket.state() == tcp::State::SynReceived {
            flow.phase = FlowPhase::Connecting;
            let generation = flow.generation;
            let destination = flow.destination.clone();
            let egress = config.egress.clone();
            let context = config.context.clone();
            let events = event_tx.clone();
            flow.upstream_task = Some(tokio::spawn(async move {
                connect_and_bridge(key, generation, destination, egress, context, events).await;
            }));
        }

        if flow.phase == FlowPhase::LocalDns {
            drive_dns_tcp(socket, flow, dns, metrics);
            continue;
        }

        if flow.phase == FlowPhase::Connected {
            // `may_recv()` is false until the guest ACK; SynReceived is not guest EOF.
            if guest_handshake_pending(socket.state()) {
                continue;
            }
            if let Some(sender) = flow.upstream.as_ref()
                && sender.capacity() > 0
                && socket.can_recv()
            {
                let mut bytes = vec![0; 16 * 1024];
                if let Ok(length) = socket.recv_slice(&mut bytes) {
                    bytes.truncate(length);
                    if length > 0 && sender.try_send(bytes).is_err() {
                        socket.abort();
                    }
                }
            }
            if !socket.may_recv() {
                flow.upstream.take();
            }
            flush_inbound(socket, flow);
            if flow.remote_eof && flow.inbound.is_empty() && socket.may_send() {
                socket.close();
            }
        }
    }
}

fn guest_handshake_pending(state: tcp::State) -> bool {
    state == tcp::State::SynReceived
}

fn drive_dns_tcp(
    socket: &mut tcp::Socket<'_>,
    flow: &mut Flow,
    dns: &mut SyntheticDns,
    metrics: &InterceptionMetrics,
) {
    flush_inbound(socket, flow);
    if flow.inbound.len() < FLOW_BUFFER_CHUNKS
        && flow.dns_input.len() < DNS_MAX_MESSAGE + 2
        && socket.can_recv()
    {
        let mut bytes = [0; DNS_MAX_MESSAGE + 2];
        let available = DNS_MAX_MESSAGE + 2 - flow.dns_input.len();
        if let Ok(length) = socket.recv_slice(&mut bytes[..available]) {
            flow.dns_input.extend_from_slice(&bytes[..length]);
        }
    }
    while flow.inbound.len() < FLOW_BUFFER_CHUNKS {
        if flow.dns_input.len() < 2 {
            break;
        }
        let message_len = u16::from_be_bytes([flow.dns_input[0], flow.dns_input[1]]) as usize;
        if message_len > DNS_MAX_MESSAGE {
            socket.abort();
            return;
        }
        if flow.dns_input.len() < message_len + 2 {
            break;
        }
        let query = flow.dns_input[2..message_len + 2].to_vec();
        flow.dns_input.drain(..message_len + 2);
        metrics.dns_query();
        let response = dns.answer(&query);
        metrics.dns_answer();
        let mut framed = Vec::with_capacity(response.len() + 2);
        framed.extend_from_slice(&(response.len() as u16).to_be_bytes());
        framed.extend_from_slice(&response);
        flow.inbound.push_back((framed, None));
    }
    flush_inbound(socket, flow);
    if !socket.may_recv() && flow.inbound.is_empty() && socket.may_send() {
        socket.close();
    }
}

fn flush_inbound(socket: &mut tcp::Socket<'_>, flow: &mut Flow) {
    while socket.can_send() {
        let Some((front, _permit)) = flow.inbound.front() else {
            break;
        };
        match socket.send_slice(&front[flow.inbound_offset..]) {
            Ok(0) | Err(_) => break,
            Ok(written) => {
                flow.inbound_offset += written;
                if flow.inbound_offset == front.len() {
                    flow.inbound.pop_front();
                    flow.inbound_offset = 0;
                }
            }
        }
    }
}

async fn connect_and_bridge(
    key: FlowKey,
    generation: u64,
    destination: FlowDestination,
    egress: EgressRuntime,
    context: EgressContext,
    events: mpsc::Sender<FlowEvent>,
) {
    let result = match destination {
        FlowDestination::Egress { host, port } => connect_vm_egress(&egress, &context, &host, port)
            .await
            .map(|(stream, bandwidth)| (stream, bandwidth, true)),
        FlowDestination::Gateway(address) => {
            match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
                Ok(Ok(stream)) => Ok((stream, Default::default(), false)),
                Ok(Err(error)) => Err(EgressError::Connect {
                    host: address.ip().to_string(),
                    port: address.port(),
                    source: error,
                }),
                Err(_) => Err(EgressError::ConnectTimeout {
                    host: address.ip().to_string(),
                    port: address.port(),
                }),
            }
        }
        FlowDestination::Dns => return,
    };
    let (stream, bandwidth, policy_authorized) = match result {
        Ok(connection) => connection,
        Err(error) => {
            let denied = matches!(error, EgressError::Denied(_));
            let _ = events
                .send(FlowEvent::ConnectFailed {
                    key,
                    generation,
                    denied,
                    error: error.to_string(),
                })
                .await;
            return;
        }
    };
    let (sender, receiver) = mpsc::channel(FLOW_BUFFER_CHUNKS);
    if events
        .send(FlowEvent::Connected {
            key,
            generation,
            sender,
            policy_authorized,
        })
        .await
        .is_err()
    {
        return;
    }
    bridge_upstream(key, generation, stream, receiver, bandwidth, events).await;
}

async fn connect_vm_egress(
    egress: &EgressRuntime,
    context: &EgressContext,
    host: &str,
    port: u16,
) -> Result<(TcpStream, crate::bandwidth::BandwidthSession), EgressError> {
    let (addresses, bandwidth) = egress.authorize_tcp(context, host, port).await?;
    let stream = match connect_via_ambient_http_proxy(host, &addresses).await {
        Some(Ok(stream)) => stream,
        Some(Err(source)) => {
            return Err(EgressError::Connect {
                host: host.to_owned(),
                port,
                source,
            });
        }
        None => connect_tcp_addresses(&addresses, host, port).await?,
    };
    Ok((stream, bandwidth))
}

async fn bridge_upstream(
    key: FlowKey,
    generation: u64,
    stream: TcpStream,
    mut outbound: mpsc::Receiver<Vec<u8>>,
    bandwidth: crate::bandwidth::BandwidthSession,
    events: mpsc::Sender<FlowEvent>,
) {
    let (mut read, mut write) = stream.into_split();
    let upload_bandwidth = bandwidth.clone();
    let upload_events = events.clone();
    let upload = async move {
        while let Some(bytes) = outbound.recv().await {
            upload_bandwidth.throttle(bytes.len()).await;
            write.write_all(&bytes).await?;
            if upload_events
                .send(FlowEvent::Uploaded {
                    key,
                    generation,
                    bytes: bytes.len(),
                })
                .await
                .is_err()
            {
                return Ok(());
            }
        }
        write.shutdown().await
    };
    let download = async {
        let mut buffer = vec![0; 16 * 1024];
        let credits = Arc::new(Semaphore::new(FLOW_BUFFER_CHUNKS));
        loop {
            // Bound each flow across both the shared event channel and inbound
            // queue. Reading resumes only after the guest socket accepts data.
            let permit = credits
                .clone()
                .acquire_owned()
                .await
                .map_err(io::Error::other)?;
            let length = read.read(&mut buffer).await?;
            if length == 0 {
                let _ = events.send(FlowEvent::RemoteEof(key, generation)).await;
                return Ok::<(), io::Error>(());
            }
            bandwidth.throttle(length).await;
            if events
                .send(FlowEvent::Data {
                    key,
                    generation,
                    bytes: buffer[..length].to_vec(),
                    permit,
                })
                .await
                .is_err()
            {
                return Ok(());
            }
        }
    };
    if let Err(error) = tokio::try_join!(upload, download) {
        let _ = events
            .send(FlowEvent::BridgeFailed {
                key,
                generation,
                error: error.to_string(),
            })
            .await;
    }
}

fn apply_flow_event(
    event: FlowEvent,
    flows: &mut HashMap<FlowKey, Flow>,
    sockets: &mut SocketSet<'static>,
    metrics: &InterceptionMetrics,
) {
    let (key, generation) = match &event {
        FlowEvent::Connected {
            key, generation, ..
        }
        | FlowEvent::Data {
            key, generation, ..
        }
        | FlowEvent::Uploaded {
            key, generation, ..
        }
        | FlowEvent::ConnectFailed {
            key, generation, ..
        }
        | FlowEvent::BridgeFailed {
            key, generation, ..
        }
        | FlowEvent::RemoteEof(key, generation) => (key, generation),
    };
    // Aborting the old task cannot retract events already queued for its tuple.
    if flows
        .get(key)
        .is_none_or(|flow| flow.generation != *generation)
    {
        return;
    }
    match event {
        FlowEvent::Connected {
            key,
            sender,
            policy_authorized,
            ..
        } => {
            if let Some(flow) = flows.get_mut(&key) {
                flow.upstream = Some(sender);
                flow.phase = FlowPhase::Connected;
                sockets
                    .get_mut::<tcp::Socket>(flow.handle)
                    .pause_synack(false);
                if policy_authorized {
                    metrics.policy_allowed();
                    if let FlowDestination::Egress { host, port } = &flow.destination {
                        metrics.target_allowed(host, *port, NetworkTransport::TcpTunnel);
                    }
                }
            }
        }
        FlowEvent::Data {
            key, bytes, permit, ..
        } => {
            if let Some(flow) = flows.get_mut(&key) {
                metrics.host_to_guest(bytes.len());
                flow.inbound.push_back((bytes, Some(permit)));
            }
        }
        FlowEvent::Uploaded { key, bytes, .. } => {
            if flows.contains_key(&key) {
                metrics.guest_to_host(bytes);
            }
        }
        FlowEvent::RemoteEof(key, _) => {
            if let Some(flow) = flows.get_mut(&key) {
                flow.remote_eof = true;
            }
        }
        FlowEvent::ConnectFailed {
            key, denied, error, ..
        } => {
            if let Some(flow) = flows.get_mut(&key) {
                tracing::debug!(?key, %error, "VM TCP flow failed closed");
                sockets.get_mut::<tcp::Socket>(flow.handle).abort();
                if denied {
                    metrics.tcp_flow_denied();
                    if let FlowDestination::Egress { host, port } = &flow.destination {
                        metrics.target_denied(host, *port, NetworkTransport::TcpTunnel);
                    }
                } else {
                    metrics.tcp_connect_failure();
                    if let FlowDestination::Egress { host, port } = &flow.destination {
                        metrics.target_failed(host, *port, NetworkTransport::TcpTunnel);
                    }
                }
            }
        }
        FlowEvent::BridgeFailed { key, error, .. } => {
            if let Some(flow) = flows.get_mut(&key) {
                tracing::debug!(?key, %error, "VM TCP bridge failed closed");
                sockets.get_mut::<tcp::Socket>(flow.handle).abort();
            }
        }
    }
}

/// One rate-limited line per five seconds while the table is full: every
/// refused SYN is guest-visible as `Connection refused`, and an unthrottled
/// warn per packet would flood the runner diagnostics during bursts.
fn warn_flow_table_full(len: usize, capacity: usize) {
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let now = std::time::Instant::now();
    let due = last.is_none_or(|then| now.duration_since(then) >= StdDuration::from_secs(5));
    if due {
        *last = Some(now);
        tracing::warn!(
            flows = len,
            capacity,
            "VM TCP flow table full; new connections are refused until flows are reaped"
        );
    }
}

fn reap_closed_flows(
    sockets: &mut SocketSet<'static>,
    flows: &mut HashMap<FlowKey, Flow>,
    metrics: &InterceptionMetrics,
    now: Instant,
) {
    let mut closed = Vec::new();
    let mut expired = Vec::new();
    for (key, flow) in flows.iter() {
        if sockets.get::<tcp::Socket>(flow.handle).state() == tcp::State::Closed {
            closed.push(*key);
        } else if (matches!(flow.phase, FlowPhase::WaitingForSyn | FlowPhase::Connecting)
            || guest_handshake_pending(sockets.get::<tcp::Socket>(flow.handle).state()))
            && now - flow.created_at >= TCP_HANDSHAKE_TIMEOUT
        {
            expired.push(*key);
        }
    }
    for key in closed {
        if let Some(mut flow) = flows.remove(&key) {
            sockets.remove(flow.handle);
            if let Some(task) = flow.upstream_task.take() {
                task.abort();
            }
            metrics.tcp_flow_closed();
        }
    }
    // Abort sends RST and releases the stalled handshake slot on the next pass.
    for key in expired {
        if let Some(flow) = flows.get_mut(&key) {
            sockets.get_mut::<tcp::Socket>(flow.handle).abort();
            if let Some(task) = flow.upstream_task.take() {
                task.abort();
            }
            tracing::warn!(
                ?key,
                "VM TCP flow handshake timed out; aborting to reclaim the slot"
            );
        }
    }
}

fn service_dhcp(
    sockets: &mut SocketSet<'static>,
    handle: SocketHandle,
    guest_address: Ipv4Addr,
    router: Ipv4Addr,
) {
    let socket = sockets.get_mut::<udp::Socket>(handle);
    while socket.can_recv() && socket.can_send() {
        let Ok((request_bytes, _)) = socket.recv() else {
            break;
        };
        let Ok(packet) = DhcpPacket::new_checked(request_bytes) else {
            continue;
        };
        let Ok(request) = DhcpRepr::parse(&packet) else {
            continue;
        };
        let message_type = match request.message_type {
            DhcpMessageType::Discover => DhcpMessageType::Offer,
            DhcpMessageType::Request | DhcpMessageType::Inform => DhcpMessageType::Ack,
            _ => continue,
        };
        let mtu = 1500_u16.to_be_bytes();
        let options = [DhcpOption {
            kind: 26,
            data: &mtu,
        }];
        let response = DhcpRepr {
            message_type,
            transaction_id: request.transaction_id,
            secs: request.secs,
            client_hardware_address: request.client_hardware_address,
            client_ip: Ipv4Address::UNSPECIFIED,
            your_ip: guest_address,
            server_ip: router,
            router: Some(router),
            subnet_mask: Some(Ipv4Address::new(255, 255, 255, 0)),
            relay_agent_ip: Ipv4Address::UNSPECIFIED,
            broadcast: true,
            requested_ip: None,
            client_identifier: request.client_identifier,
            server_identifier: Some(router),
            parameter_request_list: None,
            // DNS is configured by the injected guest resolver file.
            dns_servers: None,
            max_size: request.max_size,
            lease_duration: Some(86_400),
            renew_duration: None,
            rebind_duration: None,
            additional_options: &options,
        };
        let mut bytes = vec![0; response.buffer_len()];
        let mut packet = DhcpPacket::new_unchecked(bytes.as_mut_slice());
        if response.emit(&mut packet).is_err() {
            continue;
        }
        let mut metadata =
            UdpMetadata::from(IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::BROADCAST), 68));
        metadata.local_address = Some(IpAddress::Ipv4(router));
        if let Ok(buffer) = socket.send(bytes.len(), metadata) {
            buffer.copy_from_slice(&bytes);
        }
    }
}

fn service_dns_udp(
    sockets: &mut SocketSet<'static>,
    handle: SocketHandle,
    dns: &mut SyntheticDns,
    metrics: &InterceptionMetrics,
) {
    let socket = sockets.get_mut::<udp::Socket>(handle);
    while socket.can_recv() && socket.can_send() {
        let Ok((query, request_meta)) = socket.recv() else {
            break;
        };
        metrics.dns_query();
        let response = dns.answer(query);
        let mut response_meta = UdpMetadata::from(request_meta.endpoint);
        response_meta.local_address = request_meta.local_address.or(Some(IpAddress::Ipv4(
            dns.peers.as_ref().map_or(ROUTER_IPV4, |peer| peer.gateway),
        )));
        if let Ok(buffer) = socket.send(response.len(), response_meta) {
            buffer.copy_from_slice(&response);
            metrics.dns_answer();
        }
    }
}

#[derive(Default)]
struct SyntheticDns {
    peers: Option<PeerNetworkConfig>,
    by_name: HashMap<String, Ipv4Addr>,
    by_address: HashMap<Ipv4Addr, String>,
}

impl SyntheticDns {
    fn hostname(&self, address: Ipv4Addr) -> Option<&str> {
        self.by_address.get(&address).map(String::as_str)
    }

    fn allocate(&mut self, hostname: &str) -> Option<Ipv4Addr> {
        if let Some(address) = self.by_name.get(hostname) {
            return Some(*address);
        }
        if self.by_name.len() >= SYNTHETIC_DNS_CAPACITY {
            return None;
        }
        let offset = self.by_name.len() as u32 + 1;
        let address = Ipv4Addr::from(u32::from_be_bytes([198, 18, 0, 0]) + offset);
        self.by_name.insert(hostname.to_owned(), address);
        self.by_address.insert(address, hostname.to_owned());
        Some(address)
    }

    fn answer(&mut self, query: &[u8]) -> Vec<u8> {
        let id = query.get(..2).unwrap_or(&[0, 0]);
        let id = [id[0], id[1]];
        let parsed = parse_dns_question(query);
        let (question, rcode, answer) = match parsed {
            Ok(question) if question.opcode != 0 => (Some(question), 4, None),
            Ok(question) if question.class != 1 => (Some(question), 4, None),
            Ok(question) if question.kind == 1 => {
                let peer = self
                    .peers
                    .as_ref()
                    .and_then(|config| crate::peer::lookup_peer_name(config, &question.name));
                let peer_only = self.peers.as_ref().is_some_and(|p| !p.aliases.is_empty())
                    && !question.name.contains('.');
                let localhost = question
                    .name
                    .trim_end_matches('.')
                    .eq_ignore_ascii_case("localhost");
                let answer = if localhost {
                    // Loopback never allocates a synthetic egress address:
                    // resolvers that skip /etc/hosts must still reach the
                    // guest's own loopback, not the VM data plane.
                    Some(Ipv4Addr::LOCALHOST)
                } else if peer.is_some() {
                    peer
                } else if peer_only {
                    None
                } else {
                    self.allocate(&question.name)
                };
                let rcode = if answer.is_some() {
                    0
                } else if peer_only {
                    3
                } else {
                    2
                };
                (Some(question), rcode, answer)
            }
            Ok(question) if question.kind == 28 => (Some(question), 0, None),
            Ok(question) => (Some(question), 4, None),
            Err(()) => (None, 1, None),
        };
        let mut response = Vec::with_capacity(64);
        response.extend_from_slice(&id);
        let request_flags = query
            .get(2..4)
            .map(|flags| u16::from_be_bytes([flags[0], flags[1]]))
            .unwrap_or(0);
        let flags = 0x8000 | (request_flags & 0x0100) | 0x0080 | rcode;
        response.extend_from_slice(&flags.to_be_bytes());
        response.extend_from_slice(&(u16::from(question.is_some())).to_be_bytes());
        response.extend_from_slice(&(u16::from(answer.is_some())).to_be_bytes());
        response.extend_from_slice(&0_u16.to_be_bytes());
        response.extend_from_slice(&0_u16.to_be_bytes());
        if let Some(question) = question {
            response.extend_from_slice(&query[12..question.end]);
            if let Some(address) = answer {
                response.extend_from_slice(&[0xc0, 0x0c]);
                response.extend_from_slice(&1_u16.to_be_bytes());
                response.extend_from_slice(&1_u16.to_be_bytes());
                let ttl: u32 = if self.peers.is_some() { 0 } else { 60 };
                response.extend_from_slice(&ttl.to_be_bytes());
                response.extend_from_slice(&4_u16.to_be_bytes());
                response.extend_from_slice(&address.octets());
            }
        }
        response
    }
}

struct DnsQuestion {
    name: String,
    kind: u16,
    class: u16,
    opcode: u8,
    end: usize,
}

fn parse_dns_question(query: &[u8]) -> Result<DnsQuestion, ()> {
    if query.len() < 12 || u16::from_be_bytes([query[4], query[5]]) != 1 {
        return Err(());
    }
    let flags = u16::from_be_bytes([query[2], query[3]]);
    if flags & 0x8000 != 0 {
        return Err(());
    }
    let mut position = 12;
    let mut labels = Vec::new();
    // RFC 1035's 255-octet wire-name limit includes the root terminator.
    let mut encoded_len = 1;
    loop {
        let length = *query.get(position).ok_or(())? as usize;
        position += 1;
        if length == 0 {
            break;
        }
        if length > 63 || length & 0xc0 != 0 {
            return Err(());
        }
        encoded_len += length + 1;
        if encoded_len > 255 || position + length > query.len() {
            return Err(());
        }
        let label = std::str::from_utf8(&query[position..position + length]).map_err(|_| ())?;
        if !label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(());
        }
        labels.push(label.to_ascii_lowercase());
        position += length;
    }
    if labels.is_empty() || position + 4 > query.len() {
        return Err(());
    }
    let kind = u16::from_be_bytes([query[position], query[position + 1]]);
    let class = u16::from_be_bytes([query[position + 2], query[position + 3]]);
    position += 4;
    Ok(DnsQuestion {
        name: labels.join("."),
        kind,
        class,
        opcode: ((flags >> 11) & 0x0f) as u8,
        end: position,
    })
}

fn synthetic_address(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    octets[0] == 198 && (octets[1] == 18 || octets[1] == 19)
}

/// Hard VM destinations that cannot be enabled by Public mode or by a DNS
/// rebinding result. RFC1918 and loopback remain available intentionally for
/// explicit host/LAN services; pVisor's own virtual and special-purpose ranges
/// do not.
use pvisor_core::forbidden_vm_egress_address as forbidden_host_address;

fn blocked_literal_destination(address: Ipv4Addr) -> bool {
    forbidden_host_address(IpAddr::V4(address))
}

fn initial_tcp_syn(frame: &[u8], guest_address: Ipv4Addr) -> Option<FlowKey> {
    let ethernet = EthernetFrame::new_checked(frame).ok()?;
    if ethernet.ethertype() != EthernetProtocol::Ipv4 {
        return None;
    }
    let ipv4 = Ipv4Packet::new_checked(ethernet.payload()).ok()?;
    let checksum_caps = ChecksumCapabilities::default();
    Ipv4Repr::parse(&ipv4, &checksum_caps).ok()?;
    if ipv4.src_addr() != guest_address
        || ipv4.next_header() != IpProtocol::Tcp
        || ipv4.more_frags()
        || ipv4.frag_offset() != 0
    {
        return None;
    }
    let tcp = TcpPacket::new_checked(ipv4.payload()).ok()?;
    // Validate options, ports and flag combinations exactly as the stack does
    // before allocating a listener. A raw SYN bit and valid checksum alone can
    // otherwise pin a flow slot for a packet the stack will never accept.
    let repr = TcpRepr::parse(
        &tcp,
        &ipv4.src_addr().into(),
        &ipv4.dst_addr().into(),
        &checksum_caps,
    )
    .ok()?;
    if repr.control != TcpControl::Syn || repr.ack_number.is_some() {
        return None;
    }
    Some(FlowKey {
        guest_port: tcp.src_port(),
        destination_addr: ipv4.dst_addr(),
        destination_port: tcp.dst_port(),
    })
}

fn supported_guest_frame(frame: &[u8], guest_address: Ipv4Addr) -> bool {
    let Ok(ethernet) = EthernetFrame::new_checked(frame) else {
        return false;
    };
    if ethernet.ethertype() == EthernetProtocol::Arp {
        return true;
    }
    if ethernet.ethertype() != EthernetProtocol::Ipv4 {
        return false;
    }
    let Ok(ipv4) = Ipv4Packet::new_checked(ethernet.payload()) else {
        return false;
    };
    if !ipv4.verify_checksum()
        || (ipv4.src_addr() != guest_address && !ipv4.src_addr().is_unspecified())
    {
        return false;
    }
    match ipv4.next_header() {
        IpProtocol::Tcp => true,
        IpProtocol::Udp => {
            let payload = ipv4.payload();
            payload.len() >= 4 && matches!(u16::from_be_bytes([payload[2], payload[3]]), 53 | 67)
        }
        _ => false,
    }
}

#[derive(Default)]
struct FrameReader {
    bytes: Vec<u8>,
    offset: usize,
}

impl FrameReader {
    async fn read_more(&mut self, reader: &mut (impl AsyncRead + Unpin)) -> io::Result<usize> {
        if self.offset > 0 && self.offset == self.bytes.len() {
            self.bytes.clear();
            self.offset = 0;
        } else if self.offset > 4096 {
            self.bytes.drain(..self.offset);
            self.offset = 0;
        }
        let mut buffer = [0; 16 * 1024];
        let read = reader.read(&mut buffer).await?;
        self.bytes.extend_from_slice(&buffer[..read]);
        Ok(read)
    }

    fn next_frame(&mut self) -> anyhow::Result<Option<&[u8]>> {
        let [a, b, c, d, ..] = &self.bytes[self.offset..] else {
            return Ok(None);
        };
        let length = u32::from_be_bytes([*a, *b, *c, *d]) as usize;
        anyhow::ensure!(
            (14..=MAX_FRAME_LEN).contains(&length),
            "invalid libkrun Ethernet frame length {length}"
        );
        if self.bytes.len() - self.offset < length + 4 {
            return Ok(None);
        }
        let start = self.offset + 4;
        self.offset = start + length;
        Ok(Some(&self.bytes[start..start + length]))
    }
}

async fn write_frames(
    writer: &mut (impl AsyncWrite + Unpin),
    frames: &VecDeque<Vec<u8>>,
) -> io::Result<usize> {
    let count = frames.len().min(FRAME_BATCH);
    let prefixes: [[u8; 4]; FRAME_BATCH] = std::array::from_fn(|index| {
        (frames.get(index).map_or(0, |frame| frame.len()) as u32).to_be_bytes()
    });
    let mut slices: [io::IoSlice<'_>; FRAME_BATCH * 2] =
        std::array::from_fn(|_| io::IoSlice::new(&[]));
    for (index, frame) in frames.iter().take(count).enumerate() {
        slices[index * 2] = io::IoSlice::new(&prefixes[index]);
        slices[index * 2 + 1] = io::IoSlice::new(frame);
    }
    let mut remaining = &mut slices[..count * 2];
    // Bounded gather write; no per-batch allocation or payload concatenation.
    // Short writes may end in either a prefix or payload.
    while !remaining.is_empty() {
        let written = writer.write_vectored(remaining).await?;
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        io::IoSlice::advance_slices(&mut remaining, written);
    }
    Ok(count)
}

#[derive(Default)]
struct FrameDevice {
    receive_queue: VecDeque<Vec<u8>>,
    transmit_queue: VecDeque<Vec<u8>>,
}

struct FrameRxToken(Vec<u8>);
struct FrameTxToken<'a>(&'a mut VecDeque<Vec<u8>>);

impl Device for FrameDevice {
    type RxToken<'a> = FrameRxToken;
    type TxToken<'a> = FrameTxToken<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let frame = self.receive_queue.pop_front()?;
        Some((FrameRxToken(frame), FrameTxToken(&mut self.transmit_queue)))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(FrameTxToken(&mut self.transmit_queue))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ethernet;
        capabilities.max_transmission_unit = FRAME_MTU;
        capabilities.max_burst_size = None;
        capabilities
    }
}

impl RxToken for FrameRxToken {
    fn consume<R, F>(self, function: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        function(&self.0)
    }
}

impl TxToken for FrameTxToken<'_> {
    fn consume<R, F>(self, length: usize, function: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut frame = vec![0; length];
        let result = function(&mut frame);
        self.0.push_back(frame);
        result
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use crate::api::BandwidthRegistry;
    use crate::api::EgressControl;
    use pvisor_core::network::{NetworkConfig, NetworkMode, NetworkPolicy};

    #[rstest::rstest]
    #[case::initial(1, Some(1), Some(2))]
    #[case::near_exhaustion(u64::MAX - 2, Some(u64::MAX - 2), Some(u64::MAX - 1))]
    #[case::last_generation(u64::MAX - 1, Some(u64::MAX - 1), None)]
    #[case::exhausted(u64::MAX, None, None)]
    fn flow_generations_never_wrap_or_reuse(
        #[case] initial: u64,
        #[case] first: Option<u64>,
        #[case] second: Option<u64>,
    ) {
        use std::sync::atomic::{AtomicU64, Ordering};

        let next = AtomicU64::new(initial);
        assert_eq!(allocate_flow_generation(&next), first);
        assert_eq!(allocate_flow_generation(&next), second);
        if second.is_none() {
            for _ in 0..3 {
                assert_eq!(next.load(Ordering::Relaxed), u64::MAX);
                assert_eq!(allocate_flow_generation(&next), None);
            }
        } else {
            assert_eq!(next.load(Ordering::Relaxed), initial + 2);
        }
    }

    #[rstest::rstest]
    fn concurrent_generation_exhaustion_preserves_unique_ids() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let next = AtomicU64::new(u64::MAX - 24);
        let results = thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        (0..8)
                            .map(|_| allocate_flow_generation(&next))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|id| id.is_none()).count(), 8);
        let mut allocated: Vec<_> = results.into_iter().flatten().collect();
        allocated.sort_unstable();
        assert_eq!(allocated, (u64::MAX - 24..u64::MAX).collect::<Vec<_>>());
        assert_eq!(next.load(Ordering::Relaxed), u64::MAX);
        assert_eq!(allocate_flow_generation(&next), None);
    }

    #[rstest::rstest]
    #[case::dns(53)]
    #[case::dhcp(67)]
    #[case::maximum(65535)]
    fn udp_service_binds_the_requested_port(#[case] port: u16) {
        let socket = udp_socket(1, 512, 1, 512, port).unwrap();
        assert_eq!(socket.endpoint().port, port);
    }

    #[rstest::rstest]
    fn udp_service_rejects_zero_port_without_panicking() {
        let error = udp_socket(1, 512, 1, 512, 0).err().unwrap();
        assert_eq!(error.to_string(), "bind VM UDP service port 0");
    }

    #[rstest::rstest]
    #[case::empty(0)]
    #[case::one_byte(1)]
    #[case::two_bytes(2)]
    #[case::three_bytes(3)]
    fn frame_reader_retains_partial_prefix_after_a_complete_frame(#[case] prefix_bytes: usize) {
        let mut reader = FrameReader::default();
        reader.bytes.extend_from_slice(&14_u32.to_be_bytes());
        reader.bytes.extend_from_slice(&[1; 14]);
        let mut next = 15_u32.to_be_bytes().to_vec();
        next.extend_from_slice(&[2; 15]);
        reader.bytes.extend_from_slice(&next[..prefix_bytes]);
        assert_eq!(reader.next_frame().unwrap(), Some([1; 14].as_slice()));
        let offset = reader.offset;
        assert!(reader.next_frame().unwrap().is_none());
        assert_eq!(reader.offset, offset);
        reader.bytes.extend_from_slice(&next[prefix_bytes..]);
        assert_eq!(reader.next_frame().unwrap(), Some([2; 15].as_slice()));
        assert!(reader.next_frame().unwrap().is_none());
    }

    #[rstest::rstest]
    #[case::zero(0)]
    #[case::below_minimum(13)]
    #[case::above_maximum(65_536)]
    #[case::maximum_integer(u32::MAX)]
    fn frame_reader_rejects_invalid_lengths_without_advancing(#[case] length: u32) {
        let mut reader = FrameReader {
            bytes: length.to_be_bytes().to_vec(),
            offset: 0,
        };
        assert_eq!(
            reader.next_frame().unwrap_err().to_string(),
            format!("invalid libkrun Ethernet frame length {length}")
        );
        assert_eq!(reader.offset, 0);
    }

    #[rstest::rstest]
    #[case::minimum(14)]
    #[case::mtu(FRAME_MTU)]
    #[case::maximum(MAX_FRAME_LEN)]
    fn frame_reader_waits_for_the_complete_payload(#[case] length: usize) {
        let mut reader = FrameReader {
            bytes: (length as u32).to_be_bytes().to_vec(),
            offset: 0,
        };
        reader.bytes.resize(4 + length - 1, 7);
        assert!(reader.next_frame().unwrap().is_none());
        assert_eq!(reader.offset, 0);
        reader.bytes.push(7);
        assert_eq!(
            reader.next_frame().unwrap(),
            Some(vec![7; length].as_slice())
        );
        assert_eq!(reader.offset, 4 + length);
        assert!(reader.next_frame().unwrap().is_none());
    }

    #[rstest::rstest]
    #[tokio::test(start_paused = true)]
    async fn abandoned_startup_returns_an_error_and_releases_transport() {
        let (host, mut guest) = tokio::io::duplex(4096);
        let (_shutdown, shutdown_rx) = oneshot::channel();
        let (started, ready) = std::sync::mpsc::sync_channel(1);
        drop(ready);
        let error = tokio::time::timeout(
            StdDuration::from_secs(1),
            run_backend(
                host,
                config(),
                InterceptionMetrics::default(),
                shutdown_rx,
                None,
                &started,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "VM network startup receiver disconnected"
        );
        assert_eq!(
            tokio::time::timeout(StdDuration::from_secs(1), guest.read_u8())
                .await
                .unwrap()
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    fn config() -> VmNetworkConfig {
        let policy = NetworkPolicy::compile(&NetworkConfig {
            mode: NetworkMode::NoNetwork,
            ..Default::default()
        })
        .unwrap();
        VmNetworkConfig::new(
            EgressRuntime::with_bandwidth_registry(
                policy,
                Arc::new(pvisor_core::PolicyControlController),
                BandwidthRegistry::default(),
            ),
            EgressContext::default(),
        )
    }

    #[rstest::rstest]
    fn transport_adoption_errors_are_synchronous_and_an_idle_driver_can_shutdown() {
        let error = VmNetwork::start_with_io::<tokio::io::DuplexStream, _>(config(), || {
            Err(io::Error::other("transport adoption fixture"))
        })
        .err()
        .expect("adoption must fail");
        assert!(format!("{error:#}").contains("transport adoption fixture"));
        let (host, _idle_guest) = tokio::io::duplex(4096);
        let network = VmNetwork::start_with_io(config(), move || {
            assert!(tokio::runtime::Handle::try_current().is_ok());
            Ok(host)
        })
        .unwrap();
        network.shutdown().unwrap();
    }
}

impl VmNetworkConfiguration for VmNetworkConfig {
    fn new(egress: EgressRuntime, context: EgressContext) -> Self {
        Self {
            egress,
            context,
            gateway: None,
            max_flows: DEFAULT_MAX_FLOWS,
            metrics: InterceptionMetrics::default(),
            peer_network: None,
        }
    }
    fn guest_mac(address: Ipv4Addr) -> [u8; 6] {
        let [a, b, c, d] = address.octets();
        if address == GUEST_IPV4 {
            [0x02, 0x50, 0x56, 0x00, 0x00, 0x02]
        } else {
            [0x02, 0x50, a, b, c, d]
        }
    }
}
impl VmNetworkRuntime for VmNetwork {
    fn start_with_io<T, F>(config: VmNetworkConfig, make_stream: F) -> anyhow::Result<Self>
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        F: FnOnce() -> io::Result<T> + Send + 'static,
    {
        anyhow::ensure!(
            config.max_flows > 0,
            "VM network max_flows must be non-zero"
        );
        let peer = config
            .peer_network
            .as_ref()
            .map(|peer| {
                anyhow::ensure!(
                    config.egress.permits_peer_network(),
                    "peer networking requires EgressRuntime::for_peer_network with unrestricted policy and no bandwidth limits"
                );
                BoundPeer::bind(peer, config.context.execution_id.clone())
            })
            .transpose()?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (started, ready) = std::sync::mpsc::sync_channel(1);
        let metrics = config.metrics.clone();
        let thread_metrics = metrics.clone();
        let thread = thread::Builder::new()
            .name("pvisor-smoltcp".into())
            .spawn(move || {
                let result = (|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .context("build VM network runtime")?;
                    runtime.block_on(async {
                        let stream = make_stream().context("adopt VM network transport")?;
                        let peer = peer.map(|p| p.adopt(thread_metrics.clone())).transpose()?;
                        run_backend(stream, config, thread_metrics, shutdown_rx, peer, &started)
                            .await
                    })
                })();
                if let Err(error) = &result {
                    let _ = started.send(Err(format!("{error:#}")));
                }
                result
            })
            .context("spawn VM network backend")?;
        match ready.recv().context("VM network startup disconnected")? {
            Ok(()) => Ok(Self {
                shutdown: Some(shutdown_tx),
                thread: Some(thread),
                metrics,
            }),
            Err(error) => {
                let _ = shutdown_tx.send(());
                let _ = thread.join();
                Err(anyhow::anyhow!(error))
            }
        }
    }
    fn shutdown(mut self) -> anyhow::Result<InterceptionSnapshot> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("VM network backend panicked"))??;
        }
        Ok(self.metrics.snapshot())
    }
}

#[cfg(all(test, unix))]
mod tests {

    #[rstest::rstest]
    fn dns_tcp_stops_processing_queries_when_responses_are_full() {
        let mut sockets = SocketSet::new(Vec::new());
        let handle = sockets.add(tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; 1024]),
            tcp::SocketBuffer::new(vec![0; 1024]),
        ));
        let query = query("bounded.example", 1);
        let mut input = (query.len() as u16).to_be_bytes().to_vec();
        input.extend_from_slice(&query);
        let mut flow = Flow {
            generation: next_flow_generation().unwrap(),
            handle,
            destination: FlowDestination::Egress {
                host: "dns".into(),
                port: 53,
            },
            phase: FlowPhase::LocalDns,
            created_at: Instant::ZERO,
            upstream: None,
            upstream_task: None,
            inbound: (0..FLOW_BUFFER_CHUNKS)
                .map(|_| (vec![0; DNS_MAX_MESSAGE + 2], None))
                .collect(),
            inbound_offset: 0,
            dns_input: input.clone(),
            remote_eof: false,
        };
        let mut dns = SyntheticDns::default();
        for _ in 0..100 {
            drive_dns_tcp(
                sockets.get_mut::<tcp::Socket>(handle),
                &mut flow,
                &mut dns,
                &InterceptionMetrics::default(),
            );
            assert_eq!(flow.inbound.len(), FLOW_BUFFER_CHUNKS);
            assert_eq!(flow.dns_input, input);
        }
        flow.inbound.clear();
        drive_dns_tcp(
            sockets.get_mut::<tcp::Socket>(handle),
            &mut flow,
            &mut dns,
            &InterceptionMetrics::default(),
        );
        assert!(flow.dns_input.is_empty());
        assert_eq!(flow.inbound.len(), 1);
    }
    use super::*;

    #[rstest::rstest]
    #[tokio::test]
    async fn slow_guest_backpressures_large_download_without_losing_data() {
        tokio::time::timeout(StdDuration::from_secs(10), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let stream = TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let (mut server, _) = listener.accept().await.unwrap();
            let payload: Vec<u8> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
            let expected = payload.clone();
            let server_task = tokio::spawn(async move {
                server.write_all(&payload).await.unwrap();
                server.shutdown().await.unwrap();
            });
            let key = FlowKey {
                guest_port: 49152,
                destination_addr: Ipv4Addr::new(198, 18, 0, 1),
                destination_port: 80,
            };
            let (sender, outbound) = mpsc::channel(FLOW_BUFFER_CHUNKS);
            let (events, mut received) = mpsc::channel(TCP_CHANNEL_DEPTH);
            let bridge = tokio::spawn(bridge_upstream(
                key,
                next_flow_generation().unwrap(),
                stream,
                outbound,
                Default::default(),
                events,
            ));
            drop(sender);
            let mut bytes = Vec::new();
            let mut held = Vec::new();
            for _ in 0..FLOW_BUFFER_CHUNKS {
                match received.recv().await.unwrap() {
                    FlowEvent::Data {
                        bytes: chunk,
                        permit,
                        ..
                    } => {
                        bytes.extend(chunk);
                        held.push(permit);
                    }
                    _ => panic!("expected download data"),
                }
            }
            assert!(bytes.len() <= TCP_BUFFER_BYTES);
            // A stalled guest must stop this flow's reader, not abort the TCP
            // connection or keep filling the shared event channel.
            assert!(
                tokio::time::timeout(StdDuration::from_millis(50), received.recv())
                    .await
                    .is_err()
            );
            drop(held);
            loop {
                match received.recv().await.unwrap() {
                    FlowEvent::Data { bytes: chunk, .. } => bytes.extend(chunk),
                    FlowEvent::RemoteEof(_, _) => break,
                    _ => panic!("download failed while guest resumed"),
                }
            }
            assert_eq!(bytes, expected);
            bridge.await.unwrap();
            server_task.await.unwrap();
        })
        .await
        .expect("download stalled after guest resumed");
    }

    #[rstest::rstest]
    fn denied_vm_flow_records_its_destination() {
        let key = FlowKey {
            guest_port: 49152,
            destination_addr: Ipv4Addr::new(198, 18, 0, 1),
            destination_port: 443,
        };
        let mut sockets = SocketSet::new(Vec::new());
        let handle = sockets.add(tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; 1024]),
            tcp::SocketBuffer::new(vec![0; 1024]),
        ));
        let mut flows = HashMap::from([(
            key,
            Flow {
                generation: next_flow_generation().unwrap(),
                handle,
                destination: FlowDestination::Egress {
                    host: "blocked.example".into(),
                    port: 443,
                },
                phase: FlowPhase::Connecting,
                created_at: Instant::ZERO,
                upstream: None,
                upstream_task: None,
                inbound: VecDeque::new(),
                inbound_offset: 0,
                dns_input: Vec::new(),
                remote_eof: false,
            },
        )]);
        let metrics = InterceptionMetrics::default();
        apply_flow_event(
            FlowEvent::ConnectFailed {
                key,
                generation: flows[&key].generation,
                denied: true,
                error: "explicit deny".into(),
            },
            &mut flows,
            &mut sockets,
            &metrics,
        );
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.tcp_flows_denied, 1);
        assert_eq!(snapshot.targets["TCP blocked.example:443"].denied, 1);
    }

    fn query(name: &str, kind: u16) -> Vec<u8> {
        let mut bytes = vec![0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            bytes.push(label.len() as u8);
            bytes.extend_from_slice(label.as_bytes());
        }
        bytes.push(0);
        bytes.extend_from_slice(&kind.to_be_bytes());
        bytes.extend_from_slice(&1_u16.to_be_bytes());
        bytes
    }

    #[rstest::rstest]
    fn synthetic_dns_is_stable_and_does_not_resolve_on_the_host() {
        let mut dns = SyntheticDns::default();
        let first = dns.answer(&query("Api.Example.COM", 1));
        let second = dns.answer(&query("api.example.com", 1));
        assert_eq!(&first[first.len() - 4..], &second[second.len() - 4..]);
        assert_eq!(dns.by_name.len(), 1);
        assert_eq!(
            dns.hostname(Ipv4Addr::new(198, 18, 0, 1)),
            Some("api.example.com")
        );
    }

    #[rstest::rstest]
    fn localhost_resolves_to_loopback_without_allocating() {
        let mut dns = SyntheticDns::default();
        for name in ["localhost", "LOCALHOST"] {
            let response = dns.answer(&query(name, 1));
            assert_eq!(
                u16::from_be_bytes([response[2], response[3]]) & 0x0f,
                0,
                "{name} must resolve"
            );
            assert_eq!(&response[response.len() - 4..], &[127, 0, 0, 1], "{name}");
        }
        assert!(
            dns.by_name.is_empty(),
            "loopback must not allocate a synthetic address"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stalled_handshake_flows_are_reclaimed_after_the_deadline() {
        let key = FlowKey {
            guest_port: 40000,
            destination_addr: Ipv4Addr::new(198, 18, 0, 9),
            destination_port: 443,
        };
        let mut sockets = SocketSet::new(Vec::new());
        let mut socket = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; 1024]),
            tcp::SocketBuffer::new(vec![0; 1024]),
        );
        socket
            .listen(IpEndpoint::new(
                IpAddress::Ipv4(key.destination_addr),
                key.destination_port,
            ))
            .unwrap();
        let handle = sockets.add(socket);
        let task = tokio::spawn(std::future::pending::<()>());
        let probe = task.abort_handle();
        let mut flows = HashMap::from([(
            key,
            Flow {
                generation: next_flow_generation().unwrap(),
                handle,
                destination: FlowDestination::Egress {
                    host: "slow.example".into(),
                    port: 443,
                },
                phase: FlowPhase::Connecting,
                created_at: Instant::ZERO,
                upstream: None,
                upstream_task: Some(task),
                inbound: VecDeque::new(),
                inbound_offset: 0,
                dns_input: Vec::new(),
                remote_eof: false,
            },
        )]);
        let metrics = InterceptionMetrics::default();
        // Before the deadline the stalled handshake keeps its slot.
        reap_closed_flows(
            &mut sockets,
            &mut flows,
            &metrics,
            Instant::ZERO + Duration::from_secs(5),
        );
        assert!(flows.contains_key(&key));
        // Past the deadline the socket is aborted (its upstream task with
        // it) and the slot returns on the next pass.
        reap_closed_flows(
            &mut sockets,
            &mut flows,
            &metrics,
            Instant::ZERO + Duration::from_secs(31),
        );
        tokio::task::yield_now().await;
        assert!(probe.is_finished());
        reap_closed_flows(
            &mut sockets,
            &mut flows,
            &metrics,
            Instant::ZERO + Duration::from_secs(32),
        );
        assert!(flows.is_empty());
    }

    #[rstest::rstest]
    fn upstream_connected_without_guest_ack_still_expires() {
        let destination = ROUTER_IPV4;
        let key = FlowKey {
            guest_port: 40000,
            destination_addr: destination,
            destination_port: 443,
        };
        let mut device = FrameDevice::default();
        let mut config = InterfaceConfig::new(HardwareAddress::Ethernet(ROUTER_MAC));
        config.random_seed = 1;
        let mut interface = Interface::new(config, &mut device, Instant::ZERO);
        interface.update_ip_addrs(|addresses| {
            addresses
                .push(IpCidr::new(IpAddress::Ipv4(destination), 24))
                .unwrap()
        });
        let mut sockets = SocketSet::new(Vec::new());
        let mut socket = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; 1024]),
            tcp::SocketBuffer::new(vec![0; 1024]),
        );
        socket.listen((destination, 443)).unwrap();
        socket.pause_synack(true);
        let handle = sockets.add(socket);
        let mut frame = vec![0; 14 + 20 + 20];
        let mut ethernet = EthernetFrame::new_unchecked(&mut frame);
        ethernet.set_src_addr(EthernetAddress(VmNetworkConfig::guest_mac(GUEST_IPV4)));
        ethernet.set_dst_addr(ROUTER_MAC);
        ethernet.set_ethertype(EthernetProtocol::Ipv4);
        let mut ip = Ipv4Packet::new_unchecked(ethernet.payload_mut());
        ip.set_version(4);
        ip.set_header_len(20);
        ip.set_total_len(40);
        ip.set_hop_limit(64);
        ip.set_next_header(IpProtocol::Tcp);
        ip.set_src_addr(GUEST_IPV4);
        ip.set_dst_addr(destination);
        ip.fill_checksum();
        let mut tcp = TcpPacket::new_unchecked(ip.payload_mut());
        tcp.set_src_port(key.guest_port);
        tcp.set_dst_port(key.destination_port);
        tcp.set_header_len(20);
        tcp.set_syn(true);
        tcp.set_window_len(1024);
        tcp.set_seq_number(smoltcp::wire::TcpSeqNumber(123));
        tcp.fill_checksum(&IpAddress::Ipv4(GUEST_IPV4), &IpAddress::Ipv4(destination));
        device.receive_queue.push_back(frame);
        interface.poll(Instant::ZERO, &mut device, &mut sockets);
        assert_eq!(
            sockets.get::<tcp::Socket>(handle).state(),
            tcp::State::SynReceived
        );
        let mut flows = HashMap::from([(
            key,
            Flow {
                generation: next_flow_generation().unwrap(),
                handle,
                destination: FlowDestination::Gateway("127.0.0.1:443".parse().unwrap()),
                phase: FlowPhase::Connecting,
                created_at: Instant::ZERO,
                upstream: None,
                upstream_task: None,
                inbound: VecDeque::new(),
                inbound_offset: 0,
                dns_input: Vec::new(),
                remote_eof: false,
            },
        )]);
        let (sender, _receiver) = mpsc::channel(1);
        let metrics = InterceptionMetrics::default();
        apply_flow_event(
            FlowEvent::Connected {
                key,
                generation: flows[&key].generation,
                sender,
                policy_authorized: false,
            },
            &mut flows,
            &mut sockets,
            &metrics,
        );
        assert_eq!(flows[&key].phase, FlowPhase::Connected);
        reap_closed_flows(
            &mut sockets,
            &mut flows,
            &metrics,
            Instant::ZERO + Duration::from_secs(29),
        );
        assert_eq!(
            sockets.get::<tcp::Socket>(handle).state(),
            tcp::State::SynReceived
        );
        reap_closed_flows(
            &mut sockets,
            &mut flows,
            &metrics,
            Instant::ZERO + Duration::from_secs(31),
        );
        reap_closed_flows(
            &mut sockets,
            &mut flows,
            &metrics,
            Instant::ZERO + Duration::from_secs(32),
        );
        assert!(flows.is_empty());
    }

    #[rstest::rstest]
    fn aaaa_returns_nodata_and_unknown_type_returns_notimp() {
        let mut dns = SyntheticDns::default();
        let aaaa = dns.answer(&query("example.com", 28));
        assert_eq!(u16::from_be_bytes([aaaa[6], aaaa[7]]), 0);
        assert_eq!(u16::from_be_bytes([aaaa[2], aaaa[3]]) & 0x0f, 0);
        let txt = dns.answer(&query("example.com", 16));
        assert_eq!(u16::from_be_bytes([txt[2], txt[3]]) & 0x0f, 4);
    }

    #[rstest::rstest]
    fn malformed_dns_returns_formerr() {
        let mut dns = SyntheticDns::default();
        let response = dns.answer(&[0xab, 0xcd]);
        assert_eq!(&response[..2], &[0xab, 0xcd]);
        assert_eq!(u16::from_be_bytes([response[2], response[3]]) & 0x0f, 1);
    }

    #[rstest::rstest]
    fn virtual_and_link_local_destinations_are_never_literal_egress() {
        for address in [
            ROUTER_IPV4,
            GUEST_IPV4,
            Ipv4Addr::new(198, 18, 1, 1),
            Ipv4Addr::new(169, 254, 169, 254),
            Ipv4Addr::new(224, 0, 0, 1),
            Ipv4Addr::BROADCAST,
        ] {
            assert!(blocked_literal_destination(address), "{address}");
        }
        assert!(!blocked_literal_destination(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(!blocked_literal_destination(Ipv4Addr::new(
            93, 184, 216, 34
        )));
    }

    #[rstest::rstest]
    fn frame_reader_handles_fragmented_and_coalesced_frames() {
        let mut reader = FrameReader::default();
        reader.bytes.extend_from_slice(&14_u32.to_be_bytes());
        reader.bytes.extend_from_slice(&[1; 14]);
        reader.bytes.extend_from_slice(&15_u32.to_be_bytes());
        reader.bytes.extend_from_slice(&[2; 15]);
        assert_eq!(reader.next_frame().unwrap().unwrap(), vec![1; 14]);
        assert_eq!(reader.next_frame().unwrap().unwrap(), vec![2; 15]);
        assert!(reader.next_frame().unwrap().is_none());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn batched_guest_writes_preserve_framing_under_socket_backpressure() {
        use std::os::fd::AsRawFd;
        let (left, mut right) = tokio::net::UnixStream::pair().unwrap();
        let bytes: libc::c_int = 1024;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    left.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&bytes as *const libc::c_int).cast(),
                    std::mem::size_of_val(&bytes) as libc::socklen_t,
                )
            },
            0
        );
        let mut frames = VecDeque::new();
        let mut expected = Vec::new();
        for sequence in 0..FRAME_BATCH + 7 {
            let frame = vec![sequence as u8; if sequence % 2 == 0 { 1514 } else { 66 }];
            expected.extend_from_slice(&(frame.len() as u32).to_be_bytes());
            expected.extend_from_slice(&frame);
            frames.push_back(frame);
        }
        let task = tokio::spawn(async move {
            let (_, mut writer) = left.into_split();
            while !frames.is_empty() {
                let count = write_frames(&mut writer, &frames).await.unwrap();
                assert!((1..=FRAME_BATCH).contains(&count));
                frames.drain(..count);
            }
        });
        let mut arrived = Vec::new();
        tokio::time::timeout(StdDuration::from_secs(5), async {
            let mut chunk = [0; 13];
            loop {
                let read = right.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                arrived.extend_from_slice(&chunk[..read]);
                tokio::task::yield_now().await;
            }
            task.await.unwrap();
        })
        .await
        .unwrap();
        assert_eq!(arrived, expected);
    }

    #[rstest::rstest]
    fn host_connection_does_not_imply_the_guest_handshake_is_complete() {
        assert!(guest_handshake_pending(tcp::State::SynReceived));
        assert!(!guest_handshake_pending(tcp::State::Established));
        assert!(!guest_handshake_pending(tcp::State::CloseWait));
    }
}

#[cfg(all(test, unix))]
mod flow_lifecycle_regressions {
    use super::*;
    use crate::api::EgressControl;

    #[rstest::rstest]
    #[tokio::test]
    async fn queued_events_do_not_mutate_reused_flow_key() {
        let policy = pvisor_core::network::NetworkPolicy::compile(
            &pvisor_core::network::NetworkConfig::default(),
        )
        .unwrap();
        let mut config = VmNetworkConfig::new(
            EgressRuntime::with_bandwidth_registry(
                policy,
                Arc::new(pvisor_core::PolicyControlController),
                crate::api::BandwidthRegistry::default(),
            ),
            EgressContext::default(),
        );
        config.gateway = Some(crate::api::VmGatewayRoute {
            guest_port: 443,
            host: "127.0.0.1:443".parse().unwrap(),
        });
        let key = FlowKey {
            guest_port: 40000,
            destination_addr: ROUTER_IPV4,
            destination_port: 443,
        };
        let mut flows = HashMap::new();
        let mut sockets = SocketSet::new(Vec::new());
        let dns = SyntheticDns::default();
        let metrics = InterceptionMetrics::default();
        ensure_listener(
            key,
            &mut flows,
            &mut sockets,
            &dns,
            &config,
            &metrics,
            Instant::ZERO,
        );
        let generation = flows[&key].generation;
        let old_handle = flows[&key].handle;
        let (events, mut queued) = mpsc::channel(TCP_CHANNEL_DEPTH);
        let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
        events
            .send(FlowEvent::Data {
                key,
                generation,
                bytes: b"old TCP connection payload".to_vec(),
                permit,
            })
            .await
            .unwrap();
        events
            .send(FlowEvent::RemoteEof(key, generation))
            .await
            .unwrap();
        events
            .send(FlowEvent::BridgeFailed {
                key,
                generation,
                error: "old TCP failure".into(),
            })
            .await
            .unwrap();
        let (old_sender, _old_receiver) = mpsc::channel(1);
        for event in [
            FlowEvent::Connected {
                key,
                generation,
                sender: old_sender,
                policy_authorized: true,
            },
            FlowEvent::Uploaded {
                key,
                generation,
                bytes: 123,
            },
            FlowEvent::ConnectFailed {
                key,
                generation,
                denied: true,
                error: "old denial".into(),
            },
        ] {
            events.send(event).await.unwrap();
        }
        // The production guest RST path moves this socket to Closed, then calls reap.
        sockets.get_mut::<tcp::Socket>(old_handle).abort();
        reap_closed_flows(&mut sockets, &mut flows, &metrics, Instant::ZERO);
        assert!(flows.is_empty());
        // A new SYN can be selected from the input before queued async events drain.
        ensure_listener(
            key,
            &mut flows,
            &mut sockets,
            &dns,
            &config,
            &metrics,
            Instant::from_millis(1),
        );
        assert!(flows[&key].inbound.is_empty());
        assert!(!flows[&key].remote_eof);
        apply_flow_event(
            queued.recv().await.unwrap(),
            &mut flows,
            &mut sockets,
            &metrics,
        );
        assert!(flows[&key].inbound.is_empty());
        apply_flow_event(
            queued.recv().await.unwrap(),
            &mut flows,
            &mut sockets,
            &metrics,
        );
        assert!(!flows[&key].remote_eof);
        apply_flow_event(
            queued.recv().await.unwrap(),
            &mut flows,
            &mut sockets,
            &metrics,
        );
        assert_eq!(
            sockets.get::<tcp::Socket>(flows[&key].handle).state(),
            tcp::State::Listen
        );
        for _ in 0..3 {
            apply_flow_event(
                queued.recv().await.unwrap(),
                &mut flows,
                &mut sockets,
                &metrics,
            );
        }
        assert!(flows[&key].upstream.is_none());
        assert_eq!(flows[&key].phase, FlowPhase::WaitingForSyn);
        assert_eq!(
            sockets.get::<tcp::Socket>(flows[&key].handle).state(),
            tcp::State::Listen
        );
        // Current-generation events must still be delivered after stale events.
        let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
        apply_flow_event(
            FlowEvent::Data {
                key,
                generation: flows[&key].generation,
                bytes: b"new".to_vec(),
                permit,
            },
            &mut flows,
            &mut sockets,
            &metrics,
        );
        assert_eq!(flows[&key].inbound.front().unwrap().0, b"new");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn remote_reset_cancels_idle_upload_and_reports_failure() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        socket2::SockRef::from(&server)
            .set_linger(Some(StdDuration::ZERO))
            .unwrap();
        drop(server);
        tokio::time::timeout(StdDuration::from_secs(2), stream.readable())
            .await
            .unwrap()
            .unwrap();
        let key = FlowKey {
            guest_port: 40001,
            destination_addr: ROUTER_IPV4,
            destination_port: 443,
        };
        let (sender, outbound) = mpsc::channel(FLOW_BUFFER_CHUNKS);
        let (events, mut received) = mpsc::channel(TCP_CHANNEL_DEPTH);
        let bridge = tokio::spawn(bridge_upstream(
            key,
            next_flow_generation().unwrap(),
            stream,
            outbound,
            Default::default(),
            events,
        ));
        let event = tokio::time::timeout(StdDuration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event, FlowEvent::BridgeFailed { .. }));
        tokio::time::timeout(StdDuration::from_secs(2), bridge)
            .await
            .unwrap()
            .unwrap();
        drop(sender);
    }
    #[rstest::rstest]
    #[tokio::test]
    async fn upstream_half_close_keeps_upload_alive() {
        tokio::time::timeout(StdDuration::from_secs(3), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let stream = TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let (mut server, _) = listener.accept().await.unwrap();
            let key = FlowKey {
                guest_port: 40002,
                destination_addr: ROUTER_IPV4,
                destination_port: 443,
            };
            let (sender, outbound) = mpsc::channel(FLOW_BUFFER_CHUNKS);
            let (events, mut received) = mpsc::channel(TCP_CHANNEL_DEPTH);
            let bridge = tokio::spawn(bridge_upstream(
                key,
                next_flow_generation().unwrap(),
                stream,
                outbound,
                Default::default(),
                events,
            ));
            server.shutdown().await.unwrap();
            assert!(matches!(
                received.recv().await.unwrap(),
                FlowEvent::RemoteEof(_, _)
            ));
            assert!(!bridge.is_finished());
            sender.send(b"after EOF".to_vec()).await.unwrap();
            drop(sender);
            let mut bytes = Vec::new();
            server.read_to_end(&mut bytes).await.unwrap();
            assert_eq!(bytes, b"after EOF");
            bridge.await.unwrap();
            while let Some(event) = received.recv().await {
                assert!(!matches!(event, FlowEvent::BridgeFailed { .. }));
            }
        })
        .await
        .unwrap();
    }
}
