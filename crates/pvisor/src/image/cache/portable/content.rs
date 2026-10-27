//! Immutable content-only blobs. Small-file offsets belong to authenticated
//! data frames; file names, ownership, timestamps and links never enter blobs.
use super::binary::{SourceContent, SourceEntry};
use super::frames::{self, Digest, FrameRef};
use super::*;
use bincode::{Decode, Encode};
use std::collections::BTreeSet;

const SMALL_FILE_BYTES: u64 = 64 * 1024;
const DATA_FRAME_BYTES: usize = 16 * 1024;
// The packing target is not a hard file-size limit: one 64 KiB file must
// remain readable, including its slot-table/codec overhead.
const MAX_PACK_FRAME_RAW: usize = SMALL_FILE_BYTES as usize + 8192;
const PACK_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Encode, Decode)]
pub(super) struct Blob {
    digest: Digest,
    bytes: u64,
    layout: FrameRef,
}
#[derive(Clone, Debug, Encode, Decode)]
pub(super) struct Packed {
    pub blob: Blob,
    pub frame: u32,
    pub slot: u32,
}
impl Packed {
    pub(super) fn object(&self) -> (String, u64) {
        (pack_key(&self.blob), self.blob.bytes)
    }
}
#[derive(Encode, Decode)]
enum Data {
    Slots { offsets: Vec<u32>, bytes: Vec<u8> },
    Directory(Vec<FrameRef>),
}
pub(super) struct Cached {
    value: Data,
    reference: Option<FrameRef>,
    _charge: Option<crate::cache_budget::Charge>,
}
pub(super) struct DataCache {
    entries: LruCache<String, Arc<Cached>>,
    bytes: usize,
}
impl DataCache {
    pub(super) fn new() -> Self {
        Self {
            entries: LruCache::new(NonZeroUsize::new(4096).unwrap()),
            bytes: 0,
        }
    }
    fn get(&mut self, key: &str) -> Option<&Arc<Cached>> {
        self.entries.get(key)
    }
    fn pop_lru(&mut self) -> Option<(String, Arc<Cached>)> {
        let item = self.entries.pop_lru()?;
        self.bytes -= item.1.retained_bytes();
        Some(item)
    }
    fn put(&mut self, key: String, value: Arc<Cached>) {
        if let Some(old) = self.entries.pop(&key) {
            self.bytes -= old.retained_bytes();
        }
        let bytes = value.retained_bytes();
        while self.bytes + bytes > 64 * 1024 * 1024 || self.entries.len() >= 4096 {
            if self.pop_lru().is_none() {
                return;
            }
        }
        self.bytes += bytes;
        self.entries.put(key, value);
    }
    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
impl Cached {
    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + match &self.value {
                Data::Slots { offsets, bytes } => offsets.capacity() * 4 + bytes.capacity(),
                Data::Directory(frames) => frames.capacity() * std::mem::size_of::<FrameRef>(),
            }
    }
}

pub(super) fn pack_key(blob: &Blob) -> String {
    let hex = crate::util::encode_hex(&blob.digest);
    format!("data/packs/{}/{}/{hex}", &hex[..2], &hex[2..4])
}

fn pack(staging: &std::path::Path, digests: &[String]) -> anyhow::Result<(Vec<u8>, Vec<Packed>)> {
    let mut writer = frames::Writer::default();
    let mut references = Vec::new();
    let mut placements = Vec::new();
    let mut bytes = Vec::new();
    let mut offsets = vec![0];
    let flush = |writer: &mut frames::Writer,
                 references: &mut Vec<FrameRef>,
                 offsets: &mut Vec<u32>,
                 bytes: &mut Vec<u8>|
     -> anyhow::Result<()> {
        if offsets.len() == 1 {
            return Ok(());
        }
        let count = offsets.len() - 1;
        let value = Data::Slots {
            offsets: std::mem::replace(offsets, vec![0]),
            bytes: std::mem::take(bytes),
        };
        references.push(writer.push(&value, count as u64)?);
        Ok(())
    };
    for digest in digests {
        let value = std::fs::read(staging.join(&digest[7..]))?;
        if bytes.len() + value.len() > DATA_FRAME_BYTES || offsets.len() > 1024 {
            flush(&mut writer, &mut references, &mut offsets, &mut bytes)?;
        }
        placements.push((references.len() as u32, offsets.len() as u32 - 1));
        bytes.extend_from_slice(&value);
        offsets.push(bytes.len() as u32);
    }
    flush(&mut writer, &mut references, &mut offsets, &mut bytes)?;
    let layout = writer.push(&Data::Directory(references), digests.len() as u64)?;
    let bytes = writer.finish()?;
    let blob = Blob {
        digest: frames::digest(&bytes),
        bytes: bytes.len() as u64,
        layout,
    };
    Ok((
        bytes,
        placements
            .into_iter()
            .map(|(frame, slot)| Packed {
                blob: blob.clone(),
                frame,
                slot,
            })
            .collect(),
    ))
}

pub(super) fn encode_chunk(bytes: Vec<u8>) -> anyhow::Result<Vec<u8>> {
    let mut writer = frames::Writer::default();
    let length = bytes.len() as u32;
    writer.push(
        &Data::Slots {
            offsets: vec![0, length],
            bytes,
        },
        1,
    )?;
    writer.finish()
}

impl PortableCache {
    fn admit_data(&self, key: String, mut value: Cached) -> Arc<Cached> {
        let mut cache = self.blobs.lock().unwrap();
        let charge = crate::cache_budget::reserve_replacing(value.retained_bytes(), || {
            cache.pop_lru().is_some()
        });
        let admitted = charge.is_ok();
        value._charge = charge.ok().flatten();
        let value = Arc::new(value);
        if admitted {
            cache.put(key, value.clone());
        }
        value
    }
    fn data_frame(&self, blob: &Blob, reference: &FrameRef) -> anyhow::Result<Arc<Cached>> {
        ensure!(
            blob.bytes <= MAX_OBJECT as u64,
            "content blob exceeds limit"
        );
        frames::validate(reference, blob.bytes, frames::MAX_FRAME_RAW)?;
        let key = format!(
            "{}:{}",
            frames::digest_string(&blob.digest),
            reference.offset
        );
        if let Some(value) = self.blobs.lock().unwrap().get(&key).cloned() {
            ensure!(
                value.reference.as_ref() == Some(reference),
                "inconsistent cached data frame reference"
            );
            return Ok(value);
        }
        let owner = self.flights.acquire(&key)?;
        let _miss = owner.lock().unwrap();
        if let Some(value) = self.blobs.lock().unwrap().get(&key).cloned() {
            ensure!(
                value.reference.as_ref() == Some(reference),
                "inconsistent cached data frame reference"
            );
            return Ok(value);
        }
        let local = self.local_objects.as_ref().map(|root| {
            root.join("frames")
                .join(crate::util::encode_hex(&reference.digest))
        });
        let valid = |bytes: &[u8]| {
            bytes.len() == reference.encoded as usize && frames::digest(bytes) == reference.digest
        };
        let encoded = if let Some(bytes) = local
            .as_ref()
            .map(|p| read_local(p, frames::MAX_FRAME_ENCODED))
            .transpose()?
            .flatten()
            .filter(|v| valid(v))
        {
            bytes
        } else {
            let bytes = self.storage.range(
                &pack_key(blob),
                reference.offset..reference.offset + reference.encoded as u64,
            )?;
            ensure!(valid(&bytes), "cache data digest mismatch");
            if let Some(path) = local {
                cache_local(&path, &bytes);
            }
            bytes
        };
        let value: Data = frames::unpack(reference, &encoded)?;
        match &value {
            Data::Directory(children) => {
                ensure!(
                    !children.is_empty() && children.len() <= 4096,
                    "invalid pack frame count"
                );
                let mut count = 0u64;
                for child in children {
                    frames::validate(child, reference.offset, MAX_PACK_FRAME_RAW)?;
                    ensure!(child.count > 0 && child.count <= 1024, "invalid slot count");
                    count = count
                        .checked_add(child.count)
                        .context("slot count overflow")?;
                }
                ensure!(count == reference.count, "pack slot count mismatch");
            }
            Data::Slots { offsets, bytes } => validate_slots(reference.count, offsets, bytes)?,
        }
        Ok(self.admit_data(
            key,
            Cached {
                value,
                reference: Some(reference.clone()),
                _charge: None,
            },
        ))
    }
    pub(super) fn read_packed(
        &self,
        packed: &Packed,
        digest: &Digest,
        size: u64,
        offset: u64,
        end: u64,
    ) -> anyhow::Result<Vec<u8>> {
        let layout = self.data_frame(&packed.blob, &packed.blob.layout)?;
        let Data::Directory(children) = &layout.value else {
            bail!("invalid pack directory")
        };
        let child = children
            .get(packed.frame as usize)
            .context("pack frame out of range")?;
        let frame = self.data_frame(&packed.blob, child)?;
        let Data::Slots { offsets, bytes } = &frame.value else {
            bail!("invalid pack slot frame")
        };
        let slot = packed.slot as usize;
        ensure!(slot + 1 < offsets.len(), "pack slot out of range");
        let value = &bytes[offsets[slot] as usize..offsets[slot + 1] as usize];
        ensure!(
            value.len() as u64 == size && frames::digest(value) == *digest,
            "pack file digest or length mismatch"
        );
        Ok(value[offset as usize..end as usize].to_vec())
    }
    pub(super) fn read_chunk(
        &self,
        digest: &Digest,
        length: u32,
        offset: usize,
        end: usize,
    ) -> anyhow::Result<Vec<u8>> {
        let key = frames::digest_string(digest);
        let owner = self.flights.acquire(&key)?;
        let _miss = owner.lock().unwrap();
        let cached = self.blobs.lock().unwrap().get(&key).cloned();
        let frame = if let Some(cached) = cached {
            cached
        } else {
            let local = self
                .local_objects
                .as_ref()
                .map(|root| root.join("chunks").join(&key[7..]));
            let local_value = local
                .as_ref()
                .map(|p| read_local(p, frames::MAX_FRAME_ENCODED + 25))
                .transpose()?
                .flatten()
                .and_then(|encoded| decode_chunk(&encoded, digest, length).ok());
            let value = if let Some(value) = local_value {
                value
            } else {
                let encoded = self
                    .storage
                    .get(&data_key(&key)?)?
                    .context("missing cache data object")?;
                let value = decode_chunk(&encoded, digest, length)?;
                if let Some(path) = local {
                    cache_local(&path, &encoded);
                }
                value
            };
            self.admit_data(
                key,
                Cached {
                    value,
                    reference: None,
                    _charge: None,
                },
            )
        };
        let Data::Slots { bytes, .. } = &frame.value else {
            bail!("invalid cached chunk")
        };
        ensure!(
            bytes.len() == length as usize && offset <= end && end <= bytes.len(),
            "chunk read out of range"
        );
        Ok(bytes[offset..end].to_vec())
    }
    pub(super) fn plan_packs(
        &self,
        entries: &[SourceEntry],
        contents: &mut BTreeMap<String, SourceContent>,
        staging: &std::path::Path,
        previous: BTreeMap<String, Packed>,
    ) -> anyhow::Result<BTreeMap<String, PathBuf>> {
        let mut grouped: BTreeMap<Vec<u8>, BTreeSet<String>> = BTreeMap::new();
        let mut verified = BTreeMap::new();
        let mut assigned = BTreeSet::new();
        for entry in entries {
            let Some(digest) = &entry.content else {
                continue;
            };
            let content = contents.get_mut(digest).context("unknown source content")?;
            if content.size == 0
                || content.size > SMALL_FILE_BYTES
                || !assigned.insert(digest.clone())
            {
                continue;
            }
            if let Some(packed) = previous.get(digest) {
                let available = if let Some(available) = verified.get(&packed.blob.digest) {
                    *available
                } else {
                    let available =
                        if let Some(bytes) = self.storage.get(&pack_key(&packed.blob))? {
                            ensure!(
                                bytes.len() as u64 == packed.blob.bytes
                                    && frames::digest(&bytes) == packed.blob.digest,
                                "reused data blob digest mismatch"
                            );
                            true
                        } else {
                            false
                        };
                    verified.insert(packed.blob.digest, available);
                    available
                };
                if available {
                    self.read_packed(
                        packed,
                        &frames::parse_digest(digest)?,
                        content.size,
                        0,
                        content.size,
                    )?;
                    content.packed = Some(packed.clone());
                    continue;
                }
            }
            let parent = entry
                .path
                .iter()
                .rposition(|b| *b == b'/')
                .map_or(&[][..], |i| &entry.path[..i]);
            grouped
                .entry(parent.to_vec())
                .or_default()
                .insert(digest.clone());
        }

        let mut output = BTreeMap::new();
        for digests in grouped.into_values() {
            let mut group = Vec::new();
            let mut bytes = 0;
            for digest in digests {
                let size = contents[&digest].size as usize;
                if bytes + size > PACK_BYTES || group.len() >= 4096 {
                    finish_pack(&mut output, contents, staging, &group)?;
                    group.clear();
                    bytes = 0;
                }
                bytes += size;
                group.push(digest);
            }
            if !group.is_empty() {
                finish_pack(&mut output, contents, staging, &group)?;
            }
        }
        Ok(output)
    }
}
fn finish_pack(
    output: &mut BTreeMap<String, PathBuf>,
    contents: &mut BTreeMap<String, SourceContent>,
    staging: &std::path::Path,
    digests: &[String],
) -> anyhow::Result<()> {
    let (bytes, positions) = pack(staging, digests)?;
    let key = pack_key(&positions[0].blob);
    for (digest, location) in digests.iter().zip(positions) {
        contents.get_mut(digest).unwrap().packed = Some(location);
    }
    let directory = staging.join("packed");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(crate::util::encode_hex(&frames::digest(&bytes)));
    std::fs::write(&path, bytes)?;
    output.insert(key, path);
    Ok(())
}
fn validate_slots(count: u64, offsets: &[u32], bytes: &[u8]) -> anyhow::Result<()> {
    ensure!(
        offsets.len() as u64 == count + 1
            && offsets.first() == Some(&0)
            && offsets.last().copied().map(|v| v as usize) == Some(bytes.len())
            && offsets.windows(2).all(|w| w[0] < w[1]),
        "invalid slot offsets"
    );
    Ok(())
}

fn decode_chunk(encoded: &[u8], digest: &Digest, length: u32) -> anyhow::Result<Data> {
    ensure!(
        encoded.len() >= 25 && encoded.len() <= frames::MAX_FRAME_ENCODED + 25,
        "cache data digest mismatch or invalid encoded chunk length"
    );
    // Single-frame seekable object. Its plaintext digest is authenticated
    // by the file's chunk index; no untrusted seek table becomes authority.
    let tail = &encoded[encoded.len() - 25..];
    ensure!(
        tail[..4] == 0x184d_2a5eu32.to_le_bytes()
            && tail[4..8] == 17u32.to_le_bytes()
            && tail[16..20] == 1u32.to_le_bytes()
            && tail[20] == 0
            && tail[21..] == 0x8f92_eab1u32.to_le_bytes(),
        "invalid chunk seek table"
    );
    let csize = u32::from_le_bytes(tail[8..12].try_into()?) as usize;
    let dsize = u32::from_le_bytes(tail[12..16].try_into()?) as usize;
    ensure!(
        csize == encoded.len() - 25 && dsize <= frames::MAX_FRAME_RAW,
        "invalid chunk frame bounds"
    );
    let raw = frames::decompress(&encoded[..csize], dsize)?;
    ensure!(raw.len() == dsize, "chunk decoded length mismatch");
    let value: Data = frames::decode(&raw)?;
    let Data::Slots { offsets, bytes } = &value else {
        bail!("invalid chunk payload")
    };
    validate_slots(1, offsets, bytes)?;
    ensure!(
        bytes.len() == length as usize && frames::digest(bytes) == *digest,
        "cache data digest mismatch or invalid length"
    );

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn random_small_reads_do_not_require_large_decoded_pack_frames() {
        let staging = tempfile::tempdir().unwrap();
        let mut digests = Vec::new();
        for value in 0u8..64 {
            let bytes = vec![value; 1024];
            let digest = hash(&bytes);
            std::fs::write(staging.path().join(&digest[7..]), bytes).unwrap();
            digests.push(digest);
        }
        let (encoded, positions) = pack(staging.path(), &digests).unwrap();
        let layout = &positions[0].blob.layout;
        let Data::Directory(references) = frames::unpack::<Data>(
            layout,
            &encoded[layout.offset as usize..layout.offset as usize + layout.encoded as usize],
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(references.len(), 4);
        for reference in references {
            let Data::Slots { offsets, bytes } = frames::unpack::<Data>(
                &reference,
                &encoded[reference.offset as usize
                    ..reference.offset as usize + reference.encoded as usize],
            )
            .unwrap() else {
                panic!()
            };
            validate_slots(reference.count, &offsets, &bytes).unwrap();
            assert!(bytes.len() <= DATA_FRAME_BYTES);
        }
    }
}
