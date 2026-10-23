//! Worker for B-PEER-NET-ENG; see benchmark/peer/README.md for interpretation.
use std::io::{Read, Write};
use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use pvisor_overlaynet::vm::{VmNetwork, VmNetworkConfig, guest_mac};
use pvisor_overlaynet::{
    BandwidthRegistry, EgressContext, EgressRuntime, NetworkConfig, NetworkPolicy,
    PeerNetworkConfig,
};
use smoltcp::wire::{EthernetAddress, EthernetFrame, EthernetProtocol, IpProtocol, Ipv4Packet};

const PINGS: usize = 2000;
const BULK: usize = 16384;
const WINDOW: usize = 8;
const WARMUP: usize = 128;

fn ip(last: u8) -> Ipv4Addr {
    Ipv4Addr::new(192, 0, 2, last)
}

fn frame(src: u8, dst: u8, length: usize, sequence: usize) -> Vec<u8> {
    let mut bytes = vec![0; length];
    let mut ethernet = EthernetFrame::new_unchecked(bytes.as_mut_slice());
    ethernet.set_src_addr(EthernetAddress(guest_mac(ip(src))));
    ethernet.set_dst_addr(EthernetAddress(guest_mac(ip(dst))));
    ethernet.set_ethertype(EthernetProtocol::Ipv4);
    let mut packet = Ipv4Packet::new_unchecked(ethernet.payload_mut());
    packet.set_version(4);
    packet.set_header_len(20);
    packet.set_total_len((length - 14) as u16);
    packet.set_hop_limit(64);
    packet.set_next_header(IpProtocol::Udp);
    packet.set_src_addr(ip(src));
    packet.set_dst_addr(ip(dst));
    for (index, byte) in packet.payload_mut().iter_mut().enumerate() {
        *byte = (index.wrapping_mul(37) ^ sequence.wrapping_mul(13)) as u8;
    }
    packet.payload_mut()[..8].copy_from_slice(&(sequence as u64).to_be_bytes());
    packet.fill_checksum();
    bytes
}

fn send(stream: &mut UnixStream, frame: &[u8]) {
    let mut bytes = Vec::with_capacity(frame.len() + 4);
    bytes.extend_from_slice(&(frame.len() as u32).to_be_bytes());
    bytes.extend_from_slice(frame);
    stream.write_all(&bytes).unwrap();
}

fn receive(stream: &mut UnixStream) -> Vec<u8> {
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix).unwrap();
    let length = u32::from_be_bytes(prefix) as usize;
    assert!((14..=1514).contains(&length));
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    bytes
}

fn cpu_seconds() -> f64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    let usage = unsafe { usage.assume_init() };
    let seconds = |time: libc::timeval| time.tv_sec as f64 + time.tv_usec as f64 / 1e6;
    seconds(usage.ru_utime) + seconds(usage.ru_stime)
}

fn main() {
    let directory = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let start = |last| {
        let policy = NetworkPolicy::compile(&NetworkConfig::default()).unwrap();
        let egress = EgressRuntime::for_peer_network(policy, BandwidthRegistry::default()).unwrap();
        let mut config = VmNetworkConfig::new(egress, EgressContext::default());
        config.peer_network = Some(PeerNetworkConfig {
            directory: directory.path().to_path_buf(),
            address: ip(last),
            gateway: ip(1),
            aliases: vec![],
        });
        let (owner, stream) = VmNetwork::start(config).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        (owner, stream)
    };
    let (a, mut sa) = start(11);
    let (b, mut sb) = start(12);
    let receiver = std::thread::spawn(move || {
        for sequence in 0..WARMUP + PINGS {
            assert_eq!(receive(&mut sb), frame(11, 12, 64, sequence));
            send(&mut sb, &frame(12, 11, 64, sequence));
        }
        for sequence in 0..BULK {
            assert_eq!(receive(&mut sb), frame(11, 12, 1514, sequence));
            if (sequence + 1) % WINDOW == 0 {
                send(&mut sb, &frame(12, 11, 64, sequence));
            }
        }
    });
    let mut rtt_us = Vec::with_capacity(PINGS);
    for sequence in 0..WARMUP + PINGS {
        let packet = frame(11, 12, 64, sequence);
        let expected = frame(12, 11, 64, sequence);
        let started = Instant::now();
        send(&mut sa, &packet);
        assert_eq!(receive(&mut sa), expected);
        if sequence >= WARMUP {
            rtt_us.push(started.elapsed().as_secs_f64() * 1e6);
        }
    }
    let cpu = cpu_seconds();
    let started = Instant::now();
    for sequence in 0..BULK {
        send(&mut sa, &frame(11, 12, 1514, sequence));
        if (sequence + 1) % WINDOW == 0 {
            assert_eq!(receive(&mut sa), frame(12, 11, 64, sequence));
        }
    }
    let wall = started.elapsed().as_secs_f64();
    let cpu = cpu_seconds() - cpu;
    receiver.join().unwrap();
    let ma = a.shutdown().unwrap();
    let mb = b.shutdown().unwrap();
    assert_eq!(ma.peer_packets_dropped + mb.peer_packets_dropped, 0);
    assert_eq!(ma.peer_packets_sent, (WARMUP + PINGS + BULK) as u64);
    assert_eq!(mb.peer_packets_received, ma.peer_packets_sent);
    assert_eq!(
        mb.peer_packets_sent,
        (WARMUP + PINGS + BULK / WINDOW) as u64
    );
    assert_eq!(ma.peer_packets_received, mb.peer_packets_sent);
    let mut sorted = rtt_us.clone();
    sorted.sort_by(f64::total_cmp);
    println!(
        "{}",
        serde_json::json!({
            "benchmark": "B-PEER-NET-ENG", "frames": BULK, "frame_bytes": 1514,
            "window": WINDOW, "pings": PINGS, "bulk_wall_seconds": wall,
            "bulk_cpu_seconds": cpu, "goodput_mbps": BULK as f64 * 1514.0 * 8.0 / wall / 1e6,
            "cpu_ns_per_frame": cpu * 1e9 / BULK as f64,
            "rtt_median_us": sorted[PINGS / 2], "rtt_p95_us": sorted[PINGS * 95 / 100],
            "rtt_us": rtt_us, "drops": 0,
            "a_sent": ma.peer_packets_sent, "b_received": mb.peer_packets_received,
            "b_sent": mb.peer_packets_sent, "a_received": ma.peer_packets_received,
        })
    );
}
