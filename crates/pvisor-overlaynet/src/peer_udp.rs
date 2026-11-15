//! Private, generation-authenticated loopback datagrams for Windows peers.
//! Packet rules are shared with Unix; neither host UDP endpoints nor membership
//! nonces are exposed to guests. The caller owns the directory's private ACL.
use crate::{
    interception::InterceptionMetrics,
    peer_protocol::{
        FRAME_LIMIT, HEADER, MAGIC, PeerNetworkConfig, frame_addresses, member_address,
        reserved_frame, valid_frame,
    },
};
use anyhow::{Context, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket as StdUdp},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::net::UdpSocket;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    version: u8,
    address: Ipv4Addr,
    gateway: Ipv4Addr,
    aliases: Vec<String>,
    endpoint: SocketAddrV4,
    generation: [u8; 16],
    instance: Option<String>,
}
struct Membership {
    config: PeerNetworkConfig,
    member: Member,
    slot: PathBuf,
    _lease: File,
}
fn directory(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "unsafe peer directory"
    );
    Ok(())
}
fn read_member(path: &Path) -> anyhow::Result<Member> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 4096,
        "unsafe peer record"
    );
    Ok(serde_json::from_reader(File::open(path)?.take(4097))?)
}
fn load(config: &PeerNetworkConfig, address: Ipv4Addr) -> anyhow::Result<Member> {
    ensure!(
        member_address(address)
            && address != config.gateway
            && address.octets()[..3] == config.gateway.octets()[..3],
        "invalid peer address"
    );
    directory(&config.directory)?;
    let slot = config.directory.join(address.octets()[3].to_string());
    directory(&slot)?;
    let lease_path = slot.join("lease.lock");
    ensure!(
        !fs::symlink_metadata(&lease_path)?.file_type().is_symlink(),
        "unsafe peer lease"
    );
    let lease = OpenOptions::new().read(true).write(true).open(lease_path)?;
    match lease.try_lock_exclusive() {
        Ok(()) => anyhow::bail!("peer is no longer live"),
        Err(error)
            if error.kind() == io::ErrorKind::WouldBlock
                || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {}
        Err(error) => return Err(error.into()),
    }
    let member = read_member(&slot.join("peer.json"))?;
    ensure!(
        member.version == 2
            && member.address == address
            && member.gateway == config.gateway
            && member.endpoint.ip().is_loopback()
            && member.endpoint.port() != 0,
        "invalid peer binding"
    );
    Ok(member)
}
pub(crate) fn lookup_peer_name(config: &PeerNetworkConfig, name: &str) -> Option<Ipv4Addr> {
    if config.aliases.is_empty() {
        return None;
    }
    let mut found = None;
    for entry in fs::read_dir(&config.directory).ok()? {
        let entry = entry.ok()?;
        let Some(last) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u8>().ok())
        else {
            continue;
        };
        let mut address = config.gateway.octets();
        address[3] = last;
        if let Ok(member) = load(config, Ipv4Addr::from(address))
            && member
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
        {
            if found.is_some() {
                return None;
            }
            found = Some(member.address);
        }
    }
    found
}
impl Drop for Membership {
    fn drop(&mut self) {
        let path = self.slot.join("peer.json");
        if read_member(&path).is_ok_and(|m| m.generation == self.member.generation) {
            let _ = fs::remove_file(path);
        }
    }
}
pub(crate) struct BoundPeer {
    socket: StdUdp,
    membership: Membership,
}
impl BoundPeer {
    pub(crate) fn bind(
        config: &PeerNetworkConfig,
        instance: Option<String>,
    ) -> anyhow::Result<Self> {
        crate::peer_protocol::validate_config(config)?;
        directory(&config.directory)?;
        let slot = config
            .directory
            .join(config.address.octets()[3].to_string());
        fs::create_dir_all(&slot)?;
        directory(&slot)?;
        let lease_path = slot.join("lease.lock");
        if lease_path.exists() {
            ensure!(
                !fs::symlink_metadata(&lease_path)?.file_type().is_symlink(),
                "unsafe peer lease"
            );
        }
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lease_path)?;
        lease
            .try_lock_exclusive()
            .context("peer address already in use")?;
        let socket = StdUdp::bind((Ipv4Addr::LOCALHOST, 0))?;
        socket.set_nonblocking(true)?;
        let SocketAddr::V4(endpoint) = socket.local_addr()? else {
            unreachable!()
        };
        let member = Member {
            version: 2,
            address: config.address,
            gateway: config.gateway,
            aliases: config.aliases.clone(),
            endpoint,
            generation: *uuid::Uuid::new_v4().as_bytes(),
            instance,
        };
        let membership = Membership {
            config: config.clone(),
            member,
            slot,
            _lease: lease,
        };
        let mut temporary = tempfile::NamedTempFile::new_in(&membership.slot)?;
        serde_json::to_writer(&mut temporary, &membership.member)?;
        temporary.flush()?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(membership.slot.join("peer.json"))
            .map_err(|e| e.error)?;
        Ok(Self { socket, membership })
    }
    pub(crate) fn adopt(self, metrics: InterceptionMetrics) -> io::Result<PeerNetwork> {
        Ok(PeerNetwork {
            socket: UdpSocket::from_std(self.socket)?,
            membership: self.membership,
            cache: HashMap::new(),
            outgoing: HashMap::new(),
            metrics,
        })
    }
}
pub(crate) struct PeerNetwork {
    socket: UdpSocket,
    membership: Membership,
    cache: HashMap<Ipv4Addr, (Instant, Option<Member>)>,
    outgoing: HashMap<Ipv4Addr, (StdUdp, [u8; 16])>,
    metrics: InterceptionMetrics,
}
impl PeerNetwork {
    fn lookup(&mut self, address: Ipv4Addr) -> Option<Member> {
        if let Some((loaded, member)) = self.cache.get(&address)
            && loaded.elapsed() < Duration::from_millis(250)
        {
            return member.clone();
        }
        let member = load(&self.membership.config, address).ok();
        self.cache.insert(address, (Instant::now(), member.clone()));
        member
    }
    pub(crate) fn route(&mut self, frame: &[u8]) -> bool {
        if !reserved_frame(frame, self.membership.member.gateway) {
            return false;
        }
        let Some((source, destination)) = frame_addresses(frame) else {
            self.metrics.peer_dropped();
            return true;
        };
        if source != self.membership.member.address || !valid_frame(frame, source, destination) {
            self.metrics.peer_dropped();
            return true;
        }
        let Some(member) = self.lookup(destination) else {
            self.metrics.peer_dropped();
            return true;
        };
        if self
            .outgoing
            .get(&destination)
            .is_some_and(|(_, generation)| *generation != member.generation)
        {
            self.outgoing.remove(&destination);
        }
        if !self.outgoing.contains_key(&destination) {
            let socket = match StdUdp::bind((Ipv4Addr::LOCALHOST, 0)).and_then(|socket| {
                socket.set_nonblocking(true)?;
                socket2::SockRef::from(&socket).set_send_buffer_size(16 * 1024)?;
                Ok(socket)
            }) {
                Ok(socket) => socket,
                Err(_) => {
                    self.metrics.peer_dropped();
                    return true;
                }
            };
            self.outgoing
                .insert(destination, (socket, member.generation));
        }
        let mut packet = [0; HEADER + FRAME_LIMIT];
        packet[..4].copy_from_slice(MAGIC);
        packet[4..20].copy_from_slice(&self.membership.member.generation);
        packet[20..HEADER].copy_from_slice(&member.generation);
        packet[HEADER..HEADER + frame.len()].copy_from_slice(frame);
        match self.outgoing[&destination]
            .0
            .send_to(&packet[..HEADER + frame.len()], member.endpoint)
        {
            Ok(size) if size == HEADER + frame.len() => self.metrics.peer_sent(),
            _ => self.metrics.peer_dropped(),
        }
        true
    }
    pub(crate) fn flush(&mut self) {}
    pub(crate) async fn receive(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            self.socket.readable().await?;
            match self.try_receive() {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                result => return result,
            }
        }
    }
    pub(crate) fn try_receive(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut packet = [0; HEADER + FRAME_LIMIT + 1];
        let (len, sender) = self.socket.try_recv_from(&mut packet)?;
        let valid = (HEADER..=HEADER + FRAME_LIMIT).contains(&len)
            && sender.ip().is_loopback()
            && &packet[..4] == MAGIC
            && packet[20..HEADER] == self.membership.member.generation;
        if valid {
            let frame = &packet[HEADER..len];
            if let Some((source, destination)) = frame_addresses(frame)
                && destination == self.membership.member.address
                && valid_frame(frame, source, destination)
            {
                // Receive identity must observe a replacement immediately:
                // a cached old generation could accept a delayed packet
                // after that sender has exited and its address is reused.
                let member = load(&self.membership.config, source).ok();
                if member.is_some_and(|m| packet[4..20] == m.generation) {
                    self.metrics.peer_received();
                    return Ok(Some(frame.to_vec()));
                }
            }
        }
        self.metrics.peer_dropped();
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::wire::{ArpHardware, ArpOperation, ArpPacket, EthernetFrame, EthernetProtocol};
    fn arp(source: Ipv4Addr, destination: Ipv4Addr) -> Vec<u8> {
        let mut bytes = vec![0; 42];
        let mut frame = EthernetFrame::new_unchecked(&mut bytes);
        frame.set_src_addr(smoltcp::wire::EthernetAddress(crate::vm::guest_mac(source)));
        frame.set_dst_addr(smoltcp::wire::EthernetAddress::BROADCAST);
        frame.set_ethertype(EthernetProtocol::Arp);
        let mut arp = ArpPacket::new_unchecked(frame.payload_mut());
        arp.set_hardware_type(ArpHardware::Ethernet);
        arp.set_protocol_type(EthernetProtocol::Ipv4);
        arp.set_hardware_len(6);
        arp.set_protocol_len(4);
        arp.set_operation(ArpOperation::Request);
        arp.set_source_hardware_addr(&crate::vm::guest_mac(source));
        arp.set_source_protocol_addr(&source.octets());
        arp.set_target_hardware_addr(&[0; 6]);
        arp.set_target_protocol_addr(&destination.octets());
        bytes
    }
    #[tokio::test]
    async fn loopback_peers_fence_membership_generations_and_preserve_guest_packets() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let one = PeerNetworkConfig {
            directory: root.clone(),
            address: Ipv4Addr::new(172, 17, 0, 2),
            gateway: Ipv4Addr::new(172, 17, 0, 1),
            aliases: vec!["one".into()],
        };
        let two = PeerNetworkConfig {
            address: Ipv4Addr::new(172, 17, 0, 3),
            aliases: vec!["two".into()],
            ..one.clone()
        };
        let metrics = InterceptionMetrics::default();
        let mut left = BoundPeer::bind(&one, Some("left".into()))
            .unwrap()
            .adopt(metrics.clone())
            .unwrap();
        let mut right = BoundPeer::bind(&two, Some("right".into()))
            .unwrap()
            .adopt(metrics.clone())
            .unwrap();
        assert!(
            BoundPeer::bind(&one, None).is_err(),
            "duplicate member address was admitted"
        );
        assert_eq!(lookup_peer_name(&one, "two"), Some(two.address));
        let frame = arp(one.address, two.address);
        assert!(left.route(&frame));
        left.flush();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), right.receive())
                .await
                .unwrap()
                .unwrap(),
            Some(frame.clone())
        );
        let old_generation = left.membership.member.generation;
        drop(left);
        let mut left = BoundPeer::bind(&one, None)
            .unwrap()
            .adopt(metrics.clone())
            .unwrap();
        assert_ne!(left.membership.member.generation, old_generation);
        let mut stale = Vec::from(MAGIC.as_slice());
        stale.extend_from_slice(&old_generation);
        stale.extend_from_slice(&right.membership.member.generation);
        stale.extend_from_slice(&frame);
        let attacker = StdUdp::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        attacker
            .send_to(&stale, right.membership.member.endpoint)
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), right.receive())
                .await
                .unwrap()
                .unwrap(),
            None
        );
        left.route(&frame);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), right.receive())
                .await
                .unwrap()
                .unwrap(),
            Some(frame)
        );
        drop(right);
        assert!(load(&two, two.address).is_err());
        assert!(lookup_peer_name(&one, "two").is_none());
    }
}
