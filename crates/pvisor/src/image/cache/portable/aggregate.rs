//! Canonical Image Pack V1 content placement.
use super::binary::{SourceContent, SourceEntry};
use super::content::{
    DATA_FRAME_BYTES, Data, MAX_PACK_FRAME_RAW, SMALL_FILE_BYTES, Slot, validate_slots,
};
use super::frames::{self, Digest, FrameRef};
use super::*;
use bincode::{Decode, Encode};
use std::{collections::BTreeSet, fs};

const MAX_BLOB_FRAMES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub(super) struct Blob {
    digest: Digest,
    bytes: u64,
}

impl Blob {
    fn key(&self) -> String {
        let hex = crate::util::encode_hex(&self.digest);
        format!("data/blobs/{}/{}/{hex}", &hex[..2], &hex[2..4])
    }
}

#[derive(Clone, Debug, Encode, Decode)]
pub(super) struct Packed {
    blob: Blob,
    frame: FrameRef,
    slot: u32,
}

impl Packed {
    #[cfg(test)]
    pub(super) fn object(&self) -> (String, u64) {
        (self.blob.key(), self.blob.bytes)
    }
}

#[derive(Clone, Debug, Encode, Decode)]
pub(super) struct Chunk {
    blob: Blob,
    frame: FrameRef,
    digest: Digest,
    pub bytes: u32,
}

impl Chunk {
    pub(super) fn allocation_bytes() -> usize {
        std::mem::size_of::<Self>()
    }
}

#[derive(Default)]
pub(super) struct Plan {
    small: BTreeMap<Vec<u8>, BTreeMap<String, Packed>>,
    pub large: BTreeMap<String, Vec<Chunk>>,
    pub uploads: BTreeMap<String, PathBuf>,
    pub objects: BTreeMap<String, u64>,
}

impl Plan {
    pub(super) fn small(&self, path: &[u8], digest: &str) -> &Packed {
        &self.small[parent(path)][digest]
    }
}

fn parent(path: &[u8]) -> &[u8] {
    path.iter()
        .rposition(|b| *b == b'/')
        .map_or(&[], |i| &path[..i])
}

enum Placement {
    Small {
        directory: Vec<u8>,
        files: Vec<String>,
    },
    Large(Digest, u32),
}

struct Pending {
    reference: FrameRef,
    raw: usize,
    placement: Placement,
}

#[derive(Default)]
struct Tail {
    frames: Vec<Pending>,
    raw: usize,
    encoded: usize,
}

impl Tail {
    fn append(
        &mut self,
        frame: Pending,
        writer: &mut Planner,
        mut chunks: Option<&mut Vec<Chunk>>,
    ) -> anyhow::Result<()> {
        if !self.frames.is_empty()
            && (self.raw + frame.raw > writer.target
                || self.encoded
                    + frame.reference.encoded as usize
                    + frames::seek_table_bytes(self.frames.len() + 1)
                    > MAX_OBJECT)
        {
            writer.seal(std::mem::take(self), chunks.as_deref_mut())?;
        }
        self.raw += frame.raw;
        self.encoded += frame.reference.encoded as usize;
        self.frames.push(frame);
        if self.raw == writer.target || self.frames.len() == MAX_BLOB_FRAMES {
            writer.seal(std::mem::take(self), chunks)?;
        }
        Ok(())
    }
}

struct Planner {
    staging: PathBuf,
    target: usize,
    output: Plan,
}

impl Planner {
    fn stage(
        &self,
        offsets: Vec<u32>,
        bytes: Vec<u8>,
        placement: Placement,
    ) -> anyhow::Result<Pending> {
        let raw = bytes.len();
        let count = offsets.len() - 1;
        let (reference, encoded) = frames::compress(&Data::Slots { offsets, bytes }, count as u64)?;
        let path = self.frame_path(&reference);
        if !path.try_exists()? {
            fs::write(&path, encoded)?;
        }
        Ok(Pending {
            reference,
            raw,
            placement,
        })
    }

    fn frame_path(&self, reference: &FrameRef) -> PathBuf {
        self.staging
            .join("frames")
            .join(crate::util::encode_hex(&reference.digest))
    }

    fn seal(&mut self, tail: Tail, mut chunks: Option<&mut Vec<Chunk>>) -> anyhow::Result<()> {
        if tail.frames.is_empty() {
            return Ok(());
        }
        let mut writer = frames::Writer::default();
        let mut refs = Vec::with_capacity(tail.frames.len());
        for frame in tail.frames {
            let encoded = fs::read(self.frame_path(&frame.reference))?;
            refs.push((writer.append(frame.reference, &encoded)?, frame.placement));
        }
        let bytes = writer.finish()?;
        let blob = Blob {
            digest: frames::digest(&bytes),
            bytes: bytes.len() as u64,
        };
        let key = blob.key();
        if !self.output.uploads.contains_key(&key) {
            let path = self
                .staging
                .join("blobs")
                .join(crate::util::encode_hex(&blob.digest));
            fs::write(&path, bytes)?;
            self.output.uploads.insert(key.clone(), path);
            self.output.objects.insert(key, blob.bytes);
        }
        for (frame, placement) in refs {
            match placement {
                Placement::Small { directory, files } => {
                    let placements = self.output.small.entry(directory).or_default();
                    for (slot, digest) in files.into_iter().enumerate() {
                        placements.insert(
                            digest,
                            Packed {
                                blob: blob.clone(),
                                frame: frame.clone(),
                                slot: slot as u32,
                            },
                        );
                    }
                }
                Placement::Large(digest, bytes) => {
                    chunks
                        .as_deref_mut()
                        .context("missing large file placement")?
                        .push(Chunk {
                            blob: blob.clone(),
                            frame,
                            digest,
                            bytes,
                        });
                }
            }
        }
        Ok(())
    }

    fn directory(&mut self, directory: &[u8], files: &[&str]) -> anyhow::Result<Tail> {
        let mut tail = Tail::default();
        let mut seen = BTreeSet::new();
        let mut slots = Vec::new();
        let mut offsets = vec![0];
        let mut data = Vec::new();
        for &file in files {
            if !seen.insert(file) {
                continue;
            }
            let bytes = fs::read(self.staging.join(&file[7..]))?;
            if !slots.is_empty()
                && (data.len() + bytes.len() > DATA_FRAME_BYTES || slots.len() == 1024)
            {
                let frame = self.stage(
                    std::mem::replace(&mut offsets, vec![0]),
                    std::mem::take(&mut data),
                    Placement::Small {
                        directory: directory.to_vec(),
                        files: std::mem::take(&mut slots),
                    },
                )?;
                tail.append(frame, self, None)?;
            }
            data.extend_from_slice(&bytes);
            offsets.push(data.len() as u32);
            slots.push(file.into());
        }
        if !slots.is_empty() {
            let frame = self.stage(
                offsets,
                data,
                Placement::Small {
                    directory: directory.to_vec(),
                    files: slots,
                },
            )?;
            tail.append(frame, self, None)?;
        }
        Ok(tail)
    }
}

pub(super) fn plan(
    entries: &[SourceEntry],
    contents: &BTreeMap<String, SourceContent>,
    staging: &Path,
    packing: format::Packing,
) -> anyhow::Result<Plan> {
    fs::create_dir(staging.join("frames"))?;
    fs::create_dir(staging.join("blobs"))?;
    let mut writer = Planner {
        staging: staging.into(),
        target: packing.target(),
        output: Plan::default(),
    };
    let mut directories = BTreeMap::<&[u8], Vec<&str>>::new();
    for entry in entries {
        match &entry.metadata {
            Response::Metadata { kind, .. } if kind == "directory" => {
                directories.entry(&entry.path).or_default();
            }
            _ => {
                if let Some(digest) = &entry.content
                    && (1..=SMALL_FILE_BYTES).contains(&contents[digest].size)
                {
                    directories
                        .entry(parent(&entry.path))
                        .or_default()
                        .push(digest);
                }
            }
        }
    }
    // Reverse lexical order visits descendants first; reversing each sibling
    // list restores byte order when their unsealed tails reach the parent.
    let mut children = BTreeMap::<&[u8], Vec<Tail>>::new();
    for (directory, files) in directories.into_iter().rev() {
        let mut tail = writer.directory(directory, &files)?;
        for child in children.remove(directory).into_iter().flatten().rev() {
            for frame in child.frames {
                tail.append(frame, &mut writer, None)?;
            }
        }
        if directory.is_empty() {
            writer.seal(tail, None)?;
        } else if !tail.frames.is_empty() {
            children.entry(parent(directory)).or_default().push(tail);
        }
    }
    for (file, content) in contents {
        if content.size <= SMALL_FILE_BYTES {
            continue;
        }
        let mut chunks = Vec::with_capacity(content.chunks.len());
        let mut tail = Tail::default();
        for (digest, length) in &content.chunks {
            let bytes = fs::read(staging.join(&digest[7..]))?;
            let frame = writer.stage(
                vec![0, *length],
                bytes,
                Placement::Large(frames::parse_digest(digest)?, *length),
            )?;
            tail.append(frame, &mut writer, Some(&mut chunks))?;
        }
        writer.seal(tail, Some(&mut chunks))?;
        writer.output.large.insert(file.clone(), chunks);
    }
    Ok(writer.output)
}

impl PortableCache {
    pub(super) fn read_v1_packed(
        &self,
        packed: &Packed,
        digest: &Digest,
        size: u64,
        offset: u64,
        end: u64,
    ) -> anyhow::Result<Slot> {
        self.read_v1_slot(
            &packed.blob,
            &packed.frame,
            packed.slot,
            digest,
            size,
            offset,
            end,
            MAX_PACK_FRAME_RAW,
        )
    }

    pub(super) fn read_v1_chunk(
        &self,
        chunk: &Chunk,
        offset: u64,
        end: u64,
    ) -> anyhow::Result<Slot> {
        ensure!(
            chunk.frame.count == 1,
            "invalid large-file frame slot count"
        );
        self.read_v1_slot(
            &chunk.blob,
            &chunk.frame,
            0,
            &chunk.digest,
            u64::from(chunk.bytes),
            offset,
            end,
            frames::MAX_FRAME_RAW,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn read_v1_slot(
        &self,
        blob: &Blob,
        frame: &FrameRef,
        slot: u32,
        digest: &Digest,
        size: u64,
        offset: u64,
        end: u64,
        limit: usize,
    ) -> anyhow::Result<Slot> {
        let decoded = self.load_data_frame(&blob.key(), blob.bytes, frame, limit)?;
        let range = slot_range(&decoded.value, slot)?;
        let value = slot_bytes(&decoded.value, slot)?;
        ensure!(value.len() as u64 == size, "blob slot length mismatch");
        decoded.verify_slot(slot, digest, value)?;
        ensure!(
            offset <= end && end <= size,
            "blob slot range out of bounds"
        );
        Ok(Slot::new(
            decoded,
            range.start + offset as usize,
            range.start + end as usize,
        ))
    }
}

fn slot_range(data: &Data, slot: u32) -> anyhow::Result<std::ops::Range<usize>> {
    let Data::Slots { offsets, .. } = data else {
        bail!("invalid Image Pack slot frame")
    };
    let slot = slot as usize;
    ensure!(
        slot < offsets.len().saturating_sub(1),
        "blob slot out of range"
    );
    Ok(offsets[slot] as usize..offsets[slot + 1] as usize)
}

fn slot_bytes(data: &Data, slot: u32) -> anyhow::Result<&[u8]> {
    let Data::Slots { bytes, .. } = data else {
        bail!("invalid Image Pack slot frame")
    };
    Ok(&bytes[slot_range(data, slot)?])
}

#[derive(Default)]
pub(super) struct Reachable(BTreeMap<String, (Blob, BTreeMap<u64, ReachableFrame>)>);

struct ReachableFrame {
    frame: FrameRef,
    slots: BTreeMap<u32, (Digest, u64)>,
}

impl Reachable {
    pub(super) fn packed(
        &mut self,
        packed: Packed,
        digest: Digest,
        size: u64,
    ) -> anyhow::Result<()> {
        self.add(packed.blob, packed.frame, packed.slot, digest, size)
    }

    pub(super) fn chunk(&mut self, chunk: Chunk) -> anyhow::Result<()> {
        ensure!(
            chunk.frame.count == 1,
            "invalid large-file frame slot count"
        );
        self.add(
            chunk.blob,
            chunk.frame,
            0,
            chunk.digest,
            u64::from(chunk.bytes),
        )
    }

    fn add(
        &mut self,
        blob: Blob,
        frame: FrameRef,
        slot: u32,
        digest: Digest,
        size: u64,
    ) -> anyhow::Result<()> {
        let (stored, frames) = self
            .0
            .entry(blob.key())
            .or_insert_with(|| (blob.clone(), BTreeMap::new()));
        ensure!(*stored == blob, "inconsistent blob reference");
        let stored = frames
            .entry(frame.offset)
            .or_insert_with(|| ReachableFrame {
                frame: frame.clone(),
                slots: BTreeMap::new(),
            });
        ensure!(stored.frame == frame, "inconsistent blob frame reference");
        let value = (digest, size);
        let first = stored.slots.entry(slot).or_insert(value);
        ensure!(*first == value, "inconsistent blob slot reference");
        Ok(())
    }

    pub(super) fn inventory(&self) -> impl Iterator<Item = (&str, u64)> {
        self.0
            .iter()
            .map(|(key, (blob, _))| (key.as_str(), blob.bytes))
    }

    pub(super) fn verify(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
        let (blob, reachable) = self.0.get(key).context("unreachable exported blob")?;
        ensure!(
            bytes.len() as u64 == blob.bytes && frames::digest(bytes) == blob.digest,
            "blob object digest mismatch"
        );
        let (payload, table) = frames::seek_table(bytes)?;
        ensure!(
            table.len() == reachable.len() && table.len() <= MAX_BLOB_FRAMES,
            "unreachable or missing blob frame"
        );
        let mut offset = 0;
        for (row, reference) in table.iter().zip(reachable.values()) {
            let frame = &reference.frame;
            let encoded = u32::from_le_bytes(row[..4].try_into()?);
            let decoded = u32::from_le_bytes(row[4..].try_into()?);
            ensure!(
                frame.offset == offset && frame.encoded == encoded && frame.decoded == decoded,
                "blob seek table disagrees with index"
            );
            frames::validate(frame, payload.len() as u64, frames::MAX_FRAME_RAW)?;
            let end = offset + u64::from(frame.encoded);
            let data: Data = frames::unpack(frame, &payload[offset as usize..end as usize])?;
            let Data::Slots { offsets, bytes } = &data else {
                bail!("invalid Image Pack slot frame")
            };
            validate_slots(frame.count, offsets, bytes)?;
            ensure!(
                reference.slots.len() as u64 == frame.count,
                "unreachable or missing blob slot"
            );
            for (&slot, (digest, size)) in &reference.slots {
                let value = slot_bytes(&data, slot)?;
                ensure!(
                    value.len() as u64 == *size && frames::digest(value) == *digest,
                    "blob slot digest or length mismatch"
                );
            }
            offset = end;
        }
        ensure!(
            offset == payload.len() as u64,
            "unindexed trailing blob bytes"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case::missing_frame(true)]
    #[case::missing_slot(false)]
    fn refuses_unreachable_data_even_with_the_correct_blob_digest(#[case] missing_frame: bool) {
        let mut writer = frames::Writer::default();
        let public = writer
            .push(
                &Data::Slots {
                    offsets: if missing_frame {
                        vec![0, 1]
                    } else {
                        vec![0, 1, 2]
                    },
                    bytes: if missing_frame { vec![1] } else { vec![1, 2] },
                },
                if missing_frame { 1 } else { 2 },
            )
            .unwrap();
        if missing_frame {
            writer
                .push(
                    &Data::Slots {
                        offsets: vec![0, 1],
                        bytes: vec![2],
                    },
                    1,
                )
                .unwrap();
        }
        let bytes = writer.finish().unwrap();
        let blob = Blob {
            digest: frames::digest(&bytes),
            bytes: bytes.len() as u64,
        };
        let key = blob.key();
        let mut reachable = Reachable::default();
        reachable
            .packed(
                Packed {
                    blob,
                    frame: public,
                    slot: 0,
                },
                frames::digest(&[1]),
                1,
            )
            .unwrap();
        let error = reachable.verify(&key, &bytes).unwrap_err();
        assert_eq!(
            error.to_string(),
            if missing_frame {
                "unreachable or missing blob frame"
            } else {
                "unreachable or missing blob slot"
            }
        );
    }
}
