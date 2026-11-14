//! Experimental cross-process compressed pool over caller-authorized Unix streams.
//! The caller owns endpoint authentication, connection limits and service lifetime.
//! Legacy GET restores a bounded block; Linux sealed clients own compressed
//! memfds and decode locally. No disk backing or implicit VM eviction here.
#[cfg(target_os = "linux")]
#[path = "sealed_client.rs"]
mod sealed_client;
use super::{
    BLOCK_BYTES, ImageId, invalid,
    resident::{CompressedObject, CompressedPool, identity},
    shared_index::fingerprint,
};
#[cfg(target_os = "linux")]
pub use sealed_client::{SealedClient, SealedReference};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::{self, Read, Write},
    os::fd::AsRawFd,
    os::unix::net::UnixStream,
    sync::{Arc, Mutex, Weak},
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
const SEALED: u8 = 8;
const PUT_SEALED: u8 = 9;
const DISCOVERY: u8 = 10;
// Retired discovery operations 11/12 must never be reinterpreted.
const PUT_BATCH: u8 = 13;
const RELEASE_BATCH: u8 = 14;
const DISCOVER_XXH3: u8 = 15;
const SHARED_STATS: u8 = 16;
const DISCOVERY_VERSION: u32 = 3;
const DISCOVERY_BATCH: usize = 256;
const MAX_BATCH_REPLY: usize = 96 * 1024;

use pvisor_vm::api::{ColdRamDisposition as Admission, ColdRamSample};

/// A connection-owned reference, not an object ID that grants global access.
pub struct RemoteObject {
    session: Arc<[u8; 16]>,
    token: u64,
    id: ImageId,
    length: usize,
    // Offset + 1 leaves zero as the absent value without an extra tag word.
    offset: Option<std::num::NonZeroU64>,
}
enum Discovered {
    Complete(io::Result<Admission<RemoteObject>>),
    Upload,
    Collision(RemoteObject),
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

fn write_batch_reply(stream: &mut UnixStream, payload: &[u8]) -> io::Result<()> {
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(0);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    stream.write_all(&frame)
}

fn read_batch_reply(stream: &mut impl Read) -> io::Result<io::Result<Vec<u8>>> {
    if let Some(error) = status(stream)? {
        return Ok(Err(error));
    }
    let size = read_u32(stream)? as usize;
    if size > MAX_BATCH_REPLY {
        return Err(invalid("physical batch reply too large"));
    }
    let mut payload = vec![0; size];
    stream.read_exact(&mut payload)?;
    Ok(Ok(payload))
}

fn append_ready(reply: &mut Vec<u8>, ready: Vec<u64>) {
    reply.extend_from_slice(&(ready.len() as u32).to_le_bytes());
    for page in ready {
        reply.extend_from_slice(&page.to_le_bytes());
    }
}

fn read_ready(stream: &mut impl Read) -> io::Result<Vec<u64>> {
    let count = read_u32(stream)? as usize;
    if count > DISCOVERY_BATCH {
        return Err(invalid("too many ready hints"));
    }
    let mut ready = Vec::with_capacity(count);
    for _ in 0..count {
        let page = read_u64(stream)?;
        if page == 0 {
            return Err(invalid("zero ready handle"));
        }
        ready.push(page);
    }
    Ok(ready)
}

fn append_object(
    reply: &mut Vec<u8>,
    object: Reference,
    references: &mut BTreeMap<u64, Reference>,
    counts: &mut BTreeMap<ImageId, LocalReferenceCount>,
    next: &mut u64,
) {
    let offset = object.offset().expect("physical object");
    let id = object.id();
    let token = acquire_reference(references, counts, next, object);
    reply.push(2);
    reply.extend_from_slice(&token.to_le_bytes());
    reply.extend_from_slice(&offset.to_le_bytes());
    reply.extend_from_slice(&id);
}

fn release_reference(
    token: u64,
    references: &mut BTreeMap<u64, Reference>,
    counts: &mut BTreeMap<ImageId, LocalReferenceCount>,
    pool: &Owner,
) -> io::Result<()> {
    let object = references.remove(&token).expect("preflighted reference");
    let id = object.id();
    let entry = counts.get_mut(&id).expect("reference count must exist");
    entry.count -= 1;
    if entry.count == 0 {
        counts.remove(&id);
    }
    drop(object);
    pool.collect(Some(id))
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
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn serve_shared(
    stream: UnixStream,
    pool: Arc<Mutex<super::shared::SharedPool>>,
    max_references: usize,
) -> io::Result<()> {
    serve_inner(stream, Owner::Shared(pool), max_references)
}

/// Serve immutable compressed objects to authorized Linux clients. Outstanding
/// references on disconnect stay budgeted until pidfd proves peer exit; client
/// descriptors remain valid even if this service exits. No socket reconnection.
#[cfg(target_os = "linux")]
pub fn serve_sealed(
    stream: UnixStream,
    pool: Arc<Mutex<super::sealed::SealedPool>>,
    max_references: usize,
) -> io::Result<()> {
    serve_inner(stream, Owner::Sealed(pool), max_references)
}

enum Owner {
    Compressed(Arc<Mutex<CompressedPool>>),
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Shared(Arc<Mutex<super::shared::SharedPool>>),
    #[cfg(target_os = "linux")]
    Sealed(Arc<Mutex<super::sealed::SealedPool>>),
}
enum Reference {
    Compressed(Arc<CompressedObject>),
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Shared(Arc<super::shared::SharedObject>),
    #[cfg(target_os = "linux")]
    Sealed(Arc<super::sealed::SealedObject>),
}
// One weak observer per distinct live object in this connection. Weak owners
// do not pin slots or alter the strong-reference sharing/collection contract.
enum ObservedReference {
    Compressed(Weak<CompressedObject>),
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Shared(Weak<super::shared::SharedObject>),
    #[cfg(target_os = "linux")]
    Sealed(Weak<super::sealed::SealedObject>),
}
impl ObservedReference {
    fn strong_count(&self) -> usize {
        match self {
            Self::Compressed(o) => o.strong_count(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Shared(o) => o.strong_count(),
            #[cfg(target_os = "linux")]
            Self::Sealed(o) => o.strong_count(),
        }
    }
}
struct LocalReferenceCount {
    count: usize,
    object: ObservedReference,
}
fn acquire_reference(
    references: &mut BTreeMap<u64, Reference>,
    counts: &mut BTreeMap<ImageId, LocalReferenceCount>,
    next: &mut u64,
    object: Reference,
) -> u64 {
    *next += 1;
    counts
        .entry(object.id())
        .and_modify(|entry| entry.count += 1)
        .or_insert_with(|| LocalReferenceCount {
            count: 1,
            object: object.observe(),
        });
    references.insert(*next, object);
    *next
}
impl Reference {
    fn id(&self) -> ImageId {
        match self {
            Self::Compressed(o) => o.id(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Shared(o) => o.id(),
            #[cfg(target_os = "linux")]
            Self::Sealed(o) => o.id(),
        }
    }
    fn length(&self) -> usize {
        match self {
            Self::Compressed(o) => o.length(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Shared(_) => super::shared::PAGE,
            #[cfg(target_os = "linux")]
            Self::Sealed(o) => o.length(),
        }
    }
    fn restore(&self, out: &mut [u8]) -> io::Result<()> {
        match self {
            Self::Compressed(o) => o.restore(out),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Shared(o) => o.restore(out),
            #[cfg(target_os = "linux")]
            Self::Sealed(o) => o.restore(out),
        }
    }
    fn observe(&self) -> ObservedReference {
        match self {
            Self::Compressed(o) => ObservedReference::Compressed(Arc::downgrade(o)),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Shared(o) => ObservedReference::Shared(Arc::downgrade(o)),
            #[cfg(target_os = "linux")]
            Self::Sealed(o) => ObservedReference::Sealed(Arc::downgrade(o)),
        }
    }
    fn offset(&self) -> Option<u64> {
        match self {
            Self::Compressed(_) => None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Shared(o) => Some(o.offset()),
            #[cfg(target_os = "linux")]
            Self::Sealed(_) => None,
        }
    }
}
impl Owner {
    fn is_shared(&self) -> bool {
        match self {
            Self::Compressed(_) => false,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Shared(_) => true,
            #[cfg(target_os = "linux")]
            Self::Sealed(_) => false,
        }
    }
    fn is_sealed(&self) -> bool {
        #[cfg(target_os = "linux")]
        if matches!(self, Self::Sealed(_)) {
            return true;
        }
        false
    }
    fn intern(&self, bytes: &[u8], duplicate: bool, session: [u8; 16]) -> io::Result<Reference> {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let _ = (duplicate, session);
        match self {
            Self::Compressed(pool) => pool
                .lock()
                .map_err(|_| io::Error::other("pool poisoned"))?
                .intern(bytes)
                .map(Reference::Compressed),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Shared(pool) => {
                let mut pool = pool.lock().map_err(|_| io::Error::other("pool poisoned"))?;
                (if duplicate {
                    pool.intern_duplicate(bytes, session)
                } else {
                    pool.intern(bytes)
                })
                .map(Reference::Shared)
            }
            #[cfg(target_os = "linux")]
            Self::Sealed(pool) => pool
                .lock()
                .map_err(|_| io::Error::other("pool poisoned"))?
                .intern(bytes)
                .map(Reference::Sealed),
        }
    }
    fn collect(&self, id: Option<ImageId>) -> io::Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Sealed(pool) => {
                let mut pool = pool.lock().map_err(|_| io::Error::other("pool poisoned"))?;
                if let Some(id) = id {
                    pool.collect_one(id);
                } else {
                    pool.collect();
                }
                Ok(())
            }
            Self::Compressed(pool) => {
                let mut pool = pool.lock().map_err(|_| io::Error::other("pool poisoned"))?;
                if let Some(id) = id {
                    pool.collect_one(id);
                } else {
                    pool.collect();
                }
                Ok(())
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
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
            #[cfg(target_os = "linux")]
            Self::Sealed(pool) => Ok(pool
                .lock()
                .map_err(|_| io::Error::other("pool poisoned"))?
                .stats()),
            Self::Compressed(pool) => {
                let pool = pool.lock().map_err(|_| io::Error::other("pool poisoned"))?;
                Ok((pool.encoded_bytes() as u64, pool.object_count() as u64))
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Shared(pool) => {
                let pool = pool.lock().map_err(|_| io::Error::other("pool poisoned"))?;
                Ok((pool.payload_bytes() as u64, pool.object_count() as u64))
            }
        }
    }
}
fn serve_inner(mut stream: UnixStream, pool: Owner, max_references: usize) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let peer = if pool.is_shared() || pool.is_sealed() {
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Owner::Shared(owner) = &pool {
        owner
            .lock()
            .map_err(|_| io::Error::other("pool poisoned"))?
            .register_session(session);
    }
    let mut references: BTreeMap<u64, Reference> = BTreeMap::new();
    let mut local_counts: BTreeMap<ImageId, LocalReferenceCount> = BTreeMap::new();
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
                PUT | PUT_SHARED | PUT_DUPLICATE | PUT_SEALED => {
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
                    if (operation[0] == PUT_SEALED) != pool.is_sealed()
                        || (matches!(operation[0], PUT_SHARED | PUT_DUPLICATE) && !pool.is_shared())
                    {
                        reject(&mut stream, "publication mode unsupported by this pool")?;
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
                    let id = object.id();
                    let offset = object.offset();
                    // Reference remains owned even if this response fails; session cleanup releases it.
                    let token =
                        acquire_reference(&mut references, &mut local_counts, &mut next, object);
                    let mut reply = Vec::with_capacity(53);
                    reply.push(0);
                    reply.extend_from_slice(&token.to_le_bytes());
                    reply.extend_from_slice(&id);
                    reply.extend_from_slice(&(size as u32).to_le_bytes());
                    if matches!(operation[0], PUT_SHARED | PUT_DUPLICATE) {
                        reply.extend_from_slice(&offset.unwrap().to_le_bytes());
                    }
                    stream.write_all(&reply)?;
                    #[cfg(target_os = "linux")]
                    if let Reference::Sealed(object) = &references[&next] {
                        send_mapping_fd(&stream, object.file().as_raw_fd())?;
                    }
                }
                SEALED => {
                    if pool.is_sealed() {
                        stream.write_all(&[0])?;
                    } else {
                        reject(&mut stream, "sealed compression unsupported by this pool")?;
                    }
                }
                DISCOVERY => {
                    if pool.is_shared() {
                        let mut reply = vec![0];
                        reply.extend_from_slice(&DISCOVERY_VERSION.to_le_bytes());
                        reply.extend_from_slice(&(DISCOVERY_BATCH as u32).to_le_bytes());
                        stream.write_all(&reply)?;
                    } else {
                        reject(&mut stream, "hash discovery requires a physical pool")?;
                    }
                }
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                DISCOVER_XXH3 => {
                    let count = read_u32(&mut stream)? as usize;
                    if count == 0 || count > DISCOVERY_BATCH {
                        return Err(invalid("invalid discovery batch length"));
                    }
                    let mut fingerprints = vec![0; count * 24];
                    stream.read_exact(&mut fingerprints)?;
                    let Owner::Shared(owner) = &pool else {
                        reject(&mut stream, "hash discovery requires a physical pool")?;
                        continue;
                    };
                    let mut reply = Vec::new();
                    reply.extend_from_slice(&(count as u32).to_le_bytes());
                    {
                        let mut owner = owner
                            .lock()
                            .map_err(|_| io::Error::other("pool poisoned"))?;
                        for item in fingerprints.as_chunks::<24>().0 {
                            let id = item[..16].try_into().unwrap();
                            let page = u64::from_le_bytes(item[16..].try_into().unwrap());
                            if references.len() >= max_references || next == u64::MAX {
                                reply.push(3); // No acquired reference.
                                continue;
                            }
                            match owner.discover(id, session, page) {
                                super::shared::Discovery::Unique => reply.push(0),
                                super::shared::Discovery::Unmatched => reply.push(4),
                                super::shared::Discovery::Upload => reply.push(1),
                                super::shared::Discovery::Shared(object) => {
                                    // Pin before replying. Hash-only lookup does not
                                    // authorize use: clients must compare full bytes.
                                    append_object(
                                        &mut reply,
                                        Reference::Shared(object),
                                        &mut references,
                                        &mut local_counts,
                                        &mut next,
                                    );
                                }
                            }
                        }
                        append_ready(&mut reply, owner.take_ready(session));
                    }
                    // Neither socket I/O nor client validation holds the pool lock.
                    write_batch_reply(&mut stream, &reply)?;
                }
                PUT_BATCH => {
                    let count = read_u32(&mut stream)? as usize;
                    if count == 0 || count > DISCOVERY_BATCH || !pool.is_shared() {
                        return Err(invalid("invalid physical materialization batch"));
                    }
                    let mut bytes = vec![0; count * super::shared::PAGE];
                    stream.read_exact(&mut bytes)?;
                    let mut reply = Vec::new();
                    reply.extend_from_slice(&(count as u32).to_le_bytes());
                    for page in bytes.as_chunks::<{ super::shared::PAGE }>().0 {
                        if references.len() >= max_references || next == u64::MAX {
                            reply.push(3);
                            continue;
                        }
                        // One page per lock acquisition preserves session fairness.
                        match pool.intern(page, true, session) {
                            Ok(object) => append_object(
                                &mut reply,
                                object,
                                &mut references,
                                &mut local_counts,
                                &mut next,
                            ),
                            Err(error) if error.kind() == io::ErrorKind::OutOfMemory => {
                                reply.push(3)
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                reply.push(0)
                            }
                            Err(error) => {
                                let text = error.to_string();
                                let bytes = text.as_bytes();
                                let len = bytes.len().min(256);
                                reply.push(5);
                                reply.extend_from_slice(&(len as u32).to_le_bytes());
                                reply.extend_from_slice(&bytes[..len]);
                            }
                        }
                    }
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    if let Owner::Shared(owner) = &pool {
                        append_ready(
                            &mut reply,
                            owner
                                .lock()
                                .map_err(|_| io::Error::other("pool poisoned"))?
                                .take_ready(session),
                        );
                    }
                    write_batch_reply(&mut stream, &reply)?;
                }
                RELEASE_BATCH => {
                    let count = read_u32(&mut stream)? as usize;
                    if count == 0 || count > DISCOVERY_BATCH || !pool.is_shared() {
                        return Err(invalid("invalid physical release batch"));
                    }
                    let mut bytes = vec![0; count * 8];
                    stream.read_exact(&mut bytes)?;
                    let tokens: BTreeSet<_> = bytes
                        .as_chunks::<8>()
                        .0
                        .iter()
                        .map(|item| u64::from_le_bytes(*item))
                        .collect();
                    if tokens.len() != count
                        || tokens.iter().any(|token| !references.contains_key(token))
                    {
                        reject(&mut stream, "invalid batch references")?;
                        continue;
                    }
                    // Validate the whole chunk before consuming any authority.
                    for token in tokens {
                        release_reference(token, &mut references, &mut local_counts, &pool)?;
                    }
                    stream.write_all(&[0])?;
                }
                MAPPING => {
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
                    if !references.contains_key(&token) {
                        reject(&mut stream, "unknown pool reference")?;
                        continue;
                    }
                    release_reference(token, &mut references, &mut local_counts, &pool)?;
                    stream.write_all(&[0])?;
                }
                STATS | SHARED_STATS => {
                    if operation[0] == SHARED_STATS && !pool.is_shared() {
                        return Err(invalid("physical stats on nonphysical session"));
                    }
                    let cross_session = local_counts
                        .values()
                        .filter(|entry| entry.object.strong_count() > 1 + entry.count)
                        .count() as u64;
                    let (payload, objects) = pool.stats()?;
                    let values = [payload, objects, references.len() as u64, cross_session];
                    let mut reply = Vec::new();
                    for value in values {
                        reply.extend_from_slice(&value.to_le_bytes());
                    }
                    if operation[0] == SHARED_STATS {
                        #[cfg(any(target_os = "linux", target_os = "macos"))]
                        if let Owner::Shared(owner) = &pool {
                            append_ready(
                                &mut reply,
                                owner
                                    .lock()
                                    .map_err(|_| io::Error::other("pool poisoned"))?
                                    .take_ready(session),
                            );
                        }
                        write_batch_reply(&mut stream, &reply)?;
                    } else {
                        stream.write_all(&[0])?;
                        stream.write_all(&reply)?;
                    }
                }
                _ => return Err(invalid("unknown pool operation")),
            }
        }
    })();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Owner::Shared(owner) = &pool {
        owner
            .lock()
            .map_err(|_| io::Error::other("pool poisoned"))?
            .unregister_session(session);
        // Only cleanup passes serialize here; this gate never blocks live RPCs.
        // Preserve one rotation's scope while releasing the shared pool between
        // bounded batches. Concurrent publication may evict candidates, but cannot
        // publish new candidates belonging to this disconnected session.
        static CLEANUP: Mutex<()> = Mutex::new(());
        let _cleanup = CLEANUP
            .lock()
            .map_err(|_| io::Error::other("cleanup poisoned"))?;
        let mut remaining = owner
            .lock()
            .map_err(|_| io::Error::other("pool poisoned"))?
            .candidate_count();
        while remaining != 0 {
            let batch = remaining.min(256);
            owner
                .lock()
                .map_err(|_| io::Error::other("pool poisoned"))?
                .forget_candidates_batch(session, batch);
            remaining -= batch;
            std::thread::sleep(Duration::from_micros(50));
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
    drop(local_counts);
    drop(references);
    pool.collect(None)?;
    result
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
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
    message.msg_controllen = unsafe { libc::CMSG_SPACE(4) } as _;
    unsafe {
        let c = libc::CMSG_FIRSTHDR(&message);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(4) as _;
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
    shared_length: u64,
    ready: VecDeque<u64>,
    last_token: u64,
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
            message.msg_controllen = std::mem::size_of_val(&control) as _;
            let n = unsafe {
                libc::recvmsg(stream.as_raw_fd(), &mut message, {
                    #[cfg(target_os = "linux")]
                    {
                        libc::MSG_CMSG_CLOEXEC
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        0
                    }
                })
            };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut fds = Vec::new();
            let mut bad = false;
            unsafe {
                let mut c = libc::CMSG_FIRSTHDR(&message);
                while !c.is_null() {
                    if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                        let length = (*c).cmsg_len as usize;
                        let header = libc::CMSG_LEN(0) as usize;
                        let size = length.saturating_sub(header);
                        bad |=
                            length < header || !size.is_multiple_of(std::mem::size_of::<RawFd>());
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
            let fd = fds.remove(0);
            if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(fd)
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

fn read_shared_page(
    file: &std::fs::File,
    length: u64,
    offset: u64,
) -> io::Result<[u8; super::shared::PAGE]> {
    use std::os::unix::fs::FileExt;
    if !offset.is_multiple_of(super::shared::PAGE as u64)
        || offset
            .checked_add(super::shared::PAGE as u64)
            .is_none_or(|end| end > length)
    {
        return Err(invalid("invalid shared page offset"));
    }
    let mut actual = [0; super::shared::PAGE];
    file.read_exact_at(&mut actual, offset)?;
    Ok(actual)
}

fn validate_shared_page(
    file: &std::fs::File,
    length: u64,
    offset: u64,
    bytes: &[u8],
) -> io::Result<()> {
    let actual = read_shared_page(file, length, offset)?;
    if actual != bytes {
        return Err(invalid("shared page publication content mismatch"));
    }
    Ok(())
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
            shared_length: 0,
            ready: VecDeque::new(),
            last_token: 0,
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
        self.put_duplicates(&[bytes])?.pop().unwrap()
    }

    /// Discover up to 256 native-page snapshots with one XXH3-128-only exchange.
    /// Unique pages acquire nothing and return WouldBlock. Existing slots are
    /// pinned and fully byte-checked locally; only cross-session candidates
    /// needing materialization or fast-hash collisions upload payloads. A
    /// collision's unused pin is released before content-based publication.
    /// SHA-256 remains the stored identity, not a discovery hash. Results
    /// preserve input order.
    /// Empty batches perform no I/O; invalid geometry is rejected before I/O.
    /// A transport/protocol error may leave uncertain server references pinned
    /// until process exit, as with put. No reconnect or live RAM access occurs.
    /// Nonphysical stores retain their individual publication protocol.
    pub fn put_duplicates(
        &mut self,
        blocks: &[&[u8]],
    ) -> io::Result<Vec<io::Result<RemoteObject>>> {
        let samples: Vec<_> = blocks
            .iter()
            .map(|bytes| ColdRamSample { page: 0, bytes })
            .collect();
        Ok(self
            .publish_pages(&samples)?
            .into_iter()
            .map(|result| {
                result.and_then(|admission| match admission {
                    Admission::Published(object) => Ok(object),
                    _ => Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "page not admitted",
                    )),
                })
            })
            .collect())
    }

    /// Publish snapshots with advisory, session-scoped handles. Only Published
    /// owns a reference; hints never expose or authorize VM addresses.
    pub fn publish_pages(
        &mut self,
        blocks: &[ColdRamSample<'_>],
    ) -> io::Result<Vec<io::Result<Admission<RemoteObject>>>> {
        if blocks.is_empty() {
            return Ok(Vec::new());
        }
        if self.shared_file.is_none() {
            return Ok(blocks
                .iter()
                .map(|sample| {
                    self.put_inner(sample.bytes, false)
                        .map(Admission::Published)
                })
                .collect());
        }
        if blocks.len() > DISCOVERY_BATCH
            || blocks
                .iter()
                .any(|sample| sample.bytes.len() != super::shared::PAGE)
        {
            return Err(invalid("invalid physical discovery batch geometry"));
        }
        let discovered = self.physical_exchange(DISCOVER_XXH3, blocks)?;
        let mut results = Vec::with_capacity(blocks.len());
        let mut uploads = Vec::new();
        let mut positions = Vec::new();
        let mut unused = Vec::new();
        for (index, result) in discovered.into_iter().enumerate() {
            match result {
                Discovered::Complete(result) => results.push(Some(result)),
                other => {
                    if let Discovered::Collision(object) = other {
                        unused.push(object);
                    }
                    uploads.push(ColdRamSample {
                        page: blocks[index].page,
                        bytes: blocks[index].bytes,
                    });
                    positions.push(index);
                    results.push(None);
                }
            }
        }
        // Release all unused speculative pins before materialization.
        self.release_objects(unused)?;
        if !uploads.is_empty() {
            for (index, result) in positions
                .into_iter()
                .zip(self.physical_exchange(PUT_BATCH, &uploads)?)
            {
                let Discovered::Complete(result) = result else {
                    self.stream.take();
                    return Err(invalid("materialization returned an unresolved candidate"));
                };
                results[index] = Some(result);
            }
        }
        Ok(results.into_iter().map(Option::unwrap).collect())
    }
    fn physical_exchange(
        &mut self,
        operation: u8,
        blocks: &[ColdRamSample<'_>],
    ) -> io::Result<Vec<Discovered>> {
        let file = self.shared_file.clone().expect("physical session");
        let length = self.shared_length;
        let session = self.session.clone();
        let mut last_token = self.last_token;
        let (results, ready) = self.exchange(|stream| {
            let mut frame = vec![operation];
            frame.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
            for sample in blocks {
                if operation == DISCOVER_XXH3 {
                    frame.extend_from_slice(&fingerprint(sample.bytes));
                    frame.extend_from_slice(&sample.page.to_le_bytes());
                } else {
                    frame.extend_from_slice(sample.bytes);
                }
            }
            stream.write_all(&frame)?;
            let payload = match read_batch_reply(stream)? {
                Ok(payload) => payload,
                Err(error) => return Ok(Err(error)),
            };
            let mut reply = io::Cursor::new(payload.as_slice());
            if read_u32(&mut reply)? as usize != blocks.len() {
                return Err(invalid("discovery result count mismatch"));
            }
            let mut tokens = BTreeSet::new();
            let mut results = Vec::with_capacity(blocks.len());
            for sample in blocks {
                let bytes = sample.bytes;
                let mut tag = [0];
                reply.read_exact(&mut tag)?;
                results.push(match tag[0] {
                    0 => Discovered::Complete(Ok(Admission::Observed)),
                    1 if operation == DISCOVER_XXH3 => Discovered::Upload,
                    2 => {
                        let token = read_u64(&mut reply)?;
                        let offset = read_u64(&mut reply)?;
                        let mut id = [0; 32];
                        reply.read_exact(&mut id)?;
                        if token <= last_token || !tokens.insert(token) {
                            return Err(invalid("invalid discovery reference token"));
                        }
                        last_token = token;
                        let actual = read_shared_page(&file, length, offset)?;
                        let object = RemoteObject {
                            session: session.clone(),
                            token,
                            id,
                            length: bytes.len(),
                            offset: std::num::NonZeroU64::new(offset + 1),
                        };
                        if operation == PUT_BATCH && id != identity(bytes) {
                            return Err(invalid("invalid materialized identity"));
                        }
                        if actual == bytes {
                            Discovered::Complete(Ok(Admission::Published(object)))
                        } else {
                            if operation == PUT_BATCH
                                || identity(&actual) != id
                                || identity(bytes) == id
                            {
                                return Err(invalid("shared page publication content mismatch"));
                            }
                            Discovered::Collision(object)
                        }
                    }
                    3 => Discovered::Complete(Ok(Admission::Limited)),
                    4 => Discovered::Complete(Ok(Admission::Unmatched)),
                    5 => {
                        let size = read_u32(&mut reply)? as usize;
                        if size > 256 {
                            return Err(invalid("batch rejection too large"));
                        }
                        let mut bytes = vec![0; size];
                        reply.read_exact(&mut bytes)?;
                        let text = String::from_utf8(bytes)
                            .map_err(|_| invalid("invalid batch error text"))?;
                        Discovered::Complete(Err(io::Error::other(text)))
                    }
                    _ => return Err(invalid("invalid discovery result")),
                });
            }
            let ready = read_ready(&mut reply)?;
            if reply.position() != payload.len() as u64 {
                return Err(invalid("trailing physical batch bytes"));
            }
            Ok(Ok((results, ready)))
        })?;
        self.last_token = last_token;
        self.retain_ready(ready);
        Ok(results)
    }

    fn retain_ready(&mut self, pages: Vec<u64>) {
        for page in pages {
            if self.ready.len() < DISCOVERY_BATCH && !self.ready.contains(&page) {
                self.ready.push_back(page);
            }
        }
    }

    /// Drain bounded, advisory handles without performing IPC.
    pub fn take_ready(&mut self) -> Vec<u64> {
        self.ready.drain(..).collect()
    }

    fn put_inner(&mut self, bytes: &[u8], duplicate: bool) -> io::Result<RemoteObject> {
        if bytes.is_empty() || bytes.len() > BLOCK_BYTES {
            return Err(invalid("invalid pool input length"));
        }
        let expected = identity(bytes);
        self.put_identified(bytes, duplicate, expected)
    }
    fn put_identified(
        &mut self,
        bytes: &[u8],
        duplicate: bool,
        expected: ImageId,
    ) -> io::Result<RemoteObject> {
        let session = self.session.clone();
        let shared_file = self.shared_file.clone();
        let shared_length = self.shared_length;
        let last_token = self.last_token;
        if shared_file.is_some() && bytes.len() != super::shared::PAGE {
            return Err(invalid("shared pool requires native pages"));
        }
        let object = self.exchange(|stream| {
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
            if token <= last_token || id != expected || length != bytes.len() {
                return Err(invalid("invalid pool object reply"));
            }
            let offset = if let Some(file) = &shared_file {
                let offset = read_u64(stream)?;
                validate_shared_page(file, shared_length, offset, bytes)?;
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
        })?;
        self.last_token = object.token;
        Ok(object)
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn enable_shared_mapping(&mut self) -> io::Result<()> {
        let (file, length) = self.exchange(|stream| {
            stream.write_all(&[MAPPING])?;
            if let Some(error) = status(stream)? {
                return Ok(Err(error));
            }
            let fd = stream.receive_mapping_fd()?;
            let access = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
            #[cfg(target_os = "linux")]
            let seals = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GET_SEALS) };
            #[cfg(target_os = "linux")]
            let sealed = seals >= 0
                && seals & (libc::F_SEAL_GROW | libc::F_SEAL_SHRINK)
                    == libc::F_SEAL_GROW | libc::F_SEAL_SHRINK;
            #[cfg(target_os = "macos")]
            let sealed = {
                use std::os::unix::fs::MetadataExt;
                let m = std::fs::File::from(fd.try_clone()?).metadata()?;
                m.is_file()
                    && m.uid() == unsafe { libc::geteuid() }
                    && m.nlink() == 0
                    && m.mode() & 0o277 == 0
            };
            if access < 0 || access & libc::O_ACCMODE != libc::O_RDONLY || !sealed {
                return Err(invalid(
                    "pool mapping descriptor must be read-only and size-sealed",
                ));
            }
            let file = std::fs::File::from(fd);
            let length = file.metadata()?.len();
            if length == 0 || !length.is_multiple_of(super::shared::PAGE as u64) {
                return Err(invalid("invalid shared pool file size"));
            }
            Ok(Ok((Arc::new(file), length)))
        })?;
        self.exchange(|stream| {
            stream.write_all(&[DISCOVERY])?;
            if let Some(error) = status(stream)? {
                return Ok(Err(error));
            }
            let version = read_u32(stream)?;
            let batch = read_u32(stream)?;
            if version != DISCOVERY_VERSION || batch != DISCOVERY_BATCH as u32 {
                return Err(invalid("unsupported physical discovery protocol"));
            }
            Ok(Ok(()))
        })?;
        self.shared_file = Some(file);
        self.shared_length = length;
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
        self.release_objects(vec![object])
    }
    /// Release unmapped references in bounded chunks. A failed exchange can
    /// leave earlier chunks released and remaining tokens pinned until exit.
    pub fn release_objects(&mut self, objects: Vec<RemoteObject>) -> io::Result<()> {
        for object in &objects {
            self.owns(object)?;
        }
        if self.shared_file.is_some() {
            for chunk in objects.chunks(DISCOVERY_BATCH) {
                self.exchange(|stream| {
                    let mut frame = vec![RELEASE_BATCH];
                    frame.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
                    for object in chunk {
                        frame.extend_from_slice(&object.token.to_le_bytes());
                    }
                    stream.write_all(&frame)?;
                    Ok(match status(stream)? {
                        Some(error) => Err(error),
                        None => Ok(()),
                    })
                })?;
            }
        } else {
            for object in objects {
                self.exchange(|stream| {
                    stream.write_all(&[RELEASE])?;
                    stream.write_all(&object.token.to_le_bytes())?;
                    Ok(match status(stream)? {
                        Some(error) => Err(error),
                        None => Ok(()),
                    })
                })?;
            }
        }
        Ok(())
    }
    pub fn stats(&mut self) -> io::Result<PoolStats> {
        if self.shared_file.is_some() {
            let (stats, ready) = self.exchange(|stream| {
                stream.write_all(&[SHARED_STATS])?;
                let payload = match read_batch_reply(stream)? {
                    Ok(payload) => payload,
                    Err(error) => return Ok(Err(error)),
                };
                let mut reply = io::Cursor::new(payload.as_slice());
                let stats = PoolStats {
                    encoded_bytes: read_u64(&mut reply)?,
                    objects: read_u64(&mut reply)?,
                    session_references: read_u64(&mut reply)?,
                    cross_session_objects: read_u64(&mut reply)?,
                };
                let ready = read_ready(&mut reply)?;
                if reply.position() != payload.len() as u64 {
                    return Err(invalid("trailing physical stats bytes"));
                }
                Ok(Ok((stats, ready)))
            })?;
            self.retain_ready(ready);
            return Ok(stats);
        }
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
    fn framed_reply_reads_are_bounded_independently_of_page_count() {
        struct Counted {
            bytes: io::Cursor<Vec<u8>>,
            reads: usize,
        }
        impl Read for Counted {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                self.bytes.read(bytes)
            }
        }
        for count in [1, DISCOVERY_BATCH] {
            let payload = vec![0; 4 + count * 49 + 4];
            let mut bytes = vec![0];
            bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&payload);
            let mut input = Counted {
                bytes: io::Cursor::new(bytes),
                reads: 0,
            };
            assert_eq!(read_batch_reply(&mut input).unwrap().unwrap(), payload);
            assert_eq!(input.reads, 3);
        }
    }

    #[test]
    fn matching_peer_wakes_first_observer_without_granting_a_reference() {
        use super::super::shared::{PAGE, SharedPool};
        let pool = Arc::new(Mutex::new(SharedPool::new(PAGE, 1).unwrap()));
        let (a, sa) = UnixStream::pair().unwrap();
        let (b, sb) = UnixStream::pair().unwrap();
        let owner = pool.clone();
        let ta = std::thread::spawn(move || serve_shared(sa, owner, 2));
        let owner = pool.clone();
        let tb = std::thread::spawn(move || serve_shared(sb, owner, 2));
        let mut a = PoolClient::new(a, Duration::from_secs(2)).unwrap();
        let mut b = PoolClient::new(b, Duration::from_secs(2)).unwrap();
        a.enable_shared_mapping().unwrap();
        b.enable_shared_mapping().unwrap();
        let bytes = [7; PAGE];
        assert!(matches!(
            a.publish_pages(&[ColdRamSample {
                page: 17,
                bytes: &bytes
            }])
            .unwrap()
            .pop()
            .unwrap()
            .unwrap(),
            Admission::Observed
        ));
        assert!(matches!(
            a.publish_pages(&[ColdRamSample {
                page: 17,
                bytes: &bytes
            }])
            .unwrap()
            .pop()
            .unwrap()
            .unwrap(),
            Admission::Unmatched
        ));
        let Admission::Published(br) = b
            .publish_pages(&[ColdRamSample {
                page: 99,
                bytes: &bytes,
            }])
            .unwrap()
            .pop()
            .unwrap()
            .unwrap()
        else {
            panic!("peer must materialize")
        };
        assert_eq!(a.stats().unwrap().session_references, 0);
        assert_eq!(a.take_ready(), [17]);
        assert!(a.take_ready().is_empty());
        assert!(b.take_ready().is_empty());
        let Admission::Published(ar) = a
            .publish_pages(&[ColdRamSample {
                page: 17,
                bytes: &bytes,
            }])
            .unwrap()
            .pop()
            .unwrap()
            .unwrap()
        else {
            panic!("first observer must find peer")
        };
        assert_eq!(ar.id(), br.id());
        a.release_objects(vec![ar]).unwrap();
        b.release_objects(vec![br]).unwrap();
        assert_eq!(a.stats().unwrap().objects, 0);
        drop(a);
        drop(b);
        ta.join().unwrap().unwrap();
        tb.join().unwrap().unwrap();
    }

    #[test]
    fn a_full_materialization_and_release_use_one_bounded_exchange_each() {
        use super::super::shared::{PAGE, SharedPool};
        let pages: Vec<_> = (0..DISCOVERY_BATCH)
            .map(|index| {
                let mut page = vec![7; PAGE];
                page[..8].copy_from_slice(&(index as u64).to_le_bytes());
                page
            })
            .collect();
        let expected = pages.clone();
        let (client, mut peer) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || {
            peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut pool = SharedPool::new(DISCOVERY_BATCH * PAGE, DISCOVERY_BATCH).unwrap();
            peer.write_all(MAGIC).unwrap();
            peer.write_all(&[1; 16]).unwrap();
            let mut op = [0];
            peer.read_exact(&mut op).unwrap();
            assert_eq!(op, [MAPPING]);
            peer.write_all(&[0]).unwrap();
            send_mapping_fd(&peer, pool.readonly_file().as_raw_fd()).unwrap();
            peer.read_exact(&mut op).unwrap();
            assert_eq!(op, [DISCOVERY]);
            peer.write_all(&[0]).unwrap();
            peer.write_all(&DISCOVERY_VERSION.to_le_bytes()).unwrap();
            peer.write_all(&(DISCOVERY_BATCH as u32).to_le_bytes())
                .unwrap();
            peer.read_exact(&mut op).unwrap();
            assert_eq!(op, [DISCOVER_XXH3]);
            assert_eq!(read_u32(&mut peer).unwrap() as usize, DISCOVERY_BATCH);
            let mut discovery = vec![0; DISCOVERY_BATCH * 24];
            peer.read_exact(&mut discovery).unwrap();
            for (item, page) in discovery.as_chunks::<24>().0.iter().zip(&expected) {
                assert_eq!(&item[..16], &fingerprint(page));
                assert_eq!(&item[16..], &[0; 8]);
            }
            let mut reply = (DISCOVERY_BATCH as u32).to_le_bytes().to_vec();
            reply.extend(vec![1; DISCOVERY_BATCH]);
            append_ready(&mut reply, Vec::new());
            write_batch_reply(&mut peer, &reply).unwrap();
            peer.read_exact(&mut op).unwrap();
            assert_eq!(op, [PUT_BATCH], "no per-page PUT is permitted");
            assert_eq!(read_u32(&mut peer).unwrap() as usize, DISCOVERY_BATCH);
            let mut payload = vec![0; DISCOVERY_BATCH * PAGE];
            peer.read_exact(&mut payload).unwrap();
            let mut reply = (DISCOVERY_BATCH as u32).to_le_bytes().to_vec();
            let mut pins = Vec::new();
            for (index, (actual, expected)) in payload
                .as_chunks::<PAGE>()
                .0
                .iter()
                .zip(&expected)
                .enumerate()
            {
                assert_eq!(actual.as_slice(), expected);
                let object = pool.intern(actual).unwrap();
                reply.push(2);
                reply.extend_from_slice(&(index as u64 + 1).to_le_bytes());
                reply.extend_from_slice(&object.offset().to_le_bytes());
                reply.extend_from_slice(&object.id());
                pins.push(object);
            }
            append_ready(&mut reply, Vec::new());
            write_batch_reply(&mut peer, &reply).unwrap();
            peer.read_exact(&mut op).unwrap();
            assert_eq!(op, [RELEASE_BATCH], "no per-page RELEASE is permitted");
            assert_eq!(read_u32(&mut peer).unwrap() as usize, DISCOVERY_BATCH);
            let mut released = vec![0; DISCOVERY_BATCH * 8];
            peer.read_exact(&mut released).unwrap();
            for (index, bytes) in released.as_chunks::<8>().0.iter().enumerate() {
                assert_eq!(u64::from_le_bytes(*bytes), index as u64 + 1);
            }
            peer.write_all(&[0]).unwrap();
            drop(pins);
        });
        let mut client = PoolClient::new(client, Duration::from_secs(3)).unwrap();
        client.enable_shared_mapping().unwrap();
        client.release_objects(Vec::new()).unwrap();
        assert!(client.put_duplicates(&[]).unwrap().is_empty());
        let blocks: Vec<_> = pages.iter().map(Vec::as_slice).collect();
        let objects: Vec<_> = client
            .put_duplicates(&blocks)
            .unwrap()
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert_eq!(objects.len(), DISCOVERY_BATCH);
        client.release_objects(objects).unwrap();
        drop(client);
        thread.join().unwrap();
    }

    #[test]
    fn invalid_release_batch_does_not_consume_any_valid_reference() {
        use super::super::shared::{PAGE, SharedPool};
        let owner = Arc::new(Mutex::new(SharedPool::new(PAGE, 1).unwrap()));
        let (client, peer) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || serve_shared(peer, owner, 4));
        let mut client = PoolClient::new(client, Duration::from_secs(2)).unwrap();
        client.enable_shared_mapping().unwrap();
        let one = client.put(&[7; PAGE]).unwrap();
        let two = client.put(&[7; PAGE]).unwrap();
        for token in [one.token, u64::MAX] {
            let rejection: io::Result<()> = client.exchange(|stream| {
                let mut frame = vec![RELEASE_BATCH];
                frame.extend_from_slice(&2u32.to_le_bytes());
                frame.extend_from_slice(&one.token.to_le_bytes());
                frame.extend_from_slice(&token.to_le_bytes());
                stream.write_all(&frame)?;
                Ok(match status(stream)? {
                    Some(error) => Err(error),
                    None => Ok(()),
                })
            });
            assert!(rejection.is_err());
            assert_eq!(client.stats().unwrap().session_references, 2);
        }
        client.release_objects(vec![one, two]).unwrap();
        assert_eq!(client.stats().unwrap().objects, 0);
        drop(client);
        thread.join().unwrap().unwrap();
    }

    #[test]
    fn hash_discovery_batches_preserve_order_and_reference_budgets() {
        const PAGE: usize = super::super::shared::PAGE;
        let pool = Arc::new(Mutex::new(
            super::super::shared::SharedPool::with_candidate_limit(PAGE * 2, 2, 16).unwrap(),
        ));
        let (a, sa) = UnixStream::pair().unwrap();
        let (b, sb) = UnixStream::pair().unwrap();
        let owner = pool.clone();
        let ta = std::thread::spawn(move || serve_shared(sa, owner, 2));
        let owner = pool.clone();
        let tb = std::thread::spawn(move || serve_shared(sb, owner, 2));
        let mut a = PoolClient::new(a, Duration::from_secs(2)).unwrap();
        let mut b = PoolClient::new(b, Duration::from_secs(2)).unwrap();
        a.enable_shared_mapping().unwrap();
        b.enable_shared_mapping().unwrap();
        let seven = [7; PAGE];
        let eight = [8; PAGE];
        let nine = [9; PAGE];
        assert!(a.put_duplicates(&[]).unwrap().is_empty());
        assert!(a.put_duplicates(&[&seven[..PAGE - 1]]).is_err());
        assert!(
            a.put_duplicates(&vec![&seven[..]; DISCOVERY_BATCH + 1])
                .is_err()
        );
        let unique = a.put_duplicates(&[&seven, &eight, &nine, &seven]).unwrap();
        assert!(unique.iter().all(|r| matches!(
            r, Err(error) if error.kind() == io::ErrorKind::WouldBlock
        )));
        assert_eq!(a.stats().unwrap().objects, 0);
        assert_eq!(a.stats().unwrap().session_references, 0);
        let largest = a
            .put_duplicates(&vec![&seven[..]; DISCOVERY_BATCH])
            .unwrap();
        assert_eq!(largest.len(), DISCOVERY_BATCH);
        assert!(largest.iter().all(Result::is_err));
        let b_objects: Vec<_> = b
            .put_duplicates(&[&eight, &seven])
            .unwrap()
            .into_iter()
            .map(Result::unwrap)
            .collect();
        let results = a.put_duplicates(&[&seven, &eight, &nine]).unwrap();
        assert!(results[0].is_ok());
        assert!(results[1].is_ok());
        assert_eq!(
            results[2].as_ref().err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        let a_objects: Vec<_> = results.into_iter().filter_map(Result::ok).collect();
        assert_eq!(a_objects[0].id(), b_objects[1].id());
        assert_eq!(a_objects[1].id(), b_objects[0].id());
        assert_eq!(a.stats().unwrap().session_references, 2);
        assert_eq!(a.stats().unwrap().cross_session_objects, 2);
        for object in a_objects {
            a.release(object).unwrap();
        }
        for object in b_objects {
            b.release(object).unwrap();
        }
        assert_eq!(a.stats().unwrap().objects, 0);
        drop(a);
        drop(b);
        ta.join().unwrap().unwrap();
        tb.join().unwrap().unwrap();
    }

    // A strict peer checks the actual wire boundary, rather than inferring
    // payload avoidance from pool object counts.
    fn discovery_peer(
        mut stream: UnixStream,
        known: bool,
        fault: &'static str,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            use super::super::shared::{PAGE, SharedPool};
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut pool = SharedPool::new(2 * PAGE, 2).unwrap();
            let byte = if matches!(fault, "bytes" | "collision") {
                8
            } else {
                7
            };
            let object = pool.intern(&[byte; PAGE]).unwrap();
            stream.write_all(MAGIC).unwrap();
            stream.write_all(&[1; 16]).unwrap();
            let mut operation = [0];
            stream.read_exact(&mut operation).unwrap();
            assert_eq!(operation, [MAPPING]);
            stream.write_all(&[0]).unwrap();
            send_mapping_fd(&stream, pool.readonly_file().as_raw_fd()).unwrap();
            stream.read_exact(&mut operation).unwrap();
            assert_eq!(operation, [DISCOVERY]);
            stream.write_all(&[0]).unwrap();
            let version = if fault == "version" {
                1
            } else if fault == "xxh3-v2" {
                2
            } else {
                DISCOVERY_VERSION
            };
            stream.write_all(&version.to_le_bytes()).unwrap();
            stream
                .write_all(&(DISCOVERY_BATCH as u32).to_le_bytes())
                .unwrap();
            if matches!(fault, "version" | "xxh3-v2") {
                assert_eq!(stream.read(&mut operation).unwrap(), 0);
                return;
            }
            stream.read_exact(&mut operation).unwrap();
            assert_eq!(operation, [DISCOVER_XXH3]);
            assert_eq!(read_u32(&mut stream).unwrap(), 1);
            let mut id = [0; 16];
            stream.read_exact(&mut id).unwrap();
            assert_eq!(id, fingerprint(&[7; PAGE]));
            assert_eq!(read_u64(&mut stream).unwrap(), 0);
            if fault == "length" {
                stream.write_all(&[0]).unwrap();
                stream
                    .write_all(&((MAX_BATCH_REPLY + 1) as u32).to_le_bytes())
                    .unwrap();
                assert_eq!(stream.read(&mut operation).unwrap(), 0);
                return;
            }
            let mut reply = Vec::new();
            reply.extend_from_slice(&(if fault == "count" { 2u32 } else { 1 }).to_le_bytes());
            reply.push(if fault == "tag" {
                255
            } else if known {
                2
            } else {
                0
            });
            if known {
                if fault == "truncated" {
                    stream.write_all(&[0]).unwrap();
                    stream.write_all(&57u32.to_le_bytes()).unwrap();
                    stream.write_all(&[1, 0, 0]).unwrap();
                    return;
                }
                reply.extend_from_slice(&(if fault == "token" { 0u64 } else { 1 }).to_le_bytes());
                let offset = if fault == "offset" {
                    (2 * PAGE) as u64
                } else {
                    object.offset()
                };
                reply.extend_from_slice(&offset.to_le_bytes());
                // "bytes" is corrupt backing for the claimed strong identity,
                // unlike an intact but different object returned by a weak hit.
                let id = if fault == "bytes" {
                    identity(&[7; PAGE])
                } else {
                    object.id()
                };
                reply.extend_from_slice(&id);
            }
            if fault == "ready" {
                reply.extend_from_slice(&257u32.to_le_bytes());
            } else {
                append_ready(&mut reply, Vec::new());
            }
            if fault == "trailing" {
                reply.push(0);
            }
            write_batch_reply(&mut stream, &reply).unwrap();
            if !fault.is_empty() && fault != "collision" {
                assert_eq!(stream.read(&mut operation).unwrap(), 0);
                return;
            }
            let replacement = if fault == "collision" {
                stream.read_exact(&mut operation).unwrap();
                assert_eq!(
                    operation,
                    [RELEASE_BATCH],
                    "unused pin must be released first"
                );
                assert_eq!(read_u32(&mut stream).unwrap(), 1);
                assert_eq!(read_u64(&mut stream).unwrap(), 1);
                stream.write_all(&[0]).unwrap();
                stream.read_exact(&mut operation).unwrap();
                assert_eq!(operation, [PUT_BATCH]);
                assert_eq!(read_u32(&mut stream).unwrap(), 1);
                let mut bytes = [0; PAGE];
                stream.read_exact(&mut bytes).unwrap();
                assert_eq!(bytes, [7; PAGE]);
                let replacement = pool.intern(&bytes).unwrap();
                let mut reply = 1u32.to_le_bytes().to_vec();
                reply.push(2);
                reply.extend_from_slice(&2u64.to_le_bytes());
                reply.extend_from_slice(&replacement.offset().to_le_bytes());
                reply.extend_from_slice(&replacement.id());
                append_ready(&mut reply, Vec::new());
                write_batch_reply(&mut stream, &reply).unwrap();
                Some(replacement)
            } else {
                None
            };
            // No payload PUT may occur for a unique page or an existing hit.
            stream.read_exact(&mut operation).unwrap();
            assert_eq!(operation, [SHARED_STATS]);
            write_batch_reply(&mut stream, &[0; 36]).unwrap();
            if known {
                stream.read_exact(&mut operation).unwrap();
                assert_eq!(operation, [RELEASE_BATCH]);
                assert_eq!(read_u32(&mut stream).unwrap(), 1);
                assert_eq!(
                    read_u64(&mut stream).unwrap(),
                    if replacement.is_some() { 2 } else { 1 }
                );
                stream.write_all(&[0]).unwrap();
            }
        })
    }

    #[test]
    fn unique_and_existing_pages_never_send_payloads() {
        for known in [false, true] {
            let (client, peer) = UnixStream::pair().unwrap();
            let peer = discovery_peer(peer, known, "");
            let mut client = PoolClient::new(client, Duration::from_secs(2)).unwrap();
            client.enable_shared_mapping().unwrap();
            let result = client.put_duplicate(&[7; super::super::shared::PAGE]);
            assert_eq!(result.is_ok(), known);
            client.stats().unwrap();
            if let Ok(object) = result {
                client.release(object).unwrap();
            }
            drop(client);
            peer.join().unwrap();
        }
    }

    #[test]
    fn invalid_discovery_replies_and_different_backing_bytes_fail_closed() {
        for fault in [
            "bytes",
            "count",
            "tag",
            "token",
            "offset",
            "truncated",
            "length",
            "ready",
            "trailing",
        ] {
            let (client, peer) = UnixStream::pair().unwrap();
            let peer = discovery_peer(peer, true, fault);
            let mut client = PoolClient::new(client, Duration::from_secs(2)).unwrap();
            client.enable_shared_mapping().unwrap();
            assert!(
                client
                    .put_duplicate(&[7; super::super::shared::PAGE])
                    .is_err(),
                "{fault}"
            );
            assert!(client.stats().is_err(), "{fault}");
            peer.join().unwrap();
        }
    }

    #[test]
    fn weak_hit_with_different_intact_bytes_releases_pin_and_falls_back_to_content() {
        let (client, peer) = UnixStream::pair().unwrap();
        let peer = discovery_peer(peer, true, "collision");
        let mut client = PoolClient::new(client, Duration::from_secs(2)).unwrap();
        client.enable_shared_mapping().unwrap();
        let object = client
            .put_duplicate(&[7; super::super::shared::PAGE])
            .unwrap();
        assert_eq!(object.id(), identity(&[7; super::super::shared::PAGE]));
        assert_eq!(object.token, 2);
        client.stats().unwrap();
        client.release(object).unwrap();
        drop(client);
        peer.join().unwrap();
    }

    #[test]
    fn sha256_discovery_service_is_rejected_before_physical_mode_is_enabled() {
        for version in ["version", "xxh3-v2"] {
            let (client, peer) = UnixStream::pair().unwrap();
            let peer = discovery_peer(peer, false, version);
            let mut client = PoolClient::new(client, Duration::from_secs(2)).unwrap();
            assert!(client.enable_shared_mapping().is_err());
            assert!(client.shared_file.is_none());
            assert!(client.stats().is_err());
            peer.join().unwrap();
        }
    }

    #[test]
    fn retired_sha256_query_is_not_interpreted_as_xxh3_keys() {
        use super::super::shared::{PAGE, SharedPool};
        let pool = Arc::new(Mutex::new(SharedPool::new(PAGE, 1).unwrap()));
        let owner = pool.clone();
        let (mut client, peer) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let peer = std::thread::spawn(move || serve_shared(peer, owner, 1));
        let mut handshake = [0; 24];
        client.read_exact(&mut handshake).unwrap();
        assert_eq!(&handshake[..8], MAGIC);
        client.write_all(&[11]).unwrap();
        let mut byte = [0];
        assert_eq!(client.read(&mut byte).unwrap(), 0);
        assert!(peer.join().unwrap().is_err());
        assert_eq!(pool.lock().unwrap().candidate_count(), 0);
    }

    #[test]
    fn cached_session_counts_distinguish_local_aliases_from_other_connections() {
        let pool = Arc::new(Mutex::new(CompressedPool::new(65536, 4)));
        let (a, sa) = UnixStream::pair().unwrap();
        let (b, sb) = UnixStream::pair().unwrap();
        let owner = pool.clone();
        let ta = std::thread::spawn(move || serve(sa, owner, 8));
        let owner = pool.clone();
        let tb = std::thread::spawn(move || serve(sb, owner, 8));
        let mut a = PoolClient::new(a, Duration::from_secs(5)).unwrap();
        let mut b = PoolClient::new(b, Duration::from_secs(5)).unwrap();
        let a1 = a.put(&[7; 4096]).unwrap();
        let a2 = a.put(&[7; 4096]).unwrap();
        let a3 = a.put(&[7; 4096]).unwrap();
        assert_eq!(a.stats().unwrap().session_references, 3);
        assert_eq!(a.stats().unwrap().cross_session_objects, 0);
        let b1 = b.put(&[7; 4096]).unwrap();
        assert_eq!(a.stats().unwrap().cross_session_objects, 1);
        assert_eq!(b.stats().unwrap().cross_session_objects, 1);
        a.release(a1).unwrap();
        assert_eq!(a.stats().unwrap().cross_session_objects, 1);
        b.release(b1).unwrap();
        assert_eq!(a.stats().unwrap().cross_session_objects, 0);
        a.release(a2).unwrap();
        a.release(a3).unwrap();
        assert_eq!(a.stats().unwrap().objects, 0);
        assert_eq!(a.stats().unwrap().session_references, 0);
        let again = a.put(&[7; 4096]).unwrap();
        assert_eq!(a.stats().unwrap().cross_session_objects, 0);
        a.release(again).unwrap();
        drop(a);
        drop(b);
        ta.join().unwrap().unwrap();
        tb.join().unwrap().unwrap();
        assert_eq!(pool.lock().unwrap().object_count(), 0);
    }
    #[test]
    fn remote_page_authority_uses_at_most_64_bytes() {
        // A full 512 MiB guest can reference 131072 pages. Keep its per-page
        // authority budget at 8 MiB, with session identity held once per client.
        assert!(std::mem::size_of::<RemoteObject>() <= 64);
    }
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn physical_pool_maps_one_page_and_cow_keeps_other_references_unchanged() {
        const PAGE: usize = super::super::shared::PAGE;
        let pool = Arc::new(Mutex::new(
            super::super::shared::SharedPool::new(2 * PAGE, 2).unwrap(),
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
        assert!(a.put_duplicate(&[0x53; PAGE]).is_err());
        assert_eq!(a.stats().unwrap().objects, 0);
        let br = b.put_duplicate(&[0x53; PAGE]).unwrap();
        let ar = a.put_duplicate(&[0x53; PAGE]).unwrap();
        let am = a.shared_mapping(&ar).unwrap();
        let bm = b.shared_mapping(&br).unwrap();
        assert_eq!(am.offset, bm.offset);
        assert_eq!(a.stats().unwrap().encoded_bytes, PAGE as u64);
        assert_eq!(a.stats().unwrap().cross_session_objects, 1);
        let map = |m: &pvisor_vm::api::SharedRamMapping| unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                PAGE,
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
            libc::munmap(ap, PAGE);
            libc::munmap(bp, PAGE);
        }
        a.release(ar).unwrap();
        let mut out = [0; PAGE];
        b.restore(&br, &mut out).unwrap();
        assert_eq!(out, [0x53; PAGE]);
        b.release(br).unwrap();
        drop(a);
        drop(b);
        ta.join().unwrap().unwrap();
        tb.join().unwrap().unwrap();
        assert_eq!(pool.lock().unwrap().object_count(), 0);
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn disconnected_mapping_is_pinned_until_peer_process_exits() {
        struct ChildOwner(std::process::Child);
        impl Drop for ChildOwner {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        const PAGE: usize = super::super::shared::PAGE;
        const CHILD: &str = "PVISOR_SHARED_LIFETIME_CHILD";
        if let Some(socket) = std::env::var_os(CHILD) {
            let socket = std::path::PathBuf::from(socket);
            let mut client = PoolClient::new(
                UnixStream::connect(&socket).unwrap(),
                Duration::from_secs(5),
            )
            .unwrap();
            client.enable_shared_mapping().unwrap();
            let object = client.put(&[7; PAGE]).unwrap();
            let mapping = client.shared_mapping(&object).unwrap();
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    PAGE,
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
            // Keep readiness separate from libtest's stdout progress prefixes.
            // This receipt is sent only after mapping and disconnecting the pool.
            let mut control = UnixStream::connect(socket.with_file_name("lifetime.sock")).unwrap();
            control
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            control
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            control.write_all(b"ready").unwrap();
            let mut command = [0];
            control.read_exact(&mut command).unwrap();
            assert_eq!(command, [1]);
            unsafe {
                assert_eq!(*(ptr as *const u8), 7);
                libc::munmap(ptr, PAGE);
            }
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("pool.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let readiness =
            std::os::unix::net::UnixListener::bind(directory.path().join("lifetime.sock")).unwrap();
        let pool = Arc::new(Mutex::new(
            super::super::shared::SharedPool::new(PAGE, 1).unwrap(),
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
        let mut child = ChildOwner(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--nocapture"])
                .env(CHILD, &socket)
                .stdin(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut fd = libc::pollfd {
            fd: readiness.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(
            unsafe { libc::poll(&mut fd, 1, 5000) },
            1,
            "child did not establish its lifetime control channel within five seconds"
        );
        readiness.set_nonblocking(true).unwrap();
        let (mut control, _) = readiness.accept().unwrap();
        // BSD accepted sockets may inherit O_NONBLOCK from the listener.
        // Keep accept nonblocking, but wait for the receipt with bounded blocking I/O.
        control.set_nonblocking(false).unwrap();
        control
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        control
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut receipt = [0; 5];
        control.read_exact(&mut receipt).unwrap();
        assert_eq!(&receipt, b"ready");
        let mut observer = PoolClient::new(
            UnixStream::connect(&socket).unwrap(),
            Duration::from_secs(5),
        )
        .unwrap();
        observer.enable_shared_mapping().unwrap();
        assert_eq!(observer.stats().unwrap().objects, 1);
        assert!(observer.put(&[9; PAGE]).is_err());
        control.write_all(&[1]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "peer did not exit after release");
            std::thread::yield_now();
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while observer.stats().unwrap().objects != 0 {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let object = observer.put(&[9; PAGE]).unwrap();
        let mut bytes = [0; PAGE];
        observer.restore(&object, &mut bytes).unwrap();
        assert_eq!(bytes, [9; PAGE]);
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
