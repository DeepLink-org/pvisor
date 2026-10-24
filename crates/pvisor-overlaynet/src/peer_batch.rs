//! Private packet I/O. Linux batches syscalls; other Unix hosts keep the
//! same nonblocking packet semantics through their single-message primitives.
use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::fd::RawFd;
use std::path::Path;

use tokio::net::UnixDatagram;

pub(crate) const BATCH: usize = 32;
const CAPACITY: usize = 36 + super::peer::FRAME_LIMIT + 1;

struct Packet {
    bytes: [u8; CAPACITY],
    length: usize,
    #[cfg(target_os = "linux")]
    sender: libc::sockaddr_un,
    #[cfg(target_os = "linux")]
    sender_length: libc::socklen_t,
    #[cfg(not(target_os = "linux"))]
    sender: Option<tokio::net::unix::SocketAddr>,
}

impl Packet {
    fn sender(&self) -> Option<&Path> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStrExt;
            let offset = std::mem::offset_of!(libc::sockaddr_un, sun_path);
            let length = (self.sender_length as usize).checked_sub(offset)?;
            if self.sender.sun_family != libc::AF_UNIX as libc::sa_family_t
                || length > self.sender.sun_path.len()
            {
                return None;
            }
            // sun_path is a byte array, regardless of whether C char is signed.
            let bytes = unsafe {
                std::slice::from_raw_parts(self.sender.sun_path.as_ptr().cast::<u8>(), length)
            };
            if bytes.first().copied()? == 0 {
                return None;
            }
            let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(length);
            Some(Path::new(std::ffi::OsStr::from_bytes(&bytes[..end])))
        }
        #[cfg(not(target_os = "linux"))]
        self.sender.as_ref().and_then(|sender| sender.as_pathname())
    }
}

pub(crate) struct ReceiveBatch {
    packets: Box<[Packet; BATCH]>,
    count: usize,
    cursor: usize,
}

impl ReceiveBatch {
    pub(crate) fn new() -> Self {
        Self {
            packets: Box::new(std::array::from_fn(|_| Packet {
                bytes: [0; CAPACITY],
                length: 0,
                #[cfg(target_os = "linux")]
                sender: unsafe { std::mem::zeroed() },
                #[cfg(target_os = "linux")]
                sender_length: 0,
                #[cfg(not(target_os = "linux"))]
                sender: None,
            })),
            count: 0,
            cursor: 0,
        }
    }

    pub(crate) fn empty(&self) -> bool {
        self.cursor == self.count
    }

    pub(crate) fn next(&mut self) -> (&[u8], Option<&Path>) {
        assert!(!self.empty());
        let packet = &self.packets[self.cursor];
        self.cursor += 1;
        (&packet.bytes[..packet.length], packet.sender())
    }

    pub(crate) fn fill(&mut self, socket: &UnixDatagram) -> io::Result<()> {
        assert!(self.empty());
        #[cfg(target_os = "linux")]
        let count = socket.try_io(tokio::io::Interest::READABLE, || {
            self.receive(socket.as_raw_fd())
        })?;
        #[cfg(not(target_os = "linux"))]
        let count = {
            let packet = &mut self.packets[0];
            let (length, sender) = socket.try_recv_from(&mut packet.bytes)?;
            packet.length = length;
            packet.sender = Some(sender);
            1
        };
        self.count = count;
        self.cursor = 0;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn receive(&mut self, fd: RawFd) -> io::Result<usize> {
        let mut vectors: [libc::iovec; BATCH] = unsafe { std::mem::zeroed() };
        let mut messages: [libc::mmsghdr; BATCH] = unsafe { std::mem::zeroed() };
        for (index, packet) in self.packets.iter_mut().enumerate() {
            vectors[index].iov_base = packet.bytes.as_mut_ptr().cast();
            vectors[index].iov_len = CAPACITY;
            messages[index].msg_hdr.msg_iov = &mut vectors[index];
            messages[index].msg_hdr.msg_iovlen = 1;
            messages[index].msg_hdr.msg_name =
                (&mut packet.sender as *mut libc::sockaddr_un).cast();
            messages[index].msg_hdr.msg_namelen = std::mem::size_of_val(&packet.sender) as _;
        }
        let count = loop {
            // All output pointers reference distinct, live writable buffers.
            // DONTWAIT and a null timeout never wait for the batch to fill.
            let count = unsafe {
                libc::recvmmsg(
                    fd,
                    messages.as_mut_ptr(),
                    BATCH as _,
                    libc::MSG_DONTWAIT,
                    std::ptr::null_mut(),
                )
            };
            if count > 0 {
                break count as usize;
            }
            if count == 0 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        };
        for (packet, message) in self.packets.iter_mut().zip(&messages).take(count) {
            packet.length = if message.msg_hdr.msg_flags & libc::MSG_TRUNC != 0 {
                // CAPACITY is one byte larger than a valid protocol packet.
                // Preserve rejection even when a larger datagram was truncated.
                CAPACITY
            } else {
                (message.msg_len as usize).min(CAPACITY)
            };
            packet.sender_length = message.msg_hdr.msg_namelen;
        }
        Ok(count)
    }
}

pub(crate) fn send(fd: RawFd, packets: &[io::IoSlice<'_>]) -> io::Result<usize> {
    assert!(!packets.is_empty() && packets.len() <= BATCH);
    #[cfg(target_os = "linux")]
    {
        let mut vectors: [libc::iovec; BATCH] = unsafe { std::mem::zeroed() };
        let mut messages: [libc::mmsghdr; BATCH] = unsafe { std::mem::zeroed() };
        for (index, packet) in packets.iter().enumerate() {
            vectors[index].iov_base = packet.as_ptr().cast_mut().cast();
            vectors[index].iov_len = packet.len();
            messages[index].msg_hdr.msg_iov = &mut vectors[index];
            messages[index].msg_hdr.msg_iovlen = 1;
        }
        loop {
            // Connected peer FD: no destination can be substituted in msg_name.
            // Kernel reads each live packet and writes only message lengths.
            let count = unsafe {
                libc::sendmmsg(
                    fd,
                    messages.as_mut_ptr(),
                    packets.len() as _,
                    libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                )
            };
            if count > 0 {
                return Ok(count as usize);
            }
            if count == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let packet = &packets[0];
        let count = loop {
            let count =
                unsafe { libc::send(fd, packet.as_ptr().cast(), packet.len(), libc::MSG_DONTWAIT) };
            if count >= 0 {
                break count;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        };
        if count as usize != packet.len() {
            return Err(io::ErrorKind::WriteZero.into());
        }
        Ok(1)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixDatagram as StdDatagram;

    #[tokio::test]
    async fn receive_keeps_packet_boundaries_sender_paths_and_truncation() {
        let directory = tempfile::tempdir().unwrap();
        let receiver = UnixDatagram::bind(directory.path().join("rx")).unwrap();
        let a_path = directory.path().join("a");
        let b_path = directory.path().join("b");
        let a = StdDatagram::bind(&a_path).unwrap();
        let b = StdDatagram::bind(&b_path).unwrap();
        for socket in [&a, &b] {
            socket.connect(directory.path().join("rx")).unwrap();
        }
        a.send(&[1; 50]).unwrap();
        b.send(&[2; 100]).unwrap();
        a.send(&vec![3; CAPACITY * 2]).unwrap();
        b.send(&[]).unwrap();
        receiver.readable().await.unwrap();
        let mut batch = ReceiveBatch::new();
        batch.fill(&receiver).unwrap();
        assert_eq!(batch.count, 4);
        assert_eq!(batch.next(), (&[1; 50][..], Some(a_path.as_path())));
        assert_eq!(batch.next(), (&[2; 100][..], Some(b_path.as_path())));
        let (bytes, path) = batch.next();
        assert_eq!(bytes.len(), CAPACITY);
        assert_eq!(path, Some(a_path.as_path()));
        assert_eq!(batch.next(), (&[][..], Some(b_path.as_path())));
        assert!(batch.empty());
        assert_eq!(
            batch.fill(&receiver).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn partial_send_counts_complete_datagrams_without_replaying_them() {
        let (sender, receiver) = StdDatagram::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        let bytes: libc::c_int = 1024;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    sender.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&bytes as *const libc::c_int).cast(),
                    std::mem::size_of_val(&bytes) as _,
                )
            },
            0
        );
        let data: Vec<_> = (0..BATCH).map(|index| vec![index as u8; 1514]).collect();
        let slices: Vec<_> = data.iter().map(|bytes| io::IoSlice::new(bytes)).collect();
        let mut sent = 0;
        let mut arrived = 0;
        let mut blocked = false;
        while sent < BATCH {
            match send(sender.as_raw_fd(), &slices[sent..]) {
                Ok(count) => sent += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    blocked = true;
                    loop {
                        let mut packet = [0; 1514];
                        match receiver.recv(&mut packet) {
                            Ok(length) => {
                                assert_eq!(length, 1514);
                                assert_eq!(packet, [arrived as u8; 1514]);
                                arrived += 1;
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                            error => panic!("receive: {error:?}"),
                        }
                    }
                }
                error => panic!("send: {error:?}"),
            }
        }
        while arrived < BATCH {
            let mut packet = [0; 1514];
            assert_eq!(receiver.recv(&mut packet).unwrap(), 1514);
            assert_eq!(packet, [arrived as u8; 1514]);
            arrived += 1;
        }
        assert!(blocked);
    }
}
