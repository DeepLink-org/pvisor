//! Same-user directory discovery and a bounded Unix-datagram packet link.
//!
//! Directory records describe membership independently of the packet transport.
//! The address lease serializes publication/restart, never packet forwarding.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::Ipv4Addr;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixDatagram as StdUnixDatagram;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use smoltcp::wire::{
    ArpOperation, ArpPacket, EthernetFrame, EthernetProtocol, IpProtocol, Ipv4Packet,
};
use tokio::net::UnixDatagram;

use crate::interception::InterceptionMetrics;
use crate::vm::guest_mac;

pub(crate) const FRAME_LIMIT: usize = 1514;
const HEADER: usize = 36;
const MAGIC: &[u8; 4] = b"PVN1";
const CACHE_TTL: Duration = Duration::from_millis(250);

/// Explicit membership in a same-host IPv4 packet network.
///
/// Membership grants bidirectional IPv4 TCP, UDP and ICMP to other members of
/// this directory. IPv6, fragments, VLANs and general broadcast/multicast are
/// unsupported and dropped. External traffic still uses the VM egress policy.
/// The owning runtime must exclude this directory from every guest filesystem.
#[derive(Debug, Clone)]
pub struct PeerNetworkConfig {
    /// Existing canonical absolute network directory, owned by the effective
    /// UID with mode 0700. All members use the same directory; different
    /// directories define isolated networks. No symlink aliases are accepted.
    pub directory: PathBuf,
    /// Unique usable guest host in gateway's IPv4 /24. The per-address lifetime
    /// lease refuses duplicate live members and excludes gateway/boundaries.
    pub address: Ipv4Addr,
    /// Virtual router in the same private IPv4 /24. Neither router nor subnet
    /// boundary addresses can be members. Native legacy links use 192.0.2.1.
    pub gateway: Ipv4Addr,
    /// Network-local DNS aliases, at most 16 names of up to 128 bytes. Empty
    /// disables peer DNS for this member; external DNS remains synthetic.
    pub aliases: Vec<String>,
}

/// Transport-independent membership identity, atomically published after bind.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    version: u32,
    transport: String,
    address: Ipv4Addr,
    gateway: Ipv4Addr,
    aliases: Vec<String>,
    mac: [u8; 6],
    generation: [u8; 16],
    instance: Option<String>,
    socket_device: u64,
    socket_inode: u64,
}

struct Membership {
    root: PathBuf,
    slot: PathBuf,
    member: Member,
    // Keep the inode and lock for the whole lifetime. Never unlink lease.lock:
    // doing so would let waiters and new callers lock different inodes.
    _lease: File,
}

impl Membership {
    fn lookup(&self, address: Ipv4Addr) -> anyhow::Result<Member> {
        load_member(&self.root, self.member.gateway, address)
    }

    fn socket(&self, address: Ipv4Addr) -> PathBuf {
        self.root
            .join(address.octets()[3].to_string())
            .join("net.sock")
    }
}

fn load_member(root: &Path, gateway: Ipv4Addr, address: Ipv4Addr) -> anyhow::Result<Member> {
    ensure!(
        member_address(address)
            && address.octets()[..3] == gateway.octets()[..3]
            && address != gateway,
        "invalid peer address"
    );
    private_directory(root, false)?;
    let slot = root.join(address.octets()[3].to_string());
    private_directory(&slot, false)?;
    // A stale record from a crashed process is not a live member.
    let lease = private_file(&slot.join("lease.lock"), false)?;
    match lease.try_lock_exclusive() {
        Ok(()) => anyhow::bail!("peer is no longer live"),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
        Err(error) => return Err(error.into()),
    }
    let file = private_file(&slot.join("peer.json"), false)?;
    ensure!(file.metadata()?.len() <= 4096, "peer record too large");
    let record: Member = serde_json::from_reader(file.take(4097))?;
    ensure!(
        record.version == 1 && record.transport == "unix-datagram-v1",
        "unsupported peer protocol"
    );
    ensure!(
        record.address == address && record.gateway == gateway && record.mac == guest_mac(address),
        "peer address mismatch"
    );
    let socket = fs::symlink_metadata(slot.join("net.sock"))?;
    ensure!(
        socket.file_type().is_socket()
            && socket.uid() == uid()
            && socket.mode() & 0o7777 == 0o600
            && socket.dev() == record.socket_device
            && socket.ino() == record.socket_inode,
        "peer socket identity changed"
    );
    Ok(record)
}

pub(crate) fn lookup_peer_name(config: &PeerNetworkConfig, name: &str) -> Option<Ipv4Addr> {
    if config.aliases.is_empty() {
        return None;
    }
    private_directory(&config.directory, false).ok()?;
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
        let mut bytes = config.gateway.octets();
        bytes[3] = last;
        let address = Ipv4Addr::from(bytes);
        if let Ok(member) = load_member(&config.directory, config.gateway, address)
            && member
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
        {
            if found.is_some() {
                return None;
            }
            found = Some(address);
        }
    }
    found
}

impl Drop for Membership {
    fn drop(&mut self) {
        // The address lease still fences publication. Remove only our inode.
        let path = self.slot.join("net.sock");
        if fs::symlink_metadata(&path).is_ok_and(|m| {
            m.file_type().is_socket()
                && m.dev() == self.member.socket_device
                && m.ino() == self.member.socket_inode
        }) {
            let _ = fs::remove_file(self.slot.join("peer.json"));
            let _ = fs::remove_file(path);
        }
    }
}

struct Outgoing {
    socket: StdUnixDatagram,
    path: PathBuf,
    device: u64,
    inode: u64,
    generation: [u8; 16],
}

impl Outgoing {
    fn bind(path: PathBuf, destination: &Path, generation: [u8; 16]) -> anyhow::Result<Self> {
        let socket = StdUnixDatagram::bind(&path)?;
        let metadata = fs::symlink_metadata(&path)?;
        let link = Self {
            socket,
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            generation,
        };
        fs::set_permissions(&link.path, fs::Permissions::from_mode(0o600))?;
        link.socket.set_nonblocking(true)?;
        let bytes: libc::c_int = 16 * 1024;
        // Bound each peer's kernel send budget, independently of host defaults.
        let result = unsafe {
            libc::setsockopt(
                link.socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&bytes as *const libc::c_int).cast(),
                std::mem::size_of_val(&bytes) as libc::socklen_t,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error().into());
        }
        link.socket.connect(destination)?;
        Ok(link)
    }
}

impl Drop for Outgoing {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|m| {
            m.file_type().is_socket() && m.dev() == self.device && m.ino() == self.inode
        }) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

type MemberCache = HashMap<Ipv4Addr, (Instant, Option<[u8; 16]>)>;

struct PendingPacket {
    destination: Ipv4Addr,
    generation: [u8; 16],
    bytes: [u8; HEADER + FRAME_LIMIT],
    length: usize,
}

pub(crate) struct PeerNetwork {
    socket: UnixDatagram,
    // Separate socket send buffers prevent a blocked destination from using
    // every healthy peer's send budget. At most one link per member address.
    outgoing: HashMap<Ipv4Addr, Outgoing>,
    membership: Membership,
    // Packet forwarding only needs the generation from a fully validated
    // publication. Keep this Copy identity instead of cloning DNS aliases and
    // other discovery metadata on every send and receive.
    cache: MemberCache,
    received: crate::peer_batch::ReceiveBatch,
    pending: Box<[PendingPacket; crate::peer_batch::BATCH]>,
    pending_count: usize,
    metrics: InterceptionMetrics,
}

/// Binding happens synchronously before VmNetwork::start reports success.
/// Tokio adoption happens on the backend's runtime thread.
pub(crate) struct BoundPeer {
    socket: StdUnixDatagram,
    membership: Membership,
}

impl BoundPeer {
    pub(crate) fn bind(
        config: &PeerNetworkConfig,
        instance: Option<String>,
    ) -> anyhow::Result<Self> {
        ensure!(
            member_address(config.address)
                && member_address(config.gateway)
                && config.address.octets()[..3] == config.gateway.octets()[..3]
                && config.address != config.gateway
                && (config.gateway.is_private() || config.gateway.octets()[..3] == [192, 0, 2]),
            "peer address must be a non-gateway host in a private IPv4 /24 (or 192.0.2.0/24)"
        );
        ensure!(
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
        ensure!(
            config.directory.is_absolute() && config.directory.canonicalize()? == config.directory,
            "peer directory must be canonical and absolute"
        );
        private_directory(&config.directory, false)?;
        let slot = config
            .directory
            .join(config.address.octets()[3].to_string());
        private_directory(&slot, true)?;
        let lease = private_file(&slot.join("lease.lock"), true)?;
        lease
            .try_lock_exclusive()
            .context("peer address already in use")?;
        for name in ["peer.json", "net.sock"] {
            let path = slot.join(name);
            match fs::symlink_metadata(&path) {
                Ok(m) => {
                    ensure!(
                        m.uid() == uid()
                            && m.nlink() == 1
                            && if name == "net.sock" {
                                m.file_type().is_socket()
                            } else {
                                m.is_file()
                            },
                        "unsafe stale peer entry"
                    );
                    fs::remove_file(path)?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        for entry in fs::read_dir(&slot)? {
            let entry = entry?;
            let name = entry.file_name();
            if name
                .to_str()
                .and_then(|name| name.strip_prefix("tx-"))
                .and_then(|name| name.strip_suffix(".sock"))
                .and_then(|name| name.parse::<u8>().ok())
                .is_some_and(|last| (1..=254).contains(&last))
            {
                let metadata = fs::symlink_metadata(entry.path())?;
                ensure!(
                    metadata.file_type().is_socket()
                        && metadata.uid() == uid()
                        && metadata.nlink() == 1,
                    "unsafe stale peer transmit socket"
                );
                fs::remove_file(entry.path())?;
            }
        }
        let path = slot.join("net.sock");
        // Reserve enough space for both Linux and macOS sockaddr_un.
        ensure!(path.as_os_str().len() < 104, "peer socket path is too long");
        let socket = StdUnixDatagram::bind(&path).context("bind peer packet socket")?;
        let metadata = fs::symlink_metadata(&path)?;
        let member = Member {
            version: 1,
            transport: "unix-datagram-v1".into(),
            address: config.address,
            gateway: config.gateway,
            aliases: config.aliases.clone(),
            mac: guest_mac(config.address),
            generation: *uuid::Uuid::new_v4().as_bytes(),
            instance,
            socket_device: metadata.dev(),
            socket_inode: metadata.ino(),
        };
        let membership = Membership {
            root: config.directory.clone(),
            slot,
            member,
            _lease: lease,
        };
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        socket.set_nonblocking(true)?;
        let temporary = membership.slot.join("peer.json.tmp");
        // A previous crash may leave only this private publication scratch file.
        if temporary.try_exists()? {
            let _checked = private_file(&temporary, false)?;
            fs::remove_file(&temporary)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(&membership.member)?)?;
        fs::rename(temporary, membership.slot.join("peer.json"))?;
        Ok(Self { socket, membership })
    }

    pub(crate) fn adopt(self, metrics: InterceptionMetrics) -> io::Result<PeerNetwork> {
        Ok(PeerNetwork {
            socket: UnixDatagram::from_std(self.socket)?,
            outgoing: HashMap::new(),
            membership: self.membership,
            cache: HashMap::new(),
            received: crate::peer_batch::ReceiveBatch::new(),
            pending: Box::new(std::array::from_fn(|_| PendingPacket {
                destination: Ipv4Addr::UNSPECIFIED,
                generation: [0; 16],
                bytes: [0; HEADER + FRAME_LIMIT],
                length: 0,
            })),
            pending_count: 0,
            metrics,
        })
    }
}

impl PeerNetwork {
    fn lookup(
        membership: &Membership,
        cache: &mut MemberCache,
        address: Ipv4Addr,
    ) -> Option<[u8; 16]> {
        if !member_address(address)
            || address.octets()[..3] != membership.member.gateway.octets()[..3]
            || address == membership.member.gateway
        {
            return None;
        }
        if let Some((loaded, record)) = cache.get(&address)
            && loaded.elapsed() < CACHE_TTL
        {
            return *record;
        }
        let record = membership
            .lookup(address)
            .ok()
            .map(|member| member.generation);
        cache.insert(address, (Instant::now(), record));
        record
    }

    /// Returns true for all traffic reserved for this packet network, including
    /// malformed packets and absent peers. Such traffic must never reach egress.
    pub(crate) fn route(&mut self, frame: &[u8]) -> bool {
        if !reserved_frame(frame, self.membership.member.gateway) {
            return false;
        }
        let source = self.membership.member.address;
        let Some(destination) =
            frame_addresses(frame).and_then(|(src, dst)| (src == source).then_some(dst))
        else {
            self.metrics.peer_dropped();
            return true;
        };
        let Some(generation) = Self::lookup(&self.membership, &mut self.cache, destination) else {
            self.metrics.peer_dropped();
            return true;
        };
        if !valid_frame(frame, source, destination) {
            self.metrics.peer_dropped();
            return true;
        }
        if self
            .outgoing
            .get(&destination)
            .is_some_and(|link| link.generation != generation)
        {
            self.outgoing.remove(&destination);
        }
        if !self.outgoing.contains_key(&destination) {
            let path = self
                .membership
                .slot
                .join(format!("tx-{}.sock", destination.octets()[3]));
            match Outgoing::bind(path, &self.membership.socket(destination), generation) {
                Ok(link) => {
                    self.outgoing.insert(destination, link);
                }
                Err(_) => {
                    self.metrics.peer_dropped();
                    return true;
                }
            }
        }
        if self.pending_count == crate::peer_batch::BATCH {
            self.flush();
        }
        let packet = &mut self.pending[self.pending_count];
        packet.destination = destination;
        packet.generation = generation;
        packet.length = HEADER + frame.len();
        packet.bytes[..4].copy_from_slice(MAGIC);
        packet.bytes[4..20].copy_from_slice(&self.membership.member.generation);
        packet.bytes[20..HEADER].copy_from_slice(&generation);
        packet.bytes[HEADER..packet.length].copy_from_slice(frame);
        self.pending_count += 1;
        true
    }

    pub(crate) fn flush(&mut self) {
        let mut handled = 0u32;
        for first in 0..self.pending_count {
            if handled & (1 << first) != 0 {
                continue;
            }
            let destination = self.pending[first].destination;
            let link = self.outgoing.get(&destination);
            let generation = link.map(|link| link.generation);
            let fd = link.map(|link| link.socket.as_raw_fd());
            let mut slices: [io::IoSlice<'_>; crate::peer_batch::BATCH] =
                std::array::from_fn(|_| io::IoSlice::new(&[]));
            let mut count = 0;
            for (index, packet) in self.pending[..self.pending_count]
                .iter()
                .enumerate()
                .skip(first)
            {
                if packet.destination != destination {
                    continue;
                }
                handled |= 1 << index;
                if Some(packet.generation) != generation {
                    self.metrics.peer_dropped();
                    continue;
                }
                slices[count] = io::IoSlice::new(&packet.bytes[..packet.length]);
                count += 1;
            }
            let mut sent = 0;
            while sent < count {
                match crate::peer_batch::send(
                    fd.expect("validated outgoing link"),
                    &slices[sent..count],
                ) {
                    Ok(progress) => {
                        sent += progress;
                        for _ in 0..progress {
                            self.metrics.peer_sent();
                        }
                    }
                    Err(error) => {
                        // Retain best-effort nonblocking semantics, including
                        // independent budgets for healthy and stalled peers.
                        if error.kind() != io::ErrorKind::WouldBlock {
                            self.outgoing.remove(&destination);
                            self.cache.remove(&destination);
                        }
                        for _ in sent..count {
                            self.metrics.peer_dropped();
                        }
                        break;
                    }
                }
            }
        }
        self.pending_count = 0;
    }

    pub(crate) async fn receive(&mut self) -> io::Result<Option<Vec<u8>>> {
        while self.received.empty() {
            self.socket.readable().await?;
            match self.received.fill(&self.socket) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(self.next_received())
    }

    pub(crate) fn try_receive(&mut self) -> io::Result<Option<Vec<u8>>> {
        if self.received.empty() {
            self.received.fill(&self.socket)?;
        }
        Ok(self.next_received())
    }

    fn next_received(&mut self) -> Option<Vec<u8>> {
        let (packet, sender) = self.received.next();
        if Self::validate_received(&self.membership, &mut self.cache, packet, sender) {
            self.metrics.peer_received();
            Some(packet[HEADER..].to_vec())
        } else {
            self.metrics.peer_dropped();
            None
        }
    }

    fn validate_received(
        membership: &Membership,
        cache: &mut MemberCache,
        packet: &[u8],
        sender: Option<&Path>,
    ) -> bool {
        if packet.len() < HEADER
            || packet.len() > HEADER + FRAME_LIMIT
            || &packet[..4] != MAGIC
            || packet[20..36] != membership.member.generation
        {
            return false;
        }
        let frame = &packet[HEADER..];
        let Some((source, destination)) = frame_addresses(frame) else {
            return false;
        };
        if destination != membership.member.address
            || !valid_frame(frame, source, destination)
            || sender
                != Some(
                    membership
                        .root
                        .join(source.octets()[3].to_string())
                        .join(format!("tx-{}.sock", destination.octets()[3]))
                        .as_path(),
                )
        {
            return false;
        }
        let Some(mut generation) = Self::lookup(membership, cache, source) else {
            return false;
        };
        if packet[4..20] != generation {
            cache.remove(&source);
            let Some(fresh) = Self::lookup(membership, cache, source) else {
                return false;
            };
            generation = fresh;
        }
        packet[4..20] == generation
    }
}

pub(crate) fn member_address(address: Ipv4Addr) -> bool {
    let bytes = address.octets();
    (1..=254).contains(&bytes[3])
}

/// Reserve the entire local subnet, except the router. Even unknown members,
/// broadcast, invalid checksums or unsupported protocols must not escape.
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

fn frame_addresses(frame: &[u8]) -> Option<(Ipv4Addr, Ipv4Addr)> {
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

fn valid_frame(frame: &[u8], source: Ipv4Addr, destination: Ipv4Addr) -> bool {
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

fn uid() -> u32 {
    unsafe { libc::geteuid() }
}

fn private_directory(path: &Path, create: bool) -> anyhow::Result<()> {
    if create {
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && metadata.uid() == uid() && metadata.mode() & 0o7777 == 0o700,
        "peer directory must be owned, non-symlink and 0700: {}",
        path.display()
    );
    Ok(())
}

fn private_file(path: &Path, create: bool) -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .mode(0o600)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == uid()
            && metadata.mode() & 0o7777 == 0o600
            && metadata.nlink() == 1,
        "unsafe peer file: {}",
        path.display()
    );
    Ok(file)
}
