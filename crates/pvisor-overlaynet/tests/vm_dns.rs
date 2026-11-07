//! Bounded hostile DNS trace through the public length-prefixed guest link.
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use pvisor_overlaynet::vm::{GUEST_IPV4, ROUTER_IPV4, VM_MAC, VmNetwork, VmNetworkConfig};
use pvisor_overlaynet::{
    BandwidthRegistry, EgressContext, EgressRuntime, NetworkConfig, NetworkMode, NetworkPolicy,
};
use smoltcp::wire::{EthernetFrame, IpProtocol, Ipv4Packet, TcpPacket, UdpPacket};

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
    assert!((14..=1514).contains(&length));
    let mut frame = vec![0; length];
    stream.read_exact(&mut frame).unwrap();
    frame
}

fn query(labels: &[usize]) -> Vec<u8> {
    let mut query = vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for &length in labels {
        query.push(length as u8);
        query.extend(std::iter::repeat_n(b'a', length));
    }
    query.extend_from_slice(&[0, 0, 1, 0, 1]);
    query
}

fn dns_frame(query: &[u8]) -> Vec<u8> {
    let mut frame = vec![0; 14 + 20 + 8 + query.len()];
    frame[..6].copy_from_slice(&[2, 0x50, 0x56, 0, 0, 1]);
    frame[6..12].copy_from_slice(&VM_MAC);
    frame[12..14].copy_from_slice(&[8, 0]);
    let mut ip = Ipv4Packet::new_unchecked(&mut frame[14..]);
    ip.set_version(4);
    ip.set_header_len(20);
    ip.set_total_len((20 + 8 + query.len()) as u16);
    ip.set_hop_limit(64);
    ip.set_next_header(smoltcp::wire::IpProtocol::Udp);
    ip.set_src_addr(GUEST_IPV4);
    ip.set_dst_addr(ROUTER_IPV4);
    ip.fill_checksum();
    let mut udp = UdpPacket::new_unchecked(ip.payload_mut());
    udp.set_src_port(40000);
    udp.set_dst_port(53);
    udp.set_len((8 + query.len()) as u16);
    udp.payload_mut().copy_from_slice(query);
    udp.fill_checksum(&GUEST_IPV4.into(), &ROUTER_IPV4.into());
    frame
}

fn exchange(stream: &mut UnixStream, query: &[u8], rcode: u8, answers: u8) -> Vec<u8> {
    send(stream, &dns_frame(query));
    let frame = receive(stream);
    let ethernet = EthernetFrame::new_checked(&frame).unwrap();
    let ip = Ipv4Packet::new_checked(ethernet.payload()).unwrap();
    assert_eq!(ip.src_addr(), ROUTER_IPV4);
    assert_eq!(ip.dst_addr(), GUEST_IPV4);
    assert!(ip.verify_checksum());
    let udp = UdpPacket::new_checked(ip.payload()).unwrap();
    assert_eq!((udp.src_port(), udp.dst_port()), (53, 40000));
    assert!(udp.verify_checksum(&ROUTER_IPV4.into(), &GUEST_IPV4.into()));
    let reply = udp.payload();
    assert_eq!(&reply[..2], query.get(..2).unwrap_or(&[0, 0]));
    assert_eq!(reply[2] & 0x80, 0x80);
    assert_eq!(reply[3] & 0x0f, rcode, "query={query:02x?}");
    assert_eq!(&reply[6..8], &[0, answers]);
    reply.to_vec()
}

#[test]
fn malformed_dns_and_wire_name_limit_do_not_poison_later_queries() {
    let policy = NetworkPolicy::compile(&NetworkConfig {
        mode: NetworkMode::NoNetwork,
        ..Default::default()
    })
    .unwrap();
    let egress = EgressRuntime::with_bandwidth_registry(
        policy,
        Arc::new(pvisor_core::PolicyControlController),
        BandwidthRegistry::default(),
    );
    let (network, mut stream) =
        VmNetwork::start(VmNetworkConfig::new(egress, EgressContext::default())).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    // ARP primes the real stack's neighbor cache; no host networking is used.
    let mut arp = vec![0; 42];
    arp[..6].fill(255);
    arp[6..12].copy_from_slice(&VM_MAC);
    arp[12..22].copy_from_slice(&[8, 6, 0, 1, 8, 0, 6, 4, 0, 1]);
    arp[22..28].copy_from_slice(&VM_MAC);
    arp[28..32].copy_from_slice(&GUEST_IPV4.octets());
    arp[38..42].copy_from_slice(&ROUTER_IPV4.octets());
    send(&mut stream, &arp);
    assert_eq!(&receive(&mut stream)[12..14], &[8, 6]);

    let good = query(&[4, 7]);
    let stable = exchange(&mut stream, &good, 0, 1);
    // Truncate the header, first label and question tail. Including responses,
    // this entire trace stays below 128 small Ethernet frames.
    let mut malformed: Vec<Vec<u8>> = (0..14)
        .chain(24..good.len())
        .map(|end| good[..end].to_vec())
        .collect();
    malformed.push(query(&[64])); // Label limit is 63 octets.
    let mut pointer = good.clone();
    pointer[12..14].copy_from_slice(&[0xc0, 0x0c]); // Unsupported/self-referential compression.
    malformed.push(pointer);
    // RFC 1035 §2.3.4: the 255-octet name limit includes the root terminator.
    malformed.push(query(&[63, 63, 63, 62])); // 256 wire octets, not 255.
    malformed.push(query(&[63, 63, 63, 63]));
    for bad in &malformed {
        exchange(&mut stream, bad, 1, 0);
        assert_eq!(exchange(&mut stream, &good, 0, 1), stable);
    }
    // 254 and 255 wire octets remain supported. Sequential allocation also
    // proves the rejected queries did not consume synthetic address slots.
    for (last, slot) in [(60, 2), (61, 3)] {
        let reply = exchange(&mut stream, &query(&[63, 63, 63, last]), 0, 1);
        assert_eq!(&reply[reply.len() - 4..], &[198, 18, 0, slot]);
    }
    assert_eq!(exchange(&mut stream, &good, 0, 1), stable);
    let metrics = network.shutdown().unwrap();
    assert_eq!(metrics.dns_queries, 2 * malformed.len() as u64 + 4);
    assert_eq!(metrics.dns_answers, metrics.dns_queries);
    assert_eq!(metrics.tcp_flows_opened, 0);
}

#[test]
fn rejected_syn_headers_do_not_consume_the_only_flow_slot() {
    let policy = NetworkPolicy::compile(&NetworkConfig {
        mode: NetworkMode::NoNetwork,
        ..Default::default()
    })
    .unwrap();
    let egress = EgressRuntime::with_bandwidth_registry(
        policy,
        Arc::new(pvisor_core::PolicyControlController),
        BandwidthRegistry::default(),
    );
    let mut config = VmNetworkConfig::new(egress, EgressContext::default());
    config.max_flows = 1;
    let (network, mut stream) = VmNetwork::start(config).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut arp = vec![0; 42];
    arp[..6].fill(255);
    arp[6..12].copy_from_slice(&VM_MAC);
    arp[12..22].copy_from_slice(&[8, 6, 0, 1, 8, 0, 6, 4, 0, 1]);
    arp[22..28].copy_from_slice(&VM_MAC);
    arp[28..32].copy_from_slice(&GUEST_IPV4.octets());
    arp[38..42].copy_from_slice(&ROUTER_IPV4.octets());
    send(&mut stream, &arp);
    assert_eq!(&receive(&mut stream)[12..14], &[8, 6]);

    // A complete TCP header with four bytes of options. Checksums remain valid
    // so the test reaches semantic validation, not just checksum rejection.
    let syn = |options: [u8; 4], port: u16, destination: std::net::Ipv4Addr| {
        let mut frame = dns_frame(&[0; 16]);
        let mut ip = Ipv4Packet::new_unchecked(&mut frame[14..]);
        ip.set_next_header(IpProtocol::Tcp);
        ip.set_dst_addr(destination);
        ip.fill_checksum();
        let mut tcp = TcpPacket::new_unchecked(ip.payload_mut());
        tcp.set_src_port(port);
        tcp.set_dst_port(53);
        tcp.set_header_len(24);
        tcp.set_syn(true);
        tcp.set_seq_number(smoltcp::wire::TcpSeqNumber(40));
        tcp.set_window_len(1024);
        tcp.options_mut().copy_from_slice(&options);
        tcp.fill_checksum(&GUEST_IPV4.into(), &destination.into());
        frame
    };
    let good = syn([2, 4, 5, 180], 41000, ROUTER_IPV4);
    let bad_destination = std::net::Ipv4Addr::new(198, 18, 0, 99);
    let bad_base = syn([2, 4, 5, 180], 40000, bad_destination);
    // Zero/one/incorrect option lengths, option data past the header boundary,
    // and a final option kind whose length byte is missing.
    let mut rejected: Vec<_> = [
        [2, 0, 0, 0],
        [2, 1, 0, 0],
        [2, 3, 0, 0],
        [2, 5, 0, 0],
        [3, 4, 0, 0],
        [8, 10, 0, 0],
        [1, 1, 1, 2],
    ]
    .into_iter()
    .map(|options| syn(options, 40000, bad_destination))
    .collect();
    let mut wrong_version = bad_base.clone();
    let mut ip = Ipv4Packet::new_unchecked(&mut wrong_version[14..]);
    ip.set_version(6);
    ip.fill_checksum();
    rejected.push(wrong_version);
    // An incomplete datagram, overlapping first fragments, and a final fragment
    // whose bytes resemble a SYN must not allocate flow state.
    for (more, offset) in [(true, 0), (true, 8), (false, 8)] {
        let mut fragment = bad_base.clone();
        let mut ip = Ipv4Packet::new_unchecked(&mut fragment[14..]);
        ip.set_more_frags(more);
        ip.set_frag_offset(offset);
        ip.fill_checksum();
        rejected.push(fragment);
    }
    for end in [14, 33, 34, good.len() - 1] {
        rejected.push(good[..end].to_vec());
    }
    assert_eq!(rejected.len(), 15);
    let query = query(&[4, 7]);
    let stable = exchange(&mut stream, &query, 0, 1);
    for frame in &rejected {
        send(&mut stream, frame);
        // Ordered UDP request/reply is a backend progress barrier, and confirms
        // recovery of an unrelated flow without sleeps or waiting for silence.
        assert_eq!(exchange(&mut stream, &query, 0, 1), stable);
    }
    send(&mut stream, &good);
    let reply = receive(&mut stream);
    let ip = Ipv4Packet::new_checked(&reply[14..]).unwrap();
    let tcp = TcpPacket::new_checked(ip.payload()).unwrap();
    assert_eq!((tcp.src_port(), tcp.dst_port()), (53, 41000));
    assert!(tcp.syn() && tcp.ack() && !tcp.rst());
    assert!(tcp.verify_checksum(&ROUTER_IPV4.into(), &GUEST_IPV4.into()));
    let metrics = network.shutdown().unwrap();
    assert_eq!(metrics.dns_queries, 16);
    assert_eq!(metrics.dns_answers, 16);
    assert_eq!(metrics.tcp_flows_opened, 1);
    assert_eq!(metrics.tcp_flows_denied, 0);
    assert_eq!(metrics.tcp_connect_failures, 0);
}
