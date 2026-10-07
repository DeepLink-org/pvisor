//! Experimental cross-process compressed pool over caller-authorized Unix streams.
//! The caller owns endpoint authentication, connection limits and service lifetime.
//! Encoded data lives once in the host pool; GET restores a bounded block. There
//! is no disk backing and no implicit VM eviction in this transport.
use super::{
    BLOCK_BYTES, ImageId, invalid,
    resident::{CompressedObject, CompressedPool, identity},
};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    os::fd::AsRawFd,
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const MAGIC: &[u8; 8] = b"PVMEMP2\0";
const PUT: u8 = 1;
const GET: u8 = 2;
const RELEASE: u8 = 3;
const STATS: u8 = 4;
const PUT_SHARED: u8 = 5;
const MAPPING: u8 = 6;
const PUT_DUPLICATE: u8 = 7;

/// A connection-owned reference, not an object ID that grants global access.
pub struct RemoteObject {
    session: Arc<[u8; 16]>,
    token: u64,
    id: ImageId,
    length: usize,
    // Offset + 1 leaves zero as the absent value without an extra tag word.
    offset: Option<std::num::NonZeroU64>,
}
impl RemoteObject {
    pub fn id(&self) -> ImageId {
        self.id
    }
    pub fn length(&self) -> usize {
        self.length
    }
}
#[derive(Debug, Clone, Copy)]
pub struct PoolStats {
    pub encoded_bytes: u64,
    pub objects: u64,
    pub session_references: u64,
    /// Objects held here with additional references outside this session.
    pub cross_session_objects: u64,
}
fn read_u32(stream: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    stream.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}
fn read_u64(stream: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0; 8];
    stream.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}
fn reject(stream: &mut UnixStream, message: &str) -> io::Result<()> {
    let bytes = message.as_bytes();
    let length = bytes.len().min(256);
    let mut frame = Vec::with_capacity(5 + length);
    frame.push(1);
    frame.extend_from_slice(&(length as u32).to_le_bytes());
    frame.extend_from_slice(&bytes[..length]);
    stream.write_all(&frame)
}
fn status(stream: &mut impl Read) -> io::Result<Option<io::Error>> {
    let mut byte = [0];
    stream.read_exact(&mut byte)?;
    match byte[0] {
        0 => Ok(None),
        1 => {
            let size = read_u32(stream)? as usize;
            if size > 256 {
                return Err(invalid("pool error frame too large"));
            }
            let mut message = vec![0; size];
            stream.read_exact(&mut message)?;
            let message =
                String::from_utf8(message).map_err(|_| invalid("invalid pool error text"))?;
            Ok(Some(io::Error::other(message)))
        }
        _ => Err(invalid("invalid pool reply status")),
    }
}

/// One service thread per authorized connection; callers bound connection count.
/// No idle expiry: disconnected sessions release references, quiet VMs retain them.
pub fn serve(
    stream: UnixStream,
    pool: Arc<Mutex<CompressedPool>>,
    max_references: usize,
) -> io::Result<()> {
    serve_inner(stream, Owner::Compressed(pool), max_references)
}
#[cfg(target_os = "linux")]
pub fn serve_shared(
    stream: UnixStream,
    pool: Arc<Mutex<super::shared::SharedPool>>,
    max_references: usize,
) -> io::Result<()> {
    serve_inner(stream, Owner::Shared(pool), max_references)
}

enum Owner {
    Compressed(Arc<Mutex<CompressedPool>>),
    #[cfg(target_os = "linux")]
    Shared(Arc<Mutex<super::shared::SharedPool>>),
}
enum Reference {
    Compressed(Arc<CompressedObject>),
    #[cfg(target_os = "linux")]
    Shared(Arc<super::shared::SharedObject>),
}
impl Reference {
    fn id(&self) -> ImageId {
        match self {
            Self::Compressed(o) => o.id(),
            #[cfg(target_os = "linux")]
            Self::Shared(o) => o.id(),
        }
    }
    fn length(&self) -> usize {
        match self {
            Self::Compressed(o) => o.length(),
            #[cfg(target_os = "linux")]
            Self::Shared(_) => 4096,
        }
    }
    fn restore(&self, out: &mut [u8]) -> io::Result<()> {
        match self {
            Self::Compressed(o) => o.restore(out),
            #[cfg(target_os = "linux")]
            Self::Shared(o) => o.restore(out),
        }
    }
    fn count(&self) -> usize {
        match self {
            Self::Compressed(o) => Arc::strong_count(o),
            #[cfg(target_os = "linux")]
            Self::Shared(o) => Arc::strong_count(o),
        }
    }
    fn offset(&self) -> Option<u64> {
        match self {
            Self::Compressed(_) => None,
            #[cfg(target_os = "linux")]
            Self::Shared(o) => Some(o.offset()),
        }
    }
}
impl Owner {
    fn is_shared(&self) -> bool {
        match self {
            Self::Compressed(_) => false,
            #[cfg(target_os = "linux")]
            Self::Shared(_) => true,
        }
    }
    fn intern(&self, bytes: &[u8], duplicate: bool, session: [u8; 16]) -> io::Result<Reference> {
        #[cfg(not(target_os = "linux"))]
        let _ = (duplicate, session);
        match self {
            Self::Compressed(pool) => pool
                .lock()
                .map_err(|_| io::Error::other("pool poisoned"))?
                .intern(bytes)
                .map(Reference::Compressed),
            #[cfg(target_os = "linux")]
            Self::Shared(pool) => {
                let mut pool = pool.lock().map_err(|_| io::Error::other("pool poisoned"))?;
                (if duplicate {
                    pool.intern_duplicate(bytes, session)
                } else {
                    pool.intern(bytes)
                })
                .map(Reference::Shared)
            }
        }
    }
    fn collect(&self, id: Option<ImageId>) -> io::Result<()> {
        match self {
            Self::Compressed(pool) => {
                let mut pool = pool.lock().map_err(|_| io::Error::other("pool poisoned"))?;
                if let Some(id) = id {
                    pool.collect_one(id);
                } else {
                    pool.collect();
                }
                Ok(())
            }
            #[cfg(target_os = "linux")]
            Self::Shared(pool) => {
                let mut pool = pool.lock().map_err(|_| io::Error::other("pool poisoned"))?;
                if let Some(id) = id {
                    pool.collect_one(id)
                } else {
                    pool.collect()
                }
            }
        }
    }
    fn stats(&self) -> io::Result<(u64, u64)> {
        match self {
            Self::Compressed(pool) => {
                let pool = pool.lock().map_err(|_| io::Error::other("pool poisoned"))?;
                Ok((pool.encoded_bytes() as u64, pool.object_count() as u64))
            }
            #[cfg(target_os = "linux")]
            Self::Shared(pool) => {
                let pool = pool.lock().map_err(|_| io::Error::other("pool poisoned"))?;
                Ok((pool.payload_bytes() as u64, pool.object_count() as u64))
            }
        }
    }
}
fn serve_inner(mut stream: UnixStream, pool: Owner, max_references: usize) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    let peer = if matches!(&pool, Owner::Shared(_)) {
        Some(super::shared::peer_pidfd(&stream)?)
    } else {
        None
    };
    if max_references == 0 {
        return Err(invalid("invalid pool reference budget"));
    }
    let session = *uuid::Uuid::new_v4().as_bytes();
    stream.write_all(MAGIC)?;
    stream.write_all(&session)?;
    let mut references: BTreeMap<u64, Reference> = BTreeMap::new();
    let result = (|| {
        let mut next = 0u64;
        loop {
            let mut operation = [0];
            match stream.read(&mut operation) {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
            match operation[0] {
                PUT | PUT_SHARED | PUT_DUPLICATE => {
                    let size = read_u32(&mut stream)? as usize;
                    if size == 0 || size > BLOCK_BYTES {
                        return Err(invalid("invalid pool block length"));
                    }
                    let mut bytes = vec![0; size];
                    stream.read_exact(&mut bytes)?;
                    if references.len() >= max_references || next == u64::MAX {
                        reject(&mut stream, "pool reference budget exhausted")?;
                        continue;
                    }
                    if operation[0] != PUT && !pool.is_shared() {
                        reject(&mut stream, "physical sharing unsupported by this pool")?;
                        continue;
                    }
                    let interned = pool.intern(&bytes, operation[0] == PUT_DUPLICATE, session);
                    let object = match interned {
                        Ok(object) => object,
                        Err(error) => {
                            reject(&mut stream, &error.to_string())?;
                            continue;
                        }
                    };
                    next += 1;
                    let id = object.id();
                    let offset = object.offset();
                    references.insert(next, object);
                    // Reference remains owned even if this response fails; session cleanup releases it.
                    let mut reply = Vec::with_capacity(53);
                    reply.push(0);
                    reply.extend_from_slice(&next.to_le_bytes());
                    reply.extend_from_slice(&id);
                    reply.extend_from_slice(&(size as u32).to_le_bytes());
                    if operation[0] != PUT {
                        reply.extend_from_slice(&offset.unwrap().to_le_bytes());
                    }
                    stream.write_all(&reply)?;
                }
                MAPPING => {
                    #[cfg(target_os = "linux")]
                    if let Owner::Shared(owner) = &pool {
                        let file = owner
                            .lock()
                            .map_err(|_| io::Error::other("pool poisoned"))?
                            .readonly_file();
                        stream.write_all(&[0])?;
                        send_mapping_fd(&stream, file.as_raw_fd())?;
                        continue;
                    }
                    reject(&mut stream, "physical sharing unsupported by this pool")?;
                }
                GET => {
                    let token = read_u64(&mut stream)?;
                    let Some(object) = references.get(&token) else {
                        reject(&mut stream, "unknown pool reference")?;
                        continue;
                    };
                    let mut bytes = vec![0; object.length()];
                    if let Err(error) = object.restore(&mut bytes) {
                        reject(&mut stream, &error.to_string())?;
                        continue;
                    }
                    stream.write_all(&[0])?;
                    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
                    stream.write_all(&bytes)?;
                }
                RELEASE => {
                    let token = read_u64(&mut stream)?;
                    let Some(object) = references.remove(&token) else {
                        reject(&mut stream, "unknown pool reference")?;
                        continue;
                    };
                    let id = object.id();
                    drop(object);
                    pool.collect(Some(id))?;
                    stream.write_all(&[0])?;
                }
                STATS => {
                    let mut local = BTreeMap::<ImageId, (usize, &Reference)>::new();
                    for object in references.values() {
                        local.entry(object.id()).or_insert((0, object)).0 += 1;
                    }
                    let cross_session = local
                        .values()
                        .filter(|(count, object)| object.count() > 1 + count)
                        .count() as u64;
                    let (payload, objects) = pool.stats()?;
                    let values = [payload, objects, references.len() as u64, cross_session];
                    stream.write_all(&[0])?;
                    for value in values {
                        stream.write_all(&value.to_le_bytes())?;
                    }
                }
                _ => return Err(invalid("unknown pool operation")),
            }
        }
    })();
    #[cfg(target_os = "linux")]
    if let Owner::Shared(owner) = &pool {
        owner
            .lock()
            .map_err(|_| io::Error::other("pool poisoned"))?
            .forget_candidates(session);
    }
    #[cfg(target_os = "linux")]
    if !references.is_empty()
        && let Some(peer) = peer
    {
        // Socket loss is not proof that private COW mappings have disappeared.
        // Hold every slot until the peer's pidfd proves the VM process exited.
        super::shared::wait_peer_exit(&peer).unwrap_or_else(|error| {
            eprintln!("cannot safely release shared RAM ownership: {error}");
            std::process::abort();
        });
    }
    drop(references);
    pool.collect(None)?;
    result
}

#[cfg(target_os = "linux")]
fn send_mapping_fd(stream: &UnixStream, fd: std::os::fd::RawFd) -> io::Result<()> {
    let mut marker = [0x46u8];
    let mut iov = libc::iovec {
        iov_base: marker.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 4];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = unsafe { libc::CMSG_SPACE(4) } as usize;
    unsafe {
        let c = libc::CMSG_FIRSTHDR(&message);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(4) as usize;
        std::ptr::write_unaligned(libc::CMSG_DATA(c).cast(), fd);
        if libc::sendmsg(stream.as_raw_fd(), &message, libc::MSG_NOSIGNAL) != 1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Serialized RPCs. Transport/protocol failure permanently closes the session;
/// a fully consumed rejection preserves all previously established references.
pub struct PoolClient {
    stream: Option<UnixStream>,
    session: Arc<[u8; 16]>,
    timeout: Duration,
    shared_file: Option<Arc<std::fs::File>>,
}
// One absolute deadline covers partial writes and trickling replies. Nonblocking
// I/O plus poll avoids relying on per-syscall Unix socket timeout semantics.
struct DeadlineStream<'a> {
    stream: &'a mut UnixStream,
    deadline: Instant,
}
impl<'a> DeadlineStream<'a> {
    fn new(stream: &'a mut UnixStream, timeout: Duration) -> io::Result<Self> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| invalid("pool timeout overflow"))?;
        Ok(Self { stream, deadline })
    }
    #[cfg(target_os = "linux")]
    fn receive_mapping_fd(&mut self) -> io::Result<std::os::fd::OwnedFd> {
        use std::os::fd::{FromRawFd, OwnedFd, RawFd};
        self.io(libc::POLLIN, |stream| {
            let mut marker = [0u8];
            let mut control = [0usize; 4];
            let mut iov = libc::iovec {
                iov_base: marker.as_mut_ptr().cast(),
                iov_len: 1,
            };
            let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
            message.msg_iov = &mut iov;
            message.msg_iovlen = 1;
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen = std::mem::size_of_val(&control);
            let n =
                unsafe { libc::recvmsg(stream.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut fds = Vec::new();
            let mut bad = false;
            unsafe {
                let mut c = libc::CMSG_FIRSTHDR(&message);
                while !c.is_null() {
                    if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                        let size = (*c).cmsg_len.saturating_sub(libc::CMSG_LEN(0) as usize);
                        bad |= (*c).cmsg_len < libc::CMSG_LEN(0) as usize
                            || !size.is_multiple_of(std::mem::size_of::<RawFd>());
                        for i in 0..size / std::mem::size_of::<RawFd>() {
                            fds.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(
                                libc::CMSG_DATA(c).cast::<RawFd>().add(i),
                            )));
                        }
                    } else {
                        bad = true;
                    }
                    c = libc::CMSG_NXTHDR(&message, c);
                }
            }
            // Adopt all installed rights before validation so malformed frames
            // cannot leak descriptors, including a truncated ancillary prefix.
            if n != 1
                || marker[0] != 0x46
                || bad
                || message.msg_flags & libc::MSG_CTRUNC != 0
                || fds.len() != 1
            {
                return Err(invalid("invalid shared pool descriptor frame"));
            }
            Ok(fds.remove(0))
        })
    }
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "pool exchange deadline exceeded")
            })
    }
    fn io<T>(
        &mut self,
        events: i16,
        mut action: impl FnMut(&mut UnixStream) -> io::Result<T>,
    ) -> io::Result<T> {
        loop {
            self.remaining()?;
            match action(self.stream) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let mut fd = libc::pollfd {
                        fd: self.stream.as_raw_fd(),
                        events,
                        revents: 0,
                    };
                    let ms = self
                        .remaining()?
                        .as_millis()
                        .saturating_add(1)
                        .min(i32::MAX as u128) as i32;
                    let result = unsafe { libc::poll(&mut fd, 1, ms) };
                    if result < 0 {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::Interrupted {
                            return Err(error);
                        }
                    }
                }
                result => return result,
            }
        }
    }
}
impl Read for DeadlineStream<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.io(libc::POLLIN, |stream| stream.read(bytes))
    }
}
impl Write for DeadlineStream<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.io(libc::POLLOUT, |stream| stream.write(bytes))
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
impl PoolClient {
    pub fn new(mut stream: UnixStream, timeout: Duration) -> io::Result<Self> {
        if timeout.is_zero() {
            return Err(invalid("invalid pool timeout"));
        }
        stream.set_nonblocking(true)?;
        let mut magic = [0; 8];
        let mut session = [0; 16];
        let mut handshake = DeadlineStream::new(&mut stream, timeout)?;
        handshake.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(invalid("invalid pool protocol"));
        }
        handshake.read_exact(&mut session)?;
        handshake.remaining()?;
        Ok(Self {
            stream: Some(stream),
            session: Arc::new(session),
            timeout,
            shared_file: None,
        })
    }
    fn exchange<T>(
        &mut self,
        call: impl FnOnce(&mut DeadlineStream<'_>) -> io::Result<io::Result<T>>,
    ) -> io::Result<T> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| io::Error::other("pool session has failed"))?;
        let result = DeadlineStream::new(stream, self.timeout).and_then(|mut deadline| {
            let result = call(&mut deadline)?;
            deadline.remaining()?;
            Ok(result)
        });
        match result {
            Ok(result) => result,
            Err(error) => {
                self.stream.take();
                Err(error)
            }
        }
    }
    pub fn put(&mut self, bytes: &[u8]) -> io::Result<RemoteObject> {
        self.put_inner(bytes, false)
    }
    pub fn put_duplicate(&mut self, bytes: &[u8]) -> io::Result<RemoteObject> {
        self.put_inner(bytes, self.shared_file.is_some())
    }
    fn put_inner(&mut self, bytes: &[u8], duplicate: bool) -> io::Result<RemoteObject> {
        if bytes.is_empty() || bytes.len() > BLOCK_BYTES {
            return Err(invalid("invalid pool input length"));
        }
        let session = self.session.clone();
        let expected = identity(bytes);
        let shared_file = self.shared_file.clone();
        if shared_file.is_some() && bytes.len() != 4096 {
            return Err(invalid("shared pool requires 4 KiB pages"));
        }
        self.exchange(|stream| {
            let operation = if duplicate {
                PUT_DUPLICATE
            } else if shared_file.is_some() {
                PUT_SHARED
            } else {
                PUT
            };
            let mut frame = Vec::with_capacity(5 + bytes.len());
            frame.push(operation);
            frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            frame.extend_from_slice(bytes);
            stream.write_all(&frame)?;
            if let Some(error) = status(stream)? {
                // The complete rejection frame proves the connection is healthy.
                return Ok(Err(io::Error::new(io::ErrorKind::WouldBlock, error)));
            }
            let token = read_u64(stream)?;
            let mut id = [0; 32];
            stream.read_exact(&mut id)?;
            let length = read_u32(stream)? as usize;
            if token == 0 || id != expected || length != bytes.len() {
                return Err(invalid("invalid pool object reply"));
            }
            let offset = if let Some(file) = &shared_file {
                let offset = read_u64(stream)?;
                let file_len = file.metadata()?.len();
                if !offset.is_multiple_of(4096)
                    || offset.checked_add(4096).is_none_or(|end| end > file_len)
                {
                    return Err(invalid("invalid shared page offset"));
                }
                use std::os::unix::fs::FileExt;
                let mut actual = [0; 4096];
                file.read_exact_at(&mut actual, offset)?;
                if actual != bytes {
                    return Err(invalid("shared page publication content mismatch"));
                }
                std::num::NonZeroU64::new(offset + 1)
            } else {
                None
            };
            Ok(Ok(RemoteObject {
                session,
                token,
                id,
                length,
                offset,
            }))
        })
    }
    #[cfg(target_os = "linux")]
    pub fn enable_shared_mapping(&mut self) -> io::Result<()> {
        let file = self.exchange(|stream| {
            stream.write_all(&[MAPPING])?;
            if let Some(error) = status(stream)? {
                return Ok(Err(error));
            }
            let fd = stream.receive_mapping_fd()?;
            let access = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
            let seals = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GET_SEALS) };
            if access < 0
                || access & libc::O_ACCMODE != libc::O_RDONLY
                || seals < 0
                || seals & (libc::F_SEAL_GROW | libc::F_SEAL_SHRINK)
                    != libc::F_SEAL_GROW | libc::F_SEAL_SHRINK
            {
                return Err(invalid(
                    "pool mapping descriptor must be read-only and size-sealed",
                ));
            }
            let file = std::fs::File::from(fd);
            if file.metadata()?.len() == 0 || !file.metadata()?.len().is_multiple_of(4096) {
                return Err(invalid("invalid shared pool file size"));
            }
            Ok(Ok(Arc::new(file)))
        })?;
        self.shared_file = Some(file);
        Ok(())
    }
    pub fn shared_mapping(
        &self,
        object: &RemoteObject,
    ) -> Option<pvisor_vm::api::SharedRamMapping> {
        if self.owns(object).is_err() {
            return None;
        }
        Some(pvisor_vm::api::SharedRamMapping {
            file: self.shared_file.as_ref()?.clone(),
            offset: object.offset?.get() - 1,
        })
    }
    fn owns(&self, object: &RemoteObject) -> io::Result<()> {
        if object.session != self.session {
            return Err(invalid("pool reference belongs to another session"));
        }
        Ok(())
    }
    pub fn restore(&mut self, object: &RemoteObject, output: &mut [u8]) -> io::Result<()> {
        self.owns(object)?;
        if output.len() != object.length {
            return Err(invalid("pool output length mismatch"));
        }
        self.exchange(|stream| {
            stream.write_all(&[GET])?;
            stream.write_all(&object.token.to_le_bytes())?;
            if let Some(error) = status(stream)? {
                return Ok(Err(error));
            }
            if read_u32(stream)? as usize != output.len() {
                return Err(invalid("invalid pool restore length"));
            }
            stream.read_exact(output)?;
            if identity(output) != object.id {
                return Err(invalid("pool restore checksum mismatch"));
            }
            Ok(Ok(()))
        })
    }
    /// Release only after all guest pages referring to this handle are restored
    /// or intentionally discarded. Other PUTs of identical content remain valid.
    pub fn release(&mut self, object: RemoteObject) -> io::Result<()> {
        self.owns(&object)?;
        self.exchange(|stream| {
            stream.write_all(&[RELEASE])?;
            stream.write_all(&object.token.to_le_bytes())?;
            Ok(match status(stream)? {
                Some(error) => Err(error),
                None => Ok(()),
            })
        })
    }
    pub fn stats(&mut self) -> io::Result<PoolStats> {
        self.exchange(|stream| {
            stream.write_all(&[STATS])?;
            if let Some(error) = status(stream)? {
                return Ok(Err(error));
            }
            Ok(Ok(PoolStats {
                encoded_bytes: read_u64(stream)?,
                objects: read_u64(stream)?,
                session_references: read_u64(stream)?,
                cross_session_objects: read_u64(stream)?,
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_page_authority_uses_at_most_64_bytes() {
        // A full 512 MiB guest can reference 131072 pages. Keep its per-page
        // authority budget at 8 MiB, with session identity held once per client.
        assert!(std::mem::size_of::<RemoteObject>() <= 64);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn physical_pool_maps_one_page_and_cow_keeps_other_references_unchanged() {
        let pool = Arc::new(Mutex::new(
            super::super::shared::SharedPool::new(8192, 2).unwrap(),
        ));
        let (a, sa) = UnixStream::pair().unwrap();
        let (b, sb) = UnixStream::pair().unwrap();
        let owner = pool.clone();
        let ta = std::thread::spawn(move || serve_shared(sa, owner, 8));
        let owner = pool.clone();
        let tb = std::thread::spawn(move || serve_shared(sb, owner, 8));
        let mut a = PoolClient::new(a, Duration::from_secs(5)).unwrap();
        a.enable_shared_mapping().unwrap();
        let mut b = PoolClient::new(b, Duration::from_secs(5)).unwrap();
        b.enable_shared_mapping().unwrap();
        assert!(a.put_duplicate(&[0x53; 4096]).is_err());
        assert_eq!(a.stats().unwrap().objects, 0);
        let br = b.put_duplicate(&[0x53; 4096]).unwrap();
        let ar = a.put_duplicate(&[0x53; 4096]).unwrap();
        let am = a.shared_mapping(&ar).unwrap();
        let bm = b.shared_mapping(&br).unwrap();
        assert_eq!(am.offset, bm.offset);
        assert_eq!(a.stats().unwrap().encoded_bytes, 4096);
        assert_eq!(a.stats().unwrap().cross_session_objects, 1);
        let map = |m: &pvisor_vm::api::SharedRamMapping| unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                4096,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE,
                m.file.as_raw_fd(),
                m.offset as libc::off_t,
            )
        };
        let ap = map(&am);
        let bp = map(&bm);
        assert_ne!(ap, libc::MAP_FAILED);
        assert_ne!(bp, libc::MAP_FAILED);
        unsafe {
            assert_eq!(*(bp as *const u8), 0x53);
            *(ap as *mut u8) = 0x91;
            assert_eq!(*(bp as *const u8), 0x53);
            assert_eq!(*(ap as *const u8), 0x91);
            libc::munmap(ap, 4096);
            libc::munmap(bp, 4096);
        }
        a.release(ar).unwrap();
        let mut out = [0; 4096];
        b.restore(&br, &mut out).unwrap();
        assert_eq!(out, [0x53; 4096]);
        b.release(br).unwrap();
        drop(a);
        drop(b);
        ta.join().unwrap().unwrap();
        tb.join().unwrap().unwrap();
        assert_eq!(pool.lock().unwrap().object_count(), 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn disconnected_mapping_is_pinned_until_peer_process_exits() {
        use std::io::BufRead;
        const CHILD: &str = "PVISOR_SHARED_LIFETIME_CHILD";
        if let Some(socket) = std::env::var_os(CHILD) {
            let mut client =
                PoolClient::new(UnixStream::connect(socket).unwrap(), Duration::from_secs(5))
                    .unwrap();
            client.enable_shared_mapping().unwrap();
            let object = client.put(&[7; 4096]).unwrap();
            let mapping = client.shared_mapping(&object).unwrap();
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    4096,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE,
                    mapping.file.as_raw_fd(),
                    mapping.offset as libc::off_t,
                )
            };
            assert_ne!(ptr, libc::MAP_FAILED);
            unsafe {
                assert_eq!(*(ptr as *const u8), 7);
            }
            drop(client);
            println!("ready");
            std::io::stdout().flush().unwrap();
            let mut command = [0];
            std::io::stdin().read_exact(&mut command).unwrap();
            unsafe {
                assert_eq!(*(ptr as *const u8), 7);
                libc::munmap(ptr, 4096);
            }
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("pool.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let pool = Arc::new(Mutex::new(
            super::super::shared::SharedPool::new(4096, 1).unwrap(),
        ));
        let owner = pool.clone();
        let server = std::thread::spawn(move || {
            let mut workers = Vec::new();
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                let owner = owner.clone();
                workers.push(std::thread::spawn(move || serve_shared(stream, owner, 8)));
            }
            for worker in workers {
                worker.join().unwrap().unwrap();
            }
        });
        let name = module_path!().split_once("::").unwrap().1.to_owned()
            + "::disconnected_mapping_is_pinned_until_peer_process_exits";
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture"])
            .env(CHILD, &socket)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert!(output.read_line(&mut line).unwrap() > 0);
            if line.trim() == "ready" {
                break;
            }
        }
        let mut observer = PoolClient::new(
            UnixStream::connect(&socket).unwrap(),
            Duration::from_secs(5),
        )
        .unwrap();
        observer.enable_shared_mapping().unwrap();
        assert_eq!(observer.stats().unwrap().objects, 1);
        assert!(observer.put(&[9; 4096]).is_err());
        child.stdin.take().unwrap().write_all(&[1]).unwrap();
        assert!(child.wait().unwrap().success());
        let deadline = Instant::now() + Duration::from_secs(2);
        while observer.stats().unwrap().objects != 0 {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let object = observer.put(&[9; 4096]).unwrap();
        let mut bytes = [0; 4096];
        observer.restore(&object, &mut bytes).unwrap();
        assert_eq!(bytes, [9; 4096]);
        observer.release(object).unwrap();
        drop(observer);
        server.join().unwrap();
    }
    #[test]
    fn trickling_reply_cannot_extend_exchange_deadline() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            server.write_all(MAGIC).unwrap();
            server.write_all(&[1; 16]).unwrap();
            let mut operation = [0];
            server.read_exact(&mut operation).unwrap();
            assert_eq!(operation[0], STATS);
            server.write_all(&[0]).unwrap();
            for _ in 0..32 {
                if server.write_all(&[0]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        });
        let mut client = PoolClient::new(client, Duration::from_millis(200)).unwrap();
        let started = Instant::now();
        assert_eq!(client.stats().unwrap_err().kind(), io::ErrorKind::TimedOut);
        println!(
            "pool-trickle deadline_ms=200 elapsed_us={}",
            started.elapsed().as_micros()
        );
        assert!(client.stream.is_none());
        assert!(client.stats().is_err());
        worker.join().unwrap();
    }
    #[test]
    fn truncated_reply_permanently_closes_session() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            server.write_all(MAGIC).unwrap();
            server.write_all(&[1; 16]).unwrap();
            let mut operation = [0];
            server.read_exact(&mut operation).unwrap();
            assert_eq!(operation[0], STATS);
            server.write_all(&[0]).unwrap();
            server.write_all(&[0; 3]).unwrap(); // Incomplete u64; EOF must not be reused.
        });
        let mut client = PoolClient::new(client, Duration::from_secs(5)).unwrap();
        assert!(client.stats().is_err());
        assert!(client.stream.is_none());
        assert_eq!(
            client.stats().unwrap_err().to_string(),
            "pool session has failed"
        );
        worker.join().unwrap();
    }

    #[test]
    fn independent_references_budget_rejection_and_disconnect_cleanup() {
        let pool = Arc::new(Mutex::new(CompressedPool::new(1024, 4)));
        let (a, sa) = UnixStream::pair().unwrap();
        let (b, sb) = UnixStream::pair().unwrap();
        let owner = pool.clone();
        let ta = std::thread::spawn(move || serve(sa, owner, 2));
        let owner = pool.clone();
        let tb = std::thread::spawn(move || serve(sb, owner, 2));
        let mut a = PoolClient::new(a, Duration::from_secs(5)).unwrap();
        let mut b = PoolClient::new(b, Duration::from_secs(5)).unwrap();
        let bytes = vec![17; 16384];
        let ar = a.put(&bytes).unwrap();
        let ar2 = a.put(&bytes).unwrap();
        let br = b.put(&bytes).unwrap();
        assert_eq!(ar.id(), br.id());
        assert_eq!(a.stats().unwrap().encoded_bytes, 1);
        assert_eq!(a.stats().unwrap().cross_session_objects, 1);
        assert!(a.put(&bytes).is_err()); // A valid rejection does not destroy other references.
        let mut out = vec![0; bytes.len()];
        assert!(a.restore(&br, &mut out).is_err());
        a.release(ar).unwrap();
        a.restore(&ar2, &mut out).unwrap();
        assert_eq!(out, bytes);
        drop(a);
        ta.join().unwrap().unwrap();
        b.restore(&br, &mut out).unwrap();
        assert_eq!(out, bytes);
        assert_eq!(b.stats().unwrap().objects, 1);
        b.release(br).unwrap();
        assert_eq!(b.stats().unwrap().encoded_bytes, 0);
        drop(b);
        tb.join().unwrap().unwrap();
        assert_eq!(pool.lock().unwrap().object_count(), 0);
    }
}
