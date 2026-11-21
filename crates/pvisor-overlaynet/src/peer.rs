//! Same-user directory discovery and a bounded Unix-datagram packet link.
//!
//! Directory records describe membership independently of the packet transport.
//! The address lease serializes publication/restart, never packet forwarding.

use crate::api::VmNetworkConfig;
use crate::api::VmNetworkConfiguration;
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
use tokio::net::UnixDatagram;

use crate::api::InterceptionMetrics;

use crate::api::PeerNetworkConfig;
pub(crate) use crate::peer_protocol::{FRAME_LIMIT, member_address, reserved_frame};
use crate::peer_protocol::{HEADER, MAGIC, frame_addresses, valid_frame};
const CACHE_TTL: Duration = Duration::from_millis(250);

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
        record.address == address
            && record.gateway == gateway
            && record.mac == VmNetworkConfig::guest_mac(address),
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
    // Packet forwarding caches only validated member generations.
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
        crate::peer_protocol::validate_config(config)?;
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
            mac: VmNetworkConfig::guest_mac(config.address),
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
        crate::peer_batch::prepare_receiver(socket.as_raw_fd())
            .context("reserve peer receive burst budget")?;
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
                if link.is_none_or(|link| packet.generation != link.generation) {
                    self.metrics.peer_dropped();
                    continue;
                }
                slices[count] = io::IoSlice::new(&packet.bytes[..packet.length]);
                count += 1;
            }
            let Some(link) = link else {
                continue;
            };
            let fd = link.socket.as_raw_fd();
            let mut sent = 0;
            while sent < count {
                match crate::peer_batch::send(fd, &slices[sent..count]) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{InterceptionObservation, InterceptionSnapshot};

    #[rstest::fixture]
    fn directory() -> tempfile::TempDir {
        tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(fs::canonicalize("/tmp").unwrap())
            .unwrap()
    }

    fn config(root: &Path) -> PeerNetworkConfig {
        PeerNetworkConfig {
            directory: root.to_path_buf(),
            address: Ipv4Addr::new(192, 0, 2, 11),
            gateway: Ipv4Addr::new(192, 0, 2, 1),
            aliases: Vec::new(),
        }
    }

    fn stage(network: &mut PeerNetwork, destination: Ipv4Addr, generation: [u8; 16], bytes: &[u8]) {
        let packet = &mut network.pending[network.pending_count];
        packet.destination = destination;
        packet.generation = generation;
        packet.length = bytes.len();
        packet.bytes[..bytes.len()].copy_from_slice(bytes);
        network.pending_count += 1;
    }

    #[derive(Clone, Copy)]
    enum LinkState {
        Available,
        Missing,
        MixedGenerations,
        Disconnected,
    }

    #[rstest::rstest]
    #[case::available(LinkState::Available, 6, 0)]
    #[case::missing(LinkState::Missing, 3, 3)]
    #[case::mixed_generations(LinkState::MixedGenerations, 4, 2)]
    #[case::disconnected(LinkState::Disconnected, 3, 3)]
    #[tokio::test]
    async fn flush_preserves_healthy_destinations_and_counts_each_packet_once(
        directory: tempfile::TempDir,
        #[case] state: LinkState,
        #[case] sent: u64,
        #[case] dropped: u64,
    ) {
        let mut network = BoundPeer::bind(&config(directory.path()), None)
            .unwrap()
            .adopt(InterceptionMetrics::default())
            .unwrap();
        let destinations = [Ipv4Addr::new(192, 0, 2, 12), Ipv4Addr::new(192, 0, 2, 13)];
        let mut receivers = Vec::new();
        for (index, destination) in destinations.into_iter().enumerate() {
            let path = directory.path().join(format!("sink-{index}"));
            let receiver = StdUnixDatagram::bind(&path).unwrap();
            receiver.set_nonblocking(true).unwrap();
            let link = Outgoing::bind(
                network
                    .membership
                    .slot
                    .join(format!("tx-{}.sock", destination.octets()[3])),
                &path,
                [1; 16],
            )
            .unwrap();
            network.outgoing.insert(destination, link);
            network
                .cache
                .insert(destination, (Instant::now(), Some([1; 16])));
            receivers.push(Some(receiver));
        }
        let payloads: [&[u8]; 6] = [b"a-0", b"b-0", b"a-1", b"b-1", b"a-2", b"b-2"];
        for (index, bytes) in payloads.into_iter().enumerate() {
            let generation =
                if matches!(state, LinkState::MixedGenerations) && matches!(index, 0 | 4) {
                    [2; 16]
                } else {
                    [1; 16]
                };
            stage(&mut network, destinations[index % 2], generation, bytes);
        }
        match state {
            LinkState::Missing => {
                network.outgoing.remove(&destinations[0]);
            }
            LinkState::Disconnected => drop(receivers[0].take()),
            _ => {}
        }
        network.flush();
        assert_eq!(network.pending_count, 0);
        let expected = InterceptionSnapshot {
            peer_packets_sent: sent,
            peer_packets_dropped: dropped,
            ..Default::default()
        };
        assert_eq!(network.metrics.snapshot(), expected);
        network.flush();
        assert_eq!(network.metrics.snapshot(), expected);
        assert!(network.outgoing.contains_key(&destinations[1]));
        assert!(network.cache.contains_key(&destinations[1]));
        assert_eq!(
            network.outgoing.contains_key(&destinations[0]),
            matches!(state, LinkState::Available | LinkState::MixedGenerations)
        );
        assert_eq!(
            network.cache.contains_key(&destinations[0]),
            !matches!(state, LinkState::Disconnected)
        );
        for (index, receiver) in receivers.iter().enumerate() {
            let Some(receiver) = receiver else { continue };
            let expected: &[&[u8]] = if index == 1 {
                &[b"b-0", b"b-1", b"b-2"]
            } else {
                match state {
                    LinkState::Available => &[b"a-0", b"a-1", b"a-2"],
                    LinkState::MixedGenerations => &[b"a-1"],
                    _ => &[],
                }
            };
            let mut bytes = [0; 64];
            for packet in expected {
                let length = receiver.recv(&mut bytes).unwrap();
                assert_eq!(&bytes[..length], *packet);
            }
            assert_eq!(
                receiver.recv(&mut bytes).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
        drop(network);
        for last in [12, 13] {
            assert!(!directory.path().join(format!("11/tx-{last}.sock")).exists());
        }
    }

    #[rstest::rstest]
    fn failed_outgoing_connect_cleans_up_and_allows_retry(directory: tempfile::TempDir) {
        let path = directory.path().join("tx.sock");
        let destination = directory.path().join("sink.sock");
        let error = Outgoing::bind(path.clone(), &destination, [1; 16])
            .err()
            .unwrap();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::NotFound
        );
        assert!(!path.exists());
        let receiver = StdUnixDatagram::bind(&destination).unwrap();
        receiver.set_nonblocking(true).unwrap();
        let link = Outgoing::bind(path.clone(), &destination, [2; 16]).unwrap();
        link.socket.send(b"retry").unwrap();
        let mut bytes = [0; 16];
        let length = receiver.recv(&mut bytes).unwrap();
        assert_eq!(&bytes[..length], b"retry");
        drop(link);
        assert!(!path.exists());
    }

    #[rstest::rstest]
    fn outgoing_drop_preserves_a_replacement_socket(directory: tempfile::TempDir) {
        let path = directory.path().join("tx.sock");
        let destination = directory.path().join("sink.sock");
        let _receiver = StdUnixDatagram::bind(&destination).unwrap();
        let link = Outgoing::bind(path.clone(), &destination, [1; 16]).unwrap();
        fs::rename(&path, directory.path().join("old.sock")).unwrap();
        let _replacement = StdUnixDatagram::bind(&path).unwrap();
        let inode = fs::symlink_metadata(&path).unwrap().ino();
        drop(link);
        assert_eq!(fs::symlink_metadata(&path).unwrap().ino(), inode);
    }

    #[rstest::rstest]
    fn membership_drop_preserves_replacement_publication_and_releases_lease(
        directory: tempfile::TempDir,
    ) {
        let config = config(directory.path());
        let bound = BoundPeer::bind(&config, None).unwrap();
        let slot = directory.path().join("11");
        let path = slot.join("net.sock");
        fs::rename(&path, slot.join("old.sock")).unwrap();
        let _replacement = StdUnixDatagram::bind(&path).unwrap();
        let inode = fs::symlink_metadata(&path).unwrap().ino();
        fs::write(slot.join("peer.json"), b"replacement publication").unwrap();
        let lease = private_file(&slot.join("lease.lock"), false).unwrap();
        assert_eq!(
            lease.try_lock_exclusive().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(bound);
        assert_eq!(fs::symlink_metadata(&path).unwrap().ino(), inode);
        assert_eq!(
            fs::read(slot.join("peer.json")).unwrap(),
            b"replacement publication"
        );
        lease.try_lock_exclusive().unwrap();
    }
}
