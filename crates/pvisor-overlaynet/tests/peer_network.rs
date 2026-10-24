//! Contract tests use only the public network owner and framed guest link.
use std::io::{Read, Write};
use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use pvisor_overlaynet::vm::{VmNetwork, VmNetworkConfig, guest_mac};
use pvisor_overlaynet::{
    BandwidthRegistry, EgressContext, EgressRuntime, InterceptionMetrics, NetworkConfig,
    NetworkMode, NetworkPolicy, PeerNetworkConfig,
};
use smoltcp::wire::{EthernetAddress, EthernetFrame, EthernetProtocol, IpProtocol, Ipv4Packet};

fn directory() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap()
}

fn address(last: u8) -> Ipv4Addr {
    Ipv4Addr::new(192, 0, 2, last)
}

fn config(directory: &Path, last: u8) -> VmNetworkConfig {
    let policy = NetworkPolicy::compile(&NetworkConfig::default()).unwrap();
    let egress = EgressRuntime::for_peer_network(policy, BandwidthRegistry::default()).unwrap();
    let mut config = VmNetworkConfig::new(egress, EgressContext::default());
    config.peer_network = Some(PeerNetworkConfig {
        directory: directory.to_path_buf(),
        address: address(last),
        gateway: address(1),
        aliases: vec![],
    });
    config
}

fn start(directory: &Path, last: u8) -> (VmNetwork, UnixStream, InterceptionMetrics) {
    let config = config(directory, last);
    let metrics = config.metrics.clone();
    let (network, stream) = VmNetwork::start(config).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    (network, stream, metrics)
}

fn ip_frame(src: u8, dst: u8, protocol: IpProtocol) -> Vec<u8> {
    let mut frame = vec![0; 14 + 20 + 32];
    let mut ethernet = EthernetFrame::new_unchecked(frame.as_mut_slice());
    ethernet.set_src_addr(EthernetAddress(guest_mac(address(src))));
    ethernet.set_dst_addr(EthernetAddress(guest_mac(address(dst))));
    ethernet.set_ethertype(EthernetProtocol::Ipv4);
    let mut ip = Ipv4Packet::new_unchecked(ethernet.payload_mut());
    ip.set_version(4);
    ip.set_header_len(20);
    ip.set_total_len(52);
    ip.set_hop_limit(64);
    ip.set_next_header(protocol);
    ip.set_src_addr(address(src));
    ip.set_dst_addr(address(dst));
    // The guest owns transport validation; the packet plane preserves bytes.
    ip.payload_mut().fill(0x5a);
    ip.fill_checksum();
    frame
}

fn arp(src: u8, dst: u8, reply: bool) -> Vec<u8> {
    let mut frame = vec![0; 42];
    let destination = if reply {
        guest_mac(address(dst))
    } else {
        [255; 6]
    };
    frame[..6].copy_from_slice(&destination);
    frame[6..12].copy_from_slice(&guest_mac(address(src)));
    frame[12..22].copy_from_slice(&[8, 6, 0, 1, 8, 0, 6, 4, 0, if reply { 2 } else { 1 }]);
    frame[22..28].copy_from_slice(&guest_mac(address(src)));
    frame[28..32].copy_from_slice(&address(src).octets());
    if reply {
        frame[32..38].copy_from_slice(&guest_mac(address(dst)));
    }
    frame[38..42].copy_from_slice(&address(dst).octets());
    frame
}

fn send(stream: &mut UnixStream, frame: &[u8]) {
    stream
        .write_all(&(frame.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(frame).unwrap();
}

fn receive(stream: &mut UnixStream) -> Vec<u8> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).unwrap();
    let length = u32::from_be_bytes(length) as usize;
    assert!(length <= 1514);
    let mut frame = vec![0; length];
    stream.read_exact(&mut frame).unwrap();
    frame
}

fn wait_until(mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while !check() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "peer condition timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn forwards_arp_tcp_udp_and_icmp_bidirectionally_without_proxy_flows() {
    let root = directory();
    let (a, mut sa, ma) = start(root.path(), 11);
    let (b, mut sb, mb) = start(root.path(), 12);
    for packet in [
        arp(11, 12, false),
        ip_frame(11, 12, IpProtocol::Tcp),
        ip_frame(11, 12, IpProtocol::Udp),
        ip_frame(11, 12, IpProtocol::Icmp),
    ] {
        send(&mut sa, &packet);
        assert_eq!(receive(&mut sb), packet);
    }
    for packet in [
        arp(12, 11, true),
        ip_frame(12, 11, IpProtocol::Tcp),
        ip_frame(12, 11, IpProtocol::Udp),
        ip_frame(12, 11, IpProtocol::Icmp),
    ] {
        send(&mut sb, &packet);
        assert_eq!(receive(&mut sa), packet);
    }
    let a = a.shutdown().unwrap();
    let b = b.shutdown().unwrap();
    assert_eq!(a.peer_packets_sent, 4);
    assert_eq!(b.peer_packets_received, 4);
    assert_eq!(ma.snapshot().tcp_flows_opened, 0);
    assert_eq!(mb.snapshot().tcp_flows_opened, 0);
    assert!(!root.path().join("11/net.sock").exists());
    assert!(!root.path().join("12/peer.json").exists());
}

#[test]
fn duplicate_address_fails_synchronously_and_restart_refreshes_generation() {
    let root = directory();
    let (a, mut sa, ma) = start(root.path(), 11);
    let (b, mut sb, _) = start(root.path(), 12);
    assert!(
        VmNetwork::start(config(root.path(), 12))
            .err()
            .unwrap()
            .to_string()
            .contains("already in use")
    );
    let packet = ip_frame(11, 12, IpProtocol::Udp);
    send(&mut sa, &packet);
    assert_eq!(receive(&mut sb), packet);
    b.shutdown().unwrap();
    let (b, mut sb, mb) = start(root.path(), 12);
    // A's cache initially still holds the previous destination generation.
    send(&mut sa, &packet);
    wait_until(|| ma.snapshot().peer_packets_dropped + mb.snapshot().peer_packets_dropped > 0);
    assert_eq!(mb.snapshot().peer_packets_received, 0);
    std::thread::sleep(Duration::from_millis(275));
    send(&mut sa, &packet);
    assert_eq!(receive(&mut sb), packet);
    a.shutdown().unwrap();
    b.shutdown().unwrap();
}

#[test]
fn rejects_unsafe_directories_invalid_addresses_and_restrictive_egress() {
    let root = directory();
    for last in [0, 1, 255] {
        assert!(VmNetwork::start(config(root.path(), last)).is_err());
    }
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(root.path(), &alias).unwrap();
    assert!(VmNetwork::start(config(&alias, 11)).is_err());
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(VmNetwork::start(config(root.path(), 11)).is_err());
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    for mode in [NetworkMode::NoNetwork, NetworkMode::Allowlist] {
        let cfg = config(root.path(), 11);
        cfg.egress.replace_policy(
            NetworkPolicy::compile(&NetworkConfig {
                mode,
                ..Default::default()
            })
            .unwrap(),
        );
        assert!(
            VmNetwork::start(cfg)
                .err()
                .unwrap()
                .to_string()
                .contains("unrestricted")
        );
    }
}

#[test]
fn rejects_spoofing_fragments_and_unknown_peers_without_egress_fallback() {
    let root = directory();
    let (a, mut sa, ma) = start(root.path(), 11);
    let (b, mut sb, mb) = start(root.path(), 12);
    let mut spoof = ip_frame(13, 12, IpProtocol::Udp);
    send(&mut sa, &spoof);
    spoof = ip_frame(11, 12, IpProtocol::Udp);
    spoof[6] ^= 1;
    send(&mut sa, &spoof);
    let mut fragment = ip_frame(11, 12, IpProtocol::Udp);
    let mut ip = Ipv4Packet::new_unchecked(&mut fragment[14..]);
    ip.set_more_frags(true);
    ip.fill_checksum();
    send(&mut sa, &fragment);
    send(&mut sa, &ip_frame(11, 13, IpProtocol::Tcp));
    wait_until(|| ma.snapshot().peer_packets_dropped == 4);
    let good = ip_frame(11, 12, IpProtocol::Udp);
    send(&mut sa, &good);
    assert_eq!(receive(&mut sb), good);
    assert_eq!(mb.snapshot().peer_packets_received, 1);
    assert_eq!(ma.snapshot().tcp_flows_opened, 0);
    a.shutdown().unwrap();
    b.shutdown().unwrap();
}

#[test]
fn live_policy_replacement_closes_the_packet_gate() {
    let root = directory();
    let cfg = config(root.path(), 11);
    let egress = cfg.egress.clone();
    let ma = cfg.metrics.clone();
    let (a, mut sa) = VmNetwork::start(cfg).unwrap();
    let (b, mut sb, mb) = start(root.path(), 12);
    egress.replace_policy(
        NetworkPolicy::compile(&NetworkConfig {
            mode: NetworkMode::NoNetwork,
            ..Default::default()
        })
        .unwrap(),
    );
    send(&mut sa, &ip_frame(11, 12, IpProtocol::Udp));
    send(&mut sb, &ip_frame(12, 11, IpProtocol::Udp));
    wait_until(|| ma.snapshot().peer_packets_dropped == 2);
    assert_eq!(mb.snapshot().peer_packets_received, 0);
    a.shutdown().unwrap();
    b.shutdown().unwrap();
}

#[test]
fn different_network_directories_do_not_discover_each_other() {
    let one = directory();
    let two = directory();
    let (a, mut sa, ma) = start(one.path(), 11);
    let (b, _sb, mb) = start(two.path(), 12);
    send(&mut sa, &ip_frame(11, 12, IpProtocol::Udp));
    wait_until(|| ma.snapshot().peer_packets_dropped == 1);
    assert_eq!(mb.snapshot().peer_packets_received, 0);
    a.shutdown().unwrap();
    b.shutdown().unwrap();
}

// Spawn the same test binary to prove the directory and datagram link work
// across address spaces, without a broker or in-process endpoint registry.
struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn worker(root: &Path, role: &str) -> Worker {
    Worker(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "peer_process_worker", "--nocapture"])
            .env("PVISOR_PEER_TEST_ROOT", root)
            .env("PVISOR_PEER_TEST_ROLE", role)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    )
}

#[test]
fn separate_processes_exchange_frames_and_recover_after_crash() {
    let root = directory();
    let mut receiver = worker(root.path(), "receiver");
    wait_until(|| root.path().join("ready").exists());
    let mut sender = worker(root.path(), "sender");
    assert!(sender.0.wait().unwrap().success());
    assert!(receiver.0.wait().unwrap().success());
    let mut crashed = worker(root.path(), "crash");
    assert!(crashed.0.wait().unwrap().success());
    assert!(root.path().join("12/net.sock").exists());
    // The crashed owner's flock is released although its files remain.
    let (network, _stream, _) = start(root.path(), 12);
    network.shutdown().unwrap();
}

#[test]
fn peer_process_worker() {
    let Some(root) = std::env::var_os("PVISOR_PEER_TEST_ROOT") else {
        return;
    };
    let root = std::fs::canonicalize(root).unwrap();
    let role = std::env::var("PVISOR_PEER_TEST_ROLE").unwrap();
    let (network, mut stream, _) = start(&root, if role == "sender" { 11 } else { 12 });
    if role == "crash" {
        std::process::exit(0);
    }
    if role == "receiver" {
        std::fs::write(root.join("ready"), b"").unwrap();
        assert_eq!(receive(&mut stream), ip_frame(11, 12, IpProtocol::Tcp));
        send(&mut stream, &ip_frame(12, 11, IpProtocol::Tcp));
        wait_until(|| root.join("done").exists());
    } else {
        send(&mut stream, &ip_frame(11, 12, IpProtocol::Tcp));
        assert_eq!(receive(&mut stream), ip_frame(12, 11, IpProtocol::Tcp));
        std::fs::write(root.join("done"), b"").unwrap();
    }
    network.shutdown().unwrap();
}

// A pair of actual smoltcp guest stacks establishes TCP over the public framed
// VM links. This exercises real ARP resolution, TCP ACKs and byte integrity.
mod tcp_guests {
    use super::*;
    use smoltcp::iface::{Config, Interface, SocketSet};
    use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
    use smoltcp::socket::tcp;
    use smoltcp::time::Instant as SmolInstant;
    use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr};
    use std::collections::VecDeque;

    #[derive(Default)]
    struct Link {
        rx: VecDeque<Vec<u8>>,
        tx: VecDeque<Vec<u8>>,
    }
    struct Rx(Vec<u8>);
    struct Tx<'a>(&'a mut VecDeque<Vec<u8>>);
    impl RxToken for Rx {
        fn consume<R, F>(self, f: F) -> R
        where
            F: FnOnce(&[u8]) -> R,
        {
            f(&self.0)
        }
    }
    impl TxToken for Tx<'_> {
        fn consume<R, F>(self, length: usize, f: F) -> R
        where
            F: FnOnce(&mut [u8]) -> R,
        {
            let mut bytes = vec![0; length];
            let result = f(&mut bytes);
            self.0.push_back(bytes);
            result
        }
    }
    impl Device for Link {
        type RxToken<'a> = Rx;
        type TxToken<'a> = Tx<'a>;
        fn receive(&mut self, _: SmolInstant) -> Option<(Rx, Tx<'_>)> {
            self.rx
                .pop_front()
                .map(|frame| (Rx(frame), Tx(&mut self.tx)))
        }
        fn transmit(&mut self, _: SmolInstant) -> Option<Tx<'_>> {
            Some(Tx(&mut self.tx))
        }
        fn capabilities(&self) -> DeviceCapabilities {
            let mut caps = DeviceCapabilities::default();
            caps.medium = Medium::Ethernet;
            caps.max_transmission_unit = 1514;
            caps
        }
    }
    #[derive(Default)]
    struct Faults {
        enabled: bool,
        seen: usize,
        dropped: usize,
        duplicated: usize,
        reordered: usize,
        delayed: Option<Vec<u8>>,
    }
    struct Guest {
        stream: UnixStream,
        input: Vec<u8>,
        output: VecDeque<Vec<u8>>,
        output_offset: usize,
        link: Link,
        interface: Interface,
        sockets: SocketSet<'static>,
        handle: smoltcp::iface::SocketHandle,
        faults: Faults,
    }
    impl Guest {
        fn new(stream: UnixStream, last: u8) -> Self {
            let mut link = Link::default();
            let mut cfg = Config::new(HardwareAddress::Ethernet(EthernetAddress(guest_mac(
                address(last),
            ))));
            cfg.random_seed = u64::from(last);
            let mut interface = Interface::new(cfg, &mut link, SmolInstant::ZERO);
            interface.update_ip_addrs(|addrs| {
                addrs
                    .push(IpCidr::new(IpAddress::Ipv4(address(last)), 24))
                    .unwrap();
            });
            let mut sockets = SocketSet::new(vec![]);
            let handle = sockets.add(tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0; 8192]),
                tcp::SocketBuffer::new(vec![0; 8192]),
            ));
            Self {
                stream,
                input: vec![],
                output: VecDeque::new(),
                output_offset: 0,
                link,
                interface,
                sockets,
                handle,
                faults: Faults::default(),
            }
        }
        fn socket(&mut self) -> &mut tcp::Socket<'static> {
            self.sockets.get_mut(self.handle)
        }
        fn queue_frame(&mut self, frame: &[u8]) {
            let mut bytes = Vec::with_capacity(4 + frame.len());
            bytes.extend_from_slice(&(frame.len() as u32).to_be_bytes());
            bytes.extend_from_slice(frame);
            self.output.push_back(bytes);
        }
        fn poll(&mut self, now: SmolInstant) {
            self.stream.set_nonblocking(true).unwrap();
            let mut bytes = [0; 16384];
            loop {
                match self.stream.read(&mut bytes) {
                    Ok(0) => panic!("guest link closed"),
                    Ok(n) => self.input.extend_from_slice(&bytes[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => panic!("read guest link: {e}"),
                }
            }
            while self.input.len() >= 4 {
                let length = u32::from_be_bytes(self.input[..4].try_into().unwrap()) as usize;
                if self.input.len() < length + 4 {
                    break;
                }
                self.link.rx.push_back(self.input[4..4 + length].to_vec());
                self.input.drain(..4 + length);
            }
            self.interface.poll(now, &mut self.link, &mut self.sockets);
            let delayed = self.faults.delayed.take();
            let mut overtook_delayed = false;
            while let Some(frame) = self.link.tx.pop_front() {
                if self.faults.enabled {
                    self.faults.seen += 1;
                    if self.faults.seen.is_multiple_of(17) {
                        self.faults.dropped += 1;
                        continue;
                    }
                    if self.faults.seen.is_multiple_of(11) && self.faults.delayed.is_none() {
                        self.faults.delayed = Some(frame);
                        continue;
                    }
                    if self.faults.seen.is_multiple_of(13) {
                        self.queue_frame(&frame);
                        self.faults.duplicated += 1;
                    }
                }
                self.queue_frame(&frame);
                overtook_delayed = true;
            }
            // Release after newer output, or on the next poll if idle. This
            // cannot indefinitely withhold the last frame of a TCP exchange.
            if let Some(frame) = delayed {
                if overtook_delayed {
                    self.faults.reordered += 1;
                }
                self.queue_frame(&frame);
            }
            // Both simulated guests share this thread. Never block writes while
            // the peer needs its next poll to drain the opposite direction.
            while let Some(bytes) = self.output.front() {
                match self.stream.write(&bytes[self.output_offset..]) {
                    Ok(0) => panic!("guest link stopped accepting frames"),
                    Ok(count) => {
                        self.output_offset += count;
                        if self.output_offset == bytes.len() {
                            self.output.pop_front();
                            self.output_offset = 0;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => panic!("write guest link: {error}"),
                }
            }
        }
    }

    #[test]
    fn tcp_handshake_and_bidirectional_bytes_cross_peer_network() {
        let root = directory();
        let (a, sa, ma) = start(root.path(), 11);
        let (b, sb, mb) = start(root.path(), 12);
        let mut a_guest = Guest::new(sa, 11);
        let mut b_guest = Guest::new(sb, 12);
        b_guest.socket().listen(9000).unwrap();
        a_guest
            .sockets
            .get_mut::<tcp::Socket>(a_guest.handle)
            .connect(a_guest.interface.context(), (address(12), 9000), 49152)
            .unwrap();
        let payload: Vec<u8> = (0..262144).map(|n| (n * 37) as u8).collect();
        let mut sent = 0;
        let mut received = Vec::new();
        let mut response = Vec::new();
        let mut replied = false;
        let started = Instant::now();
        while response.len() < 3 {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "guest TCP exchange stalled"
            );
            let now = SmolInstant::from_micros(started.elapsed().as_micros() as i64);
            a_guest.poll(now);
            b_guest.poll(now);
            if a_guest.socket().can_send() && sent < payload.len() {
                sent += a_guest.socket().send_slice(&payload[sent..]).unwrap();
            }
            if b_guest.socket().can_recv() {
                b_guest
                    .socket()
                    .recv(|bytes| {
                        received.extend_from_slice(bytes);
                        (bytes.len(), ())
                    })
                    .unwrap();
            }
            if received.len() == payload.len() && !replied {
                assert_eq!(received, payload);
                b_guest.socket().send_slice(b"ACK").unwrap();
                replied = true;
            }
            if a_guest.socket().can_recv() {
                a_guest
                    .socket()
                    .recv(|bytes| {
                        response.extend_from_slice(bytes);
                        (bytes.len(), ())
                    })
                    .unwrap();
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(response, b"ACK");
        assert_eq!(ma.snapshot().tcp_flows_opened, 0);
        assert_eq!(mb.snapshot().tcp_flows_opened, 0);
        a.shutdown().unwrap();
        b.shutdown().unwrap();
    }

    #[test]
    fn tcp_duplex_recovers_from_loss_duplication_and_reordering() {
        let root = directory();
        let (a, sa, ma) = start(root.path(), 11);
        let (b, sb, mb) = start(root.path(), 12);
        let mut left = Guest::new(sa, 11);
        let mut right = Guest::new(sb, 12);
        left.faults.enabled = true;
        right.faults.enabled = true;
        right.socket().listen(9000).unwrap();
        left.sockets
            .get_mut::<tcp::Socket>(left.handle)
            .connect(left.interface.context(), (address(12), 9000), 49152)
            .unwrap();
        let forward: Vec<u8> = (0..1048576u32).map(|n| (n ^ (n >> 9)) as u8).collect();
        let reverse: Vec<u8> = (0..1048576u32)
            .map(|n| (n.wrapping_mul(37) ^ (n >> 7)) as u8)
            .collect();
        let (mut sent_left, mut sent_right) = (0, 0);
        let (mut received_left, mut received_right) = (Vec::new(), Vec::new());
        let started = Instant::now();
        while received_left.len() < reverse.len() || received_right.len() < forward.len() {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "TCP failed to recover: left={}/{}, right={}/{}",
                received_left.len(),
                reverse.len(),
                received_right.len(),
                forward.len()
            );
            // Accelerate retransmission time without depending on long sleeps.
            let now = SmolInstant::from_micros(started.elapsed().as_micros() as i64 * 4);
            left.poll(now);
            right.poll(now);
            for (guest, payload, sent, received) in [
                (&mut left, &forward, &mut sent_left, &mut received_left),
                (&mut right, &reverse, &mut sent_right, &mut received_right),
            ] {
                if guest.socket().can_send() && *sent < payload.len() {
                    *sent += guest.socket().send_slice(&payload[*sent..]).unwrap();
                }
                if guest.socket().can_recv() {
                    guest
                        .socket()
                        .recv(|bytes| {
                            received.extend_from_slice(bytes);
                            (bytes.len(), ())
                        })
                        .unwrap();
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(received_left, reverse);
        assert_eq!(received_right, forward);
        for guest in [&left, &right] {
            assert!(guest.faults.dropped > 0);
            assert!(guest.faults.duplicated > 0);
            assert!(guest.faults.reordered > 0);
        }
        assert_eq!(
            ma.snapshot().tcp_flows_opened + mb.snapshot().tcp_flows_opened,
            0
        );
        a.shutdown().unwrap();
        b.shutdown().unwrap();
    }
}

fn sequenced_frame(src: u8, dst: u8, length: usize, sequence: u64) -> Vec<u8> {
    let mut frame = ip_frame(src, dst, IpProtocol::Udp);
    frame.resize(length, 0);
    let mut ip = Ipv4Packet::new_unchecked(&mut frame[14..]);
    ip.set_total_len((length - 14) as u16);
    for (index, byte) in ip.payload_mut().iter_mut().enumerate() {
        *byte = ((index as u64 * 37) ^ sequence.rotate_left(7) ^ sequence) as u8;
    }
    ip.payload_mut()[..8].copy_from_slice(&sequence.to_be_bytes());
    ip.fill_checksum();
    frame
}

#[test]
fn seeded_fragmented_streams_forward_concurrent_ring_without_corruption() {
    let root = directory();
    let members: Vec<_> = (11..15).map(|last| start(root.path(), last)).collect();
    let mut owners = Vec::new();
    let mut workers = Vec::new();
    for (index, (owner, mut stream, metrics)) in members.into_iter().enumerate() {
        owners.push((owner, metrics));
        workers.push(std::thread::spawn(move || {
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let source = 11 + index as u8;
            let destination = 11 + (index as u8 + 1) % 4;
            let previous = 11 + (index as u8 + 3) % 4;
            let mut seed = 0x504e_4554_u64 + index as u64;
            for batch in 0..256 {
                for offset in 0..4 {
                    let sequence = batch * 4 + offset;
                    let length = if sequence % 2 == 0 { 1514 } else { 66 };
                    let packet = sequenced_frame(source, destination, length, sequence);
                    let mut framed = (length as u32).to_be_bytes().to_vec();
                    framed.extend(packet);
                    let mut position = 0;
                    while position < framed.len() {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        let end = (position + 1 + seed as usize % 73).min(framed.len());
                        stream.write_all(&framed[position..end]).unwrap();
                        position = end;
                    }
                }
                for offset in 0..4 {
                    let sequence = batch * 4 + offset;
                    let length = if sequence % 2 == 0 { 1514 } else { 66 };
                    assert_eq!(
                        receive(&mut stream),
                        sequenced_frame(previous, source, length, sequence)
                    );
                }
            }
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    for (owner, metrics) in owners {
        let result = owner.shutdown().unwrap();
        assert_eq!(result.peer_packets_sent, 1024);
        assert_eq!(result.peer_packets_received, 1024);
        assert_eq!(result.peer_packets_dropped, 0);
        assert_eq!(metrics.snapshot().tcp_flows_opened, 0);
    }
}

#[test]
fn repeated_restart_recovers_both_directions_and_reclaims_sender_sockets() {
    let root = directory();
    let (a, mut sa, ma) = start(root.path(), 11);
    let (c, mut sc, _) = start(root.path(), 13);
    for cycle in 0..12 {
        let (b, mut sb, _) = start(root.path(), 12);
        // Previous positive/negative entries and generation may still be cached.
        std::thread::sleep(Duration::from_millis(275));
        let packet = sequenced_frame(11, 12, 1514, cycle);
        send(&mut sa, &packet);
        assert_eq!(receive(&mut sb), packet);
        let reply = sequenced_frame(12, 11, 66, cycle);
        send(&mut sb, &reply);
        assert_eq!(receive(&mut sa), reply);
        b.shutdown().unwrap();
        // No listener: these must drop, without blocking another destination.
        for _ in 0..32 {
            send(&mut sa, &packet);
        }
        let healthy = sequenced_frame(11, 13, 1514, cycle);
        send(&mut sa, &healthy);
        assert_eq!(receive(&mut sc), healthy);
        let remaining: Vec<_> = std::fs::read_dir(root.path().join("12"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(remaining, [std::ffi::OsString::from("lease.lock")]);
    }
    assert!(ma.snapshot().peer_packets_dropped >= 12 * 32);
    a.shutdown().unwrap();
    c.shutdown().unwrap();
    for slot in ["11", "12", "13"] {
        assert_eq!(
            std::fs::read_dir(root.path().join(slot)).unwrap().count(),
            1
        );
    }
}

#[test]
fn a_stalled_guest_drops_packets_without_blocking_other_peers() {
    let root = directory();
    let (a, mut sa, ma) = start(root.path(), 11);
    let (b, _blocked_guest, _) = start(root.path(), 12);
    let (c, mut sc, mc) = start(root.path(), 13);
    let packet = ip_frame(11, 12, IpProtocol::Udp);
    for _ in 0..4096 {
        send(&mut sa, &packet);
    }
    wait_until(|| ma.snapshot().peer_packets_dropped > 0);
    let healthy = ip_frame(11, 13, IpProtocol::Udp);
    send(&mut sa, &healthy);
    let mut length = [0; 4];
    let result = sc.read_exact(&mut length);
    assert!(
        result.is_ok(),
        "healthy peer blocked: {result:?}; A={:?}; C={:?}",
        ma.snapshot(),
        mc.snapshot()
    );
    let mut arrived = vec![0; u32::from_be_bytes(length) as usize];
    sc.read_exact(&mut arrived).unwrap();
    assert_eq!(arrived, healthy);
    a.shutdown().unwrap();
    b.shutdown().unwrap();
    c.shutdown().unwrap();
}

#[test]
fn saturated_receiver_resumes_without_corruption_and_can_reply() {
    let root = directory();
    let (a, mut sa, ma) = start(root.path(), 11);
    let (b, mut sb, mb) = start(root.path(), 12);
    const OFFERED: u64 = 4096;
    for sequence in 0..OFFERED {
        send(&mut sa, &sequenced_frame(11, 12, 1514, sequence));
    }
    wait_until(|| {
        let snapshot = ma.snapshot();
        snapshot.peer_packets_sent + snapshot.peer_packets_dropped == OFFERED
    });
    let accepted = ma.snapshot().peer_packets_sent;
    assert!(accepted > 0 && accepted < OFFERED);
    let mut previous = None;
    for _ in 0..accepted {
        let arrived = receive(&mut sb);
        assert_eq!(arrived.len(), 1514);
        let sequence = u64::from_be_bytes(arrived[34..42].try_into().unwrap());
        assert!(sequence < OFFERED);
        assert!(
            previous.is_none_or(|last| sequence > last),
            "duplicate/reordered datagram"
        );
        assert_eq!(arrived, sequenced_frame(11, 12, 1514, sequence));
        previous = Some(sequence);
    }
    wait_until(|| mb.snapshot().peer_packets_received == accepted);
    let marker = sequenced_frame(11, 12, 66, OFFERED);
    send(&mut sa, &marker);
    assert_eq!(receive(&mut sb), marker);
    let reply = sequenced_frame(12, 11, 1514, OFFERED);
    send(&mut sb, &reply);
    assert_eq!(receive(&mut sa), reply);
    a.shutdown().unwrap();
    b.shutdown().unwrap();
}

#[test]
fn seeded_malformed_datagram_flood_does_not_poison_the_next_packet() {
    use std::os::unix::net::UnixDatagram;
    let root = directory();
    let (a, _sa, _) = start(root.path(), 11);
    let (b, mut sb, mb) = start(root.path(), 12);
    let tx = UnixDatagram::bind(root.path().join("11/tx-12.sock")).unwrap();
    tx.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
    let inject = |bytes: &[u8]| {
        // Unlike Linux's blocking datagram send, Darwin can return ENOBUFS
        // immediately. This driver must inject every rejected test packet;
        // runtime packet forwarding retains its separate best-effort contract.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match tx.send_to(bytes, root.path().join("12/net.sock")) {
                Ok(length) => {
                    assert_eq!(length, bytes.len());
                    break;
                }
                Err(error)
                    if (matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) || error.raw_os_error() == Some(libc::ENOBUFS))
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("raw peer datagram injection failed: {error}"),
            }
        }
    };
    let mut seed = 0x504e_4554_u64;
    for index in 0..1024 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let mut bytes = vec![0; seed as usize % 2048];
        for byte in &mut bytes {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            *byte = (seed >> 32) as u8;
        }
        // Deterministically invalid magic, including zero-length/truncated and
        // oversized datagrams. No random accidental valid packet is allowed.
        if !bytes.is_empty() {
            bytes[0] = 0;
        }
        inject(&bytes);
        if index % 128 == 127 {
            wait_until(|| mb.snapshot().peer_packets_dropped == index + 1);
        }
    }
    let generation = |slot: &str| -> Vec<u8> {
        let record: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.path().join(slot).join("peer.json")).unwrap(),
        )
        .unwrap();
        serde_json::from_value(record["generation"].clone()).unwrap()
    };
    let frame = sequenced_frame(11, 12, 1514, 1024);
    let mut valid = b"PVN1".to_vec();
    valid.extend(generation("11"));
    valid.extend(generation("12"));
    valid.extend_from_slice(&frame);
    inject(&valid);
    assert_eq!(receive(&mut sb), frame);
    assert_eq!(mb.snapshot().peer_packets_received, 1);
    assert_eq!(mb.snapshot().peer_packets_dropped, 1024);
    a.shutdown().unwrap();
    b.shutdown().unwrap();
}

#[test]
fn continuous_peer_flood_does_not_starve_router_dns_or_shutdown() {
    use std::os::unix::net::UnixDatagram;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let root = directory();
    let (network, mut stream, metrics) = start(root.path(), 11);
    let tx = UnixDatagram::bind(root.path().join("flood.sock")).unwrap();
    tx.connect(root.path().join("11/net.sock")).unwrap();
    tx.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    let flood = std::thread::spawn(move || {
        let started = Instant::now();
        while !signal.load(Ordering::Relaxed) && started.elapsed() < Duration::from_secs(5) {
            match tx.send(b"invalid peer datagram") {
                Ok(_) => {}
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(libc::ENOBUFS) =>
                {
                    std::thread::yield_now()
                }
                Err(_) => break,
            }
        }
    });
    wait_until(|| metrics.snapshot().peer_packets_dropped > 100);
    let reply = resolve_name(&mut stream, address(11), address(1), "external.invalid");
    assert_eq!(&reply[6..8], &[0, 1]);
    network.shutdown().unwrap();
    stop.store(true, Ordering::Relaxed);
    flood.join().unwrap();
    assert_eq!(metrics.snapshot().dns_answers, 1);
}

#[test]
fn receiver_rejects_forged_generations_pathnames_and_oversized_datagrams() {
    let root = directory();
    let (a, _sa, _) = start(root.path(), 11);
    let (b, mut sb, mb) = start(root.path(), 12);
    let generation = |slot: &str| -> [u8; 16] {
        let record: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.path().join(slot).join("peer.json")).unwrap(),
        )
        .unwrap();
        serde_json::from_value(record["generation"].clone()).unwrap()
    };
    let mut good = b"PVN1".to_vec();
    good.extend(generation("11"));
    good.extend(generation("12"));
    good.extend(ip_frame(11, 12, IpProtocol::Udp));
    let tx = std::os::unix::net::UnixDatagram::bind(root.path().join("11/tx-12.sock")).unwrap();
    let send_raw = |bytes: &[u8]| {
        tx.send_to(bytes, root.path().join("12/net.sock")).unwrap();
    };
    let mut bad_source = good.clone();
    bad_source[4] ^= 1;
    send_raw(&bad_source);
    let mut bad_destination = good.clone();
    bad_destination[20] ^= 1;
    send_raw(&bad_destination);
    let mut bad_version = good.clone();
    bad_version[3] ^= 1;
    send_raw(&bad_version);
    let mut oversized = good.clone();
    oversized.resize(36 + 1515, 0);
    send_raw(&oversized);
    let outsider =
        std::os::unix::net::UnixDatagram::bind(root.path().join("outsider.sock")).unwrap();
    outsider
        .send_to(&good, root.path().join("12/net.sock"))
        .unwrap();
    wait_until(|| mb.snapshot().peer_packets_dropped == 5);
    send_raw(&good);
    assert_eq!(receive(&mut sb), ip_frame(11, 12, IpProtocol::Udp));
    assert_eq!(mb.snapshot().peer_packets_received, 1);
    a.shutdown().unwrap();
    b.shutdown().unwrap();
}

#[test]
fn peer_membership_requires_the_default_controller_constructor() {
    let root = directory();
    let mut cfg = config(root.path(), 11);
    cfg.egress = EgressRuntime::with_bandwidth_registry(
        NetworkPolicy::compile(&NetworkConfig::default()).unwrap(),
        std::sync::Arc::new(pvisor_core::PolicyControlController),
        BandwidthRegistry::default(),
    );
    assert!(
        VmNetwork::start(cfg)
            .err()
            .unwrap()
            .to_string()
            .contains("for_peer_network")
    );
}

#[test]
fn peer_guest_address_keeps_the_existing_router_and_dns_path() {
    use smoltcp::wire::UdpPacket;
    let root = directory();
    let (network, mut stream, metrics) = start(root.path(), 11);
    // Learn the router and populate its neighbor cache for the configured guest.
    send(&mut stream, &arp(11, 1, false));
    let reply = receive(&mut stream);
    assert_eq!(
        EthernetFrame::new_checked(&reply).unwrap().ethertype(),
        EthernetProtocol::Arp
    );
    let router_mac = [0x02, 0x50, 0x56, 0, 0, 1];
    let query =
        b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x04demo\x07invalid\x00\x00\x01\x00\x01";
    let ip_length = 20 + 8 + query.len();
    let mut frame = vec![0; 14 + ip_length];
    let mut ethernet = EthernetFrame::new_unchecked(frame.as_mut_slice());
    ethernet.set_src_addr(EthernetAddress(guest_mac(address(11))));
    ethernet.set_dst_addr(EthernetAddress(router_mac));
    ethernet.set_ethertype(EthernetProtocol::Ipv4);
    let mut ip = Ipv4Packet::new_unchecked(ethernet.payload_mut());
    ip.set_version(4);
    ip.set_header_len(20);
    ip.set_total_len(ip_length as u16);
    ip.set_hop_limit(64);
    ip.set_next_header(IpProtocol::Udp);
    ip.set_src_addr(address(11));
    ip.set_dst_addr(address(1));
    ip.fill_checksum();
    let mut udp = UdpPacket::new_unchecked(ip.payload_mut());
    udp.set_src_port(40000);
    udp.set_dst_port(53);
    udp.set_len((8 + query.len()) as u16);
    udp.payload_mut().copy_from_slice(query);
    udp.fill_checksum(&address(11).into(), &address(1).into());
    send(&mut stream, &frame);
    let reply = receive(&mut stream);
    let ethernet = EthernetFrame::new_checked(&reply).unwrap();
    let ip = Ipv4Packet::new_checked(ethernet.payload()).unwrap();
    assert_eq!(ip.dst_addr(), address(11));
    let udp = UdpPacket::new_checked(ip.payload()).unwrap();
    assert_eq!(&udp.payload()[..2], &[0x12, 0x34]);
    assert_eq!(&udp.payload()[6..8], &[0, 1]);
    assert_eq!(&udp.payload()[udp.payload().len() - 4..], &[198, 18, 0, 1]);
    assert_eq!(metrics.snapshot().dns_answers, 1);
    assert_eq!(metrics.snapshot().peer_packets_sent, 0);
    network.shutdown().unwrap();
}

#[test]
fn named_network_uses_its_subnet_gateway_and_live_container_dns() {
    let root = directory();
    let gateway: Ipv4Addr = "172.23.0.254".parse().unwrap();
    let address_a: Ipv4Addr = "172.23.0.11".parse().unwrap();
    let address_b: Ipv4Addr = "172.23.0.1".parse().unwrap();
    let named_config = |ip, name: &str| {
        let mut cfg = config(root.path(), 11);
        let peer = cfg.peer_network.as_mut().unwrap();
        peer.address = ip;
        peer.gateway = gateway;
        peer.aliases = vec![name.into()];
        cfg
    };
    let (a, mut sa) = VmNetwork::start(named_config(address_a, "client")).unwrap();
    let (b, mut sb) = VmNetwork::start(named_config(address_b, "web")).unwrap();
    sa.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    sb.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let packet = ip_frame_at(address_a, address_b, IpProtocol::Udp);
    send(&mut sa, &packet);
    assert_eq!(receive(&mut sb), packet);
    let answer = resolve_name(&mut sa, address_a, gateway, "web");
    assert_eq!(&answer[6..8], &[0, 1]);
    assert_eq!(&answer[answer.len() - 4..], &address_b.octets());
    b.shutdown().unwrap();
    let absent = resolve_name(&mut sa, address_a, gateway, "web");
    assert_eq!(absent[3] & 0xf, 3);
    let external = resolve_name(&mut sa, address_a, gateway, "example.invalid");
    assert_eq!(&external[external.len() - 4..], &[198, 18, 0, 1]);
    a.shutdown().unwrap();
}

fn ip_frame_at(src: Ipv4Addr, dst: Ipv4Addr, protocol: IpProtocol) -> Vec<u8> {
    let mut bytes = ip_frame(11, 12, protocol);
    let mut frame = EthernetFrame::new_unchecked(bytes.as_mut_slice());
    frame.set_src_addr(EthernetAddress(guest_mac(src)));
    frame.set_dst_addr(EthernetAddress(guest_mac(dst)));
    let mut ip = Ipv4Packet::new_unchecked(frame.payload_mut());
    ip.set_src_addr(src);
    ip.set_dst_addr(dst);
    ip.fill_checksum();
    bytes
}

fn resolve_name(stream: &mut UnixStream, guest: Ipv4Addr, router: Ipv4Addr, name: &str) -> Vec<u8> {
    use smoltcp::wire::UdpPacket;
    let mut arp = arp(11, 1, false);
    arp[6..12].copy_from_slice(&guest_mac(guest));
    arp[22..28].copy_from_slice(&guest_mac(guest));
    arp[28..32].copy_from_slice(&guest.octets());
    arp[38..42].copy_from_slice(&router.octets());
    send(stream, &arp);
    let reply = receive(stream);
    assert_eq!(
        EthernetFrame::new_checked(&reply).unwrap().ethertype(),
        EthernetProtocol::Arp
    );
    let mut query = vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.') {
        query.push(label.len() as u8);
        query.extend(label.as_bytes());
    }
    query.extend([0, 0, 1, 0, 1]);
    let length = 20 + 8 + query.len();
    let mut bytes = vec![0; 14 + length];
    let mut frame = EthernetFrame::new_unchecked(bytes.as_mut_slice());
    frame.set_src_addr(EthernetAddress(guest_mac(guest)));
    frame.set_dst_addr(EthernetAddress([2, 0x50, 0x56, 0, 0, 1]));
    frame.set_ethertype(EthernetProtocol::Ipv4);
    let mut ip = Ipv4Packet::new_unchecked(frame.payload_mut());
    ip.set_version(4);
    ip.set_header_len(20);
    ip.set_total_len(length as u16);
    ip.set_hop_limit(64);
    ip.set_next_header(IpProtocol::Udp);
    ip.set_src_addr(guest);
    ip.set_dst_addr(router);
    ip.fill_checksum();
    let mut udp = UdpPacket::new_unchecked(ip.payload_mut());
    udp.set_src_port(40000);
    udp.set_dst_port(53);
    udp.set_len((8 + query.len()) as u16);
    udp.payload_mut().copy_from_slice(&query);
    udp.fill_checksum(&guest.into(), &router.into());
    send(stream, &bytes);
    let reply = receive(stream);
    let frame = EthernetFrame::new_checked(&reply).unwrap();
    let ip = Ipv4Packet::new_checked(frame.payload()).unwrap();
    assert_eq!(ip.dst_addr(), guest);
    UdpPacket::new_checked(ip.payload())
        .unwrap()
        .payload()
        .to_vec()
}
