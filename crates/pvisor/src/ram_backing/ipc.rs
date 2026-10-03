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

/// A connection-owned reference, not an object ID that grants global access.
pub struct RemoteObject {
    session: [u8; 16],
    token: u64,
    id: ImageId,
    length: usize,
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
    stream.write_all(&[1])?;
    stream.write_all(&(length as u32).to_le_bytes())?;
    stream.write_all(&bytes[..length])
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
    mut stream: UnixStream,
    pool: Arc<Mutex<CompressedPool>>,
    max_references: usize,
) -> io::Result<()> {
    if max_references == 0 {
        return Err(invalid("invalid pool reference budget"));
    }
    let session = *uuid::Uuid::new_v4().as_bytes();
    stream.write_all(MAGIC)?;
    stream.write_all(&session)?;
    let mut references: BTreeMap<u64, Arc<CompressedObject>> = BTreeMap::new();
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
                PUT => {
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
                    let interned = pool
                        .lock()
                        .map_err(|_| io::Error::other("pool lock poisoned"))?
                        .intern(&bytes);
                    let object = match interned {
                        Ok(object) => object,
                        Err(error) => {
                            reject(&mut stream, &error.to_string())?;
                            continue;
                        }
                    };
                    next += 1;
                    let id = object.id();
                    references.insert(next, object);
                    // Reference remains owned even if this response fails; session cleanup releases it.
                    stream.write_all(&[0])?;
                    stream.write_all(&next.to_le_bytes())?;
                    stream.write_all(&id)?;
                    stream.write_all(&(size as u32).to_le_bytes())?;
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
                    pool.lock()
                        .map_err(|_| io::Error::other("pool lock poisoned"))?
                        .collect_one(id);
                    stream.write_all(&[0])?;
                }
                STATS => {
                    let mut local = BTreeMap::<ImageId, (usize, &Arc<CompressedObject>)>::new();
                    for object in references.values() {
                        local.entry(object.id()).or_insert((0, object)).0 += 1;
                    }
                    let cross_session = local
                        .values()
                        .filter(|(count, object)| Arc::strong_count(object) > 1 + count)
                        .count() as u64;
                    let values = {
                        let pool = pool
                            .lock()
                            .map_err(|_| io::Error::other("pool lock poisoned"))?;
                        [
                            pool.encoded_bytes() as u64,
                            pool.object_count() as u64,
                            references.len() as u64,
                            cross_session,
                        ]
                    };
                    stream.write_all(&[0])?;
                    for value in values {
                        stream.write_all(&value.to_le_bytes())?;
                    }
                }
                _ => return Err(invalid("unknown pool operation")),
            }
        }
    })();
    drop(references);
    if let Ok(mut pool) = pool.lock() {
        pool.collect();
    }
    result
}

/// Serialized RPCs. Transport/protocol failure permanently closes the session;
/// a fully consumed rejection preserves all previously established references.
pub struct PoolClient {
    stream: Option<UnixStream>,
    session: [u8; 16],
    timeout: Duration,
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
            session,
            timeout,
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
        if bytes.is_empty() || bytes.len() > BLOCK_BYTES {
            return Err(invalid("invalid pool input length"));
        }
        let session = self.session;
        let expected = identity(bytes);
        self.exchange(|stream| {
            stream.write_all(&[PUT])?;
            stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
            stream.write_all(bytes)?;
            if let Some(error) = status(stream)? {
                return Ok(Err(error));
            }
            let token = read_u64(stream)?;
            let mut id = [0; 32];
            stream.read_exact(&mut id)?;
            let length = read_u32(stream)? as usize;
            if token == 0 || id != expected || length != bytes.len() {
                return Err(invalid("invalid pool object reply"));
            }
            Ok(Ok(RemoteObject {
                session,
                token,
                id,
                length,
            }))
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
