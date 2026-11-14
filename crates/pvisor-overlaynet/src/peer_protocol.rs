//! Shared membership contract and Ethernet validation for host packet links.
use crate::vm::guest_mac;
use smoltcp::wire::{
    ArpOperation, ArpPacket, EthernetFrame, EthernetProtocol, IpProtocol, Ipv4Packet,
};
use std::{net::Ipv4Addr, path::PathBuf};

pub(crate) const FRAME_LIMIT: usize = 1514;
pub(crate) const HEADER: usize = 36;
pub(crate) const MAGIC: &[u8; 4] = b"PVN1";

/// Explicit membership in a same-host IPv4 packet network.
///
/// Grants unicast IPv4 TCP/UDP/ICMP and targeted ARP between members, not
/// arbitrary host access. The caller allocates addresses and hides the directory
/// from guests. Unix directories must be owned and 0700; Windows directories
/// must have a private caller/SYSTEM ACL inherited from the owning store.
#[derive(Debug, Clone)]
pub struct PeerNetworkConfig {
    /// Existing canonical private host directory; different directories isolate networks.
    pub directory: PathBuf,
    /// Unique usable member address in the gateway's private IPv4 /24.
    pub address: Ipv4Addr,
    /// Router in the same /24, excluding network and broadcast addresses.
    pub gateway: Ipv4Addr,
    /// At most 16 network-local DNS names. Empty disables member-name DNS.
    pub aliases: Vec<String>,
}

pub(crate) fn validate_config(config: &PeerNetworkConfig) -> anyhow::Result<()> {
    anyhow::ensure!(
        member_address(config.address)
            && member_address(config.gateway)
            && config.address != config.gateway
            && config.address.octets()[..3] == config.gateway.octets()[..3]
            && (config.gateway.is_private() || config.gateway.octets()[..3] == [192, 0, 2]),
        "peer address must be a non-gateway host in a private IPv4 /24 (or 192.0.2.0/24)"
    );
    anyhow::ensure!(
        config.aliases.len() <= 16
            && config.aliases.iter().all(|name| !name.is_empty()
                && name.len() <= 128
                && name.split('.').all(|part| !part.is_empty()
                    && part.len() <= 63
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))),
        "invalid peer DNS alias"
    );
    anyhow::ensure!(
        config.directory.is_absolute() && config.directory.canonicalize()? == config.directory,
        "peer directory must be canonical and absolute"
    );
    Ok(())
}

pub(crate) fn member_address(address: Ipv4Addr) -> bool {
    (1..=254).contains(&address.octets()[3])
}
pub(crate) fn reserved_frame(frame: &[u8], gateway: Ipv4Addr) -> bool {
    let Ok(ethernet) = EthernetFrame::new_checked(frame) else {
        return true;
    };
    match ethernet.ethertype() {
        EthernetProtocol::Ipv4 => Ipv4Packet::new_checked(ethernet.payload()).is_ok_and(|ip| {
            ip.dst_addr().octets()[..3] == gateway.octets()[..3] && ip.dst_addr() != gateway
        }),
        EthernetProtocol::Arp => ArpPacket::new_checked(ethernet.payload())
            .map(|arp| arp.target_protocol_addr() != gateway.octets())
            .unwrap_or(true),
        _ => true,
    }
}
pub(crate) fn frame_addresses(frame: &[u8]) -> Option<(Ipv4Addr, Ipv4Addr)> {
    let ethernet = EthernetFrame::new_checked(frame).ok()?;
    match ethernet.ethertype() {
        EthernetProtocol::Ipv4 => {
            let ip = Ipv4Packet::new_checked(ethernet.payload()).ok()?;
            Some((ip.src_addr(), ip.dst_addr()))
        }
        EthernetProtocol::Arp => {
            let arp = ArpPacket::new_checked(ethernet.payload()).ok()?;
            Some((
                Ipv4Addr::from(<[u8; 4]>::try_from(arp.source_protocol_addr()).ok()?),
                Ipv4Addr::from(<[u8; 4]>::try_from(arp.target_protocol_addr()).ok()?),
            ))
        }
        _ => None,
    }
}
pub(crate) fn valid_frame(frame: &[u8], source: Ipv4Addr, destination: Ipv4Addr) -> bool {
    if frame.len() > FRAME_LIMIT
        || source == destination
        || !member_address(source)
        || !member_address(destination)
        || source.octets()[..3] != destination.octets()[..3]
    {
        return false;
    }
    let Ok(ethernet) = EthernetFrame::new_checked(frame) else {
        return false;
    };
    if ethernet.src_addr().0 != guest_mac(source) {
        return false;
    }
    match ethernet.ethertype() {
        EthernetProtocol::Ipv4 => {
            let Ok(ip) = Ipv4Packet::new_checked(ethernet.payload()) else {
                return false;
            };
            ethernet.dst_addr().0 == guest_mac(destination)
                && ip.verify_checksum()
                && !ip.more_frags()
                && ip.frag_offset() == 0
                && matches!(
                    ip.next_header(),
                    IpProtocol::Tcp | IpProtocol::Udp | IpProtocol::Icmp
                )
        }
        EthernetProtocol::Arp => {
            let Ok(arp) = ArpPacket::new_checked(ethernet.payload()) else {
                return false;
            };
            arp.hardware_type() == smoltcp::wire::ArpHardware::Ethernet
                && arp.protocol_type() == EthernetProtocol::Ipv4
                && arp.hardware_len() == 6
                && arp.protocol_len() == 4
                && arp.source_hardware_addr() == guest_mac(source)
                && match arp.operation() {
                    ArpOperation::Request => {
                        ethernet.dst_addr().is_broadcast()
                            || ethernet.dst_addr().0 == guest_mac(destination)
                    }
                    ArpOperation::Reply => {
                        ethernet.dst_addr().0 == guest_mac(destination)
                            && arp.target_hardware_addr() == guest_mac(destination)
                    }
                    _ => false,
                }
        }
        _ => false,
    }
}
