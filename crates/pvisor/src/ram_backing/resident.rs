//! Experimental in-process compressed content pool and cold-window bookkeeping.
//! This module performs no mapping or eviction. The VMM must arm real CPU/device
//! observation before starting a window and quiesce access before consuming it.
use super::{BLOCK_BYTES, ImageId, invalid};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug)]
enum Payload {
    Fill(u8),
    Zstd(Box<[u8]>),
    Raw(Box<[u8]>),
}

/// Immutable content shared by references in one host owner; not an IPC handle.
#[derive(Debug)]
pub struct CompressedObject {
    id: ImageId,
    length: usize,
    payload: Payload,
}
impl CompressedObject {
    /// Shared encoding for resident and durable content; callers provide stable bytes.
    pub(crate) fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > BLOCK_BYTES {
            return Err(invalid("invalid resident block length"));
        }
        let payload = if bytes.iter().all(|b| *b == bytes[0]) {
            Payload::Fill(bytes[0])
        } else {
            let encoded = zstd::bulk::compress(bytes, 1)?;
            if encoded.len() < bytes.len() {
                Payload::Zstd(encoded.into_boxed_slice())
            } else {
                Payload::Raw(bytes.into())
            }
        };
        Ok(Self {
            id: identity(bytes),
            length: bytes.len(),
            payload,
        })
    }

    /// Versioned disk frame; checksums identify decoded bytes, not codec output.
    pub(crate) fn frame(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(45 + self.encoded_bytes());
        bytes.extend_from_slice(b"PVBLK1\0\0");
        bytes.extend_from_slice(&self.id);
        bytes.extend_from_slice(&(self.length as u32).to_le_bytes());
        match &self.payload {
            Payload::Fill(byte) => bytes.extend_from_slice(&[0, *byte]),
            Payload::Raw(payload) => {
                bytes.push(1);
                bytes.extend_from_slice(payload);
            }
            Payload::Zstd(payload) => {
                bytes.push(2);
                bytes.extend_from_slice(payload);
            }
        }
        bytes
    }

    /// Parse framing only. Callers must verify decoded identity with `restore`
    /// before exposing bytes. This avoids decoding the same block twice.
    pub(crate) fn from_frame(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < 46 || bytes.len() > 45 + BLOCK_BYTES || &bytes[..8] != b"PVBLK1\0\0" {
            return Err(invalid("invalid content frame"));
        }
        let length = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize;
        if length == 0 || length > BLOCK_BYTES {
            return Err(invalid("invalid content frame length"));
        }
        let data = &bytes[45..];
        let payload = match bytes[44] {
            0 if data.len() == 1 => Payload::Fill(data[0]),
            1 if data.len() == length => Payload::Raw(data.into()),
            2 if data.len() < length
                && zstd::zstd_safe::find_frame_compressed_size(data).ok() == Some(data.len()) =>
            {
                Payload::Zstd(data.into())
            }
            _ => return Err(invalid("invalid content frame encoding")),
        };
        let object = Self {
            id: bytes[8..40].try_into().unwrap(),
            length,
            payload,
        };
        Ok(object)
    }

    pub fn id(&self) -> ImageId {
        self.id
    }
    pub fn length(&self) -> usize {
        self.length
    }
    /// Encoded payload bytes only; excludes object/index and allocator overhead.
    pub fn encoded_bytes(&self) -> usize {
        match &self.payload {
            Payload::Fill(_) => 1,
            Payload::Zstd(b) | Payload::Raw(b) => b.len(),
        }
    }
    pub fn restore(&self, output: &mut [u8]) -> io::Result<()> {
        if output.len() != self.length {
            return Err(invalid("compressed object output length mismatch"));
        }
        match &self.payload {
            Payload::Fill(byte) => output.fill(*byte),
            Payload::Raw(bytes) => output.copy_from_slice(bytes),
            Payload::Zstd(bytes) => {
                let n = zstd::bulk::decompress_to_buffer(bytes, output)?;
                if n != output.len() {
                    return Err(invalid("compressed object decoded length mismatch"));
                }
            }
        }
        if identity(output) != self.id {
            return Err(invalid("compressed object checksum mismatch"));
        }
        Ok(())
    }
}
pub(super) fn identity(bytes: &[u8]) -> ImageId {
    // Domain and decoded length keep this identity separate from PVZRAM image IDs.
    let mut digest = Sha256::new();
    digest.update(b"PVRES1\0\0");
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
    digest.finalize().into()
}

/// One host owner serializes this pool; payload and index have separate budgets.
/// Call collect after releasing external references. No hidden collector thread.
pub struct CompressedPool {
    objects: BTreeMap<ImageId, Arc<CompressedObject>>,
    payload_budget: usize,
    object_budget: usize,
    encoded_bytes: usize,
}
impl CompressedPool {
    pub fn new(payload_budget: usize, object_budget: usize) -> Self {
        Self {
            objects: BTreeMap::new(),
            payload_budget,
            object_budget,
            encoded_bytes: 0,
        }
    }
    pub fn encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }
    /// Input must come from a stable epoch; hashing does not make a VM quiescent.
    pub fn intern(&mut self, bytes: &[u8]) -> io::Result<Arc<CompressedObject>> {
        if bytes.is_empty() || bytes.len() > BLOCK_BYTES {
            return Err(invalid("invalid resident block length"));
        }
        let id = identity(bytes);
        if let Some(object) = self.objects.get(&id) {
            // Fill and raw payloads can be checked directly under the pool lock.
            // Preserve full content comparison; the digest alone never grants sharing.
            let matches = object.length == bytes.len()
                && match &object.payload {
                    Payload::Fill(byte) => bytes.iter().all(|b| b == byte),
                    Payload::Raw(stored) => stored.as_ref() == bytes,
                    Payload::Zstd(_) => {
                        let mut decoded = vec![0; bytes.len()];
                        object.restore(&mut decoded)?;
                        decoded == bytes
                    }
                };
            if !matches {
                return Err(invalid("resident content identity collision"));
            }
            return Ok(object.clone());
        }
        let object = Arc::new(CompressedObject::from_bytes(bytes)?);
        let size = object.encoded_bytes();
        if self.objects.len() >= self.object_budget
            || size > self.payload_budget.saturating_sub(self.encoded_bytes)
        {
            self.collect();
        }
        if self.objects.len() >= self.object_budget
            || size > self.payload_budget.saturating_sub(self.encoded_bytes)
        {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "resident compressed pool budget exhausted",
            ));
        }
        self.encoded_bytes += size;
        self.objects.insert(id, object.clone());
        Ok(object)
    }
    pub fn collect(&mut self) -> usize {
        let before = self.objects.len();
        self.objects.retain(|_, object| {
            if Arc::strong_count(object) == 1 {
                self.encoded_bytes -= object.encoded_bytes();
                false
            } else {
                true
            }
        });
        before - self.objects.len()
    }
    /// Release paths know the changed identity; avoid scanning unrelated objects.
    pub(super) fn collect_one(&mut self, id: ImageId) -> bool {
        if self
            .objects
            .get(&id)
            .is_some_and(|object| Arc::strong_count(object) == 1)
        {
            let object = self.objects.remove(&id).unwrap();
            self.encoded_bytes -= object.encoded_bytes();
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ColdCandidate {
    page: usize,
    observation: u64,
}
impl ColdCandidate {
    pub fn page(&self) -> usize {
        self.page
    }
}
struct Page {
    observation: u64,
    armed: Option<Instant>,
}
/// A window is valid only while CPU read/write/execute and device access are
/// observed. Residency/mincore is not an observation of access.
pub struct ColdWindows {
    pages: Vec<Page>,
    window: Duration,
}
impl ColdWindows {
    pub fn new(page_count: usize, window: Duration) -> io::Result<Self> {
        if page_count == 0 || window.is_zero() {
            return Err(invalid("invalid cold observation geometry"));
        }
        Ok(Self {
            pages: (0..page_count)
                .map(|_| Page {
                    observation: 0,
                    armed: None,
                })
                .collect(),
            window,
        })
    }
    /// Call only after the actual no-access/observer transition succeeds.
    pub fn arm(&mut self, page: usize, now: Instant) -> io::Result<()> {
        let p = self
            .pages
            .get_mut(page)
            .ok_or_else(|| invalid("cold page out of range"))?;
        p.observation = p
            .observation
            .checked_add(1)
            .ok_or_else(|| invalid("cold observation exhausted"))?;
        p.armed = Some(now);
        Ok(())
    }
    /// CPU and device access both disarm; callers rearm in a later sampling round.
    pub fn touch(&mut self, page: usize) -> io::Result<()> {
        let p = self
            .pages
            .get_mut(page)
            .ok_or_else(|| invalid("cold page out of range"))?;
        p.armed = None;
        Ok(())
    }
    // ponytail: scans the page index; use a rotating bounded scan if sampling CPU is material.
    pub fn candidates(&self, now: Instant, limit: usize) -> Vec<ColdCandidate> {
        self.pages
            .iter()
            .enumerate()
            .filter_map(|(page, p)| {
                p.armed
                    .filter(|at| {
                        now.checked_duration_since(*at)
                            .is_some_and(|age| age >= self.window)
                    })
                    .map(|_| ColdCandidate {
                        page,
                        observation: p.observation,
                    })
            })
            .take(limit)
            .collect()
    }
    /// Recheck under a quiescent mapping epoch; a candidate alone grants no right
    /// to discard bytes. The mapping owner still commits content before eviction.
    pub fn consume(&mut self, candidate: ColdCandidate, now: Instant) -> bool {
        let Some(p) = self.pages.get_mut(candidate.page) else {
            return false;
        };
        if p.observation != candidate.observation
            || !p.armed.is_some_and(|at| {
                now.checked_duration_since(at)
                    .is_some_and(|age| age >= self.window)
            })
        {
            return false;
        }
        p.armed = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn targeted_collection_preserves_pins_and_avoids_full_scan() {
        for count in [1usize, 256, 8192] {
            let mut pool = CompressedPool::new(count * 16 + 16, count + 1);
            let pins: Vec<_> = (0..count)
                .map(|n| pool.intern(&(n as u128).to_le_bytes()).unwrap())
                .collect();
            assert!(!pool.collect_one(pins[0].id()));
            assert!(!pool.collect_one([255; 32]));
            let mut full = Duration::ZERO;
            let mut targeted = Duration::ZERO;
            for _ in 0..256 {
                let object = pool.intern(&(count as u128).to_le_bytes()).unwrap();
                let id = object.id();
                let duplicate = object.clone();
                drop(object);
                assert!(!pool.collect_one(id));
                drop(duplicate);
                let start = Instant::now();
                assert!(pool.collect_one(id));
                targeted += start.elapsed();
                let object = pool.intern(&(count as u128).to_le_bytes()).unwrap();
                drop(object);
                let start = Instant::now();
                assert_eq!(pool.collect(), 1);
                full += start.elapsed();
                assert_eq!(pool.object_count(), count);
                assert_eq!(
                    pool.encoded_bytes(),
                    pins.iter().map(|p| p.encoded_bytes()).sum::<usize>()
                );
            }
            println!(
                "pool-collection objects={count} releases=256 full_us={} targeted_us={}",
                full.as_micros(),
                targeted.as_micros()
            );
            drop(pins);
            assert_eq!(pool.collect(), count);
            assert_eq!(pool.encoded_bytes(), 0);
        }
    }
    #[test]
    fn sharing_budget_lifetime_restore_and_stale_cold_candidates() {
        let mut pool = CompressedPool::new(1024, 2);
        let bytes = vec![42; 16384];
        let a = pool.intern(&bytes).unwrap();
        let b = pool.intern(&bytes).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(pool.encoded_bytes(), 1);
        let mut decoded = vec![0; bytes.len()];
        a.restore(&mut decoded).unwrap();
        assert_eq!(decoded, bytes);
        assert!(a.restore(&mut decoded[..1]).is_err());
        drop(a);
        assert_eq!(pool.collect(), 0);
        drop(b);
        assert_eq!(pool.collect(), 1);
        assert_eq!(pool.encoded_bytes(), 0);
        let repeated: Vec<_> = (0..BLOCK_BYTES).map(|n| (n % 251) as u8).collect();
        let mut larger = CompressedPool::new(BLOCK_BYTES * 2, 2);
        let compressed = larger.intern(&repeated).unwrap();
        assert!(matches!(compressed.payload, Payload::Zstd(_)));
        assert!(Arc::ptr_eq(&compressed, &larger.intern(&repeated).unwrap()));
        let mut output = vec![0; BLOCK_BYTES];
        compressed.restore(&mut output).unwrap();
        assert_eq!(output, repeated);
        let mut seed = 123456789u64;
        let random: Vec<_> = (0..BLOCK_BYTES)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect();
        let raw = larger.intern(&random).unwrap();
        assert!(matches!(raw.payload, Payload::Raw(_)));
        assert!(Arc::ptr_eq(&raw, &larger.intern(&random).unwrap()));
        raw.restore(&mut output).unwrap();
        assert_eq!(output, random);
        assert!(larger.intern(&bytes).is_err()); // Both object slots are pinned.
        let corrupt = CompressedObject {
            id: raw.id,
            length: raw.length,
            payload: Payload::Fill(0),
        };
        assert!(corrupt.restore(&mut output).is_err());
        // Inject an identity collision without needing a real SHA-256 collision.
        for payload in [
            Payload::Fill(41),
            Payload::Raw(vec![41; bytes.len()].into_boxed_slice()),
            Payload::Zstd(
                zstd::bulk::compress(&vec![41; bytes.len()], 1)
                    .unwrap()
                    .into_boxed_slice(),
            ),
        ] {
            let mut collision = CompressedPool::new(BLOCK_BYTES, 2);
            let id = identity(&bytes);
            collision.objects.insert(
                id,
                Arc::new(CompressedObject {
                    id,
                    length: bytes.len(),
                    payload,
                }),
            );
            assert!(collision.intern(&bytes).is_err());
        }
        assert!(pool.intern(&[]).is_err());
        assert!(pool.intern(&vec![0; BLOCK_BYTES + 1]).is_err());
        let kept = pool.intern(&bytes).unwrap();
        let mut no_budget = CompressedPool::new(0, 2);
        assert!(no_budget.intern(&bytes).is_err());
        assert_eq!(pool.object_count(), 1);
        drop(kept);
        let now = Instant::now();
        let mut windows = ColdWindows::new(2, Duration::from_secs(1)).unwrap();
        windows.arm(0, now).unwrap();
        windows.arm(1, now).unwrap();
        assert!(windows.candidates(now, 2).is_empty());
        let later = now + Duration::from_secs(2);
        let candidates = windows.candidates(later, 2);
        windows.touch(0).unwrap();
        assert!(!windows.consume(candidates[0], later));
        windows.arm(1, later).unwrap();
        assert!(!windows.consume(candidates[1], later));
        assert!(windows.arm(2, now).is_err());
        let next = windows.candidates(later + Duration::from_secs(1), 1);
        assert_eq!(next.len(), 1);
        assert!(windows.consume(next[0], later + Duration::from_secs(1)));
        assert!(!windows.consume(next[0], later + Duration::from_secs(1)));
    }
}
