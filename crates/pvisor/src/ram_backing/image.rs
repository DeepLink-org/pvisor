//! Immutable RAM generations. The payload is a standard Zstd Seekable stream;
//! RAM addressing, inheritance and publication belong to the enclosing format.
use super::{BLOCK_BYTES, hash, invalid};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const FOOTER_MAGIC: &[u8; 8] = b"PVSNAP2\0";
const FOOTER_BYTES: u64 = 64;
const SEEK_MAGIC: u32 = 0x184d2a5e;
const SEEK_FOOTER_MAGIC: u32 = 0x8f92eab1;
const MAX_METADATA: u64 = 64 * 1024 * 1024;
const MAX_RAM: u64 = 64 * 1024 * 1024 * 1024;
const MAX_CHAIN: usize = 32;
const COMPACT_DEPTH: usize = 8;
pub type ImageId = [u8; 32];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RamRegion {
    pub guest_address: u64,
    pub length: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RamLayout {
    pub page_bytes: u32,
    pub regions: Vec<RamRegion>,
}

impl RamLayout {
    pub fn validate(&self) -> io::Result<()> {
        if !self.page_bytes.is_power_of_two()
            || !(4096..=65536).contains(&self.page_bytes)
            || self.regions.is_empty()
            || self.regions.len() > 1024
        {
            return Err(invalid("invalid RAM layout"));
        }
        let mut previous_end = 0;
        let mut total = 0u64;
        for region in &self.regions {
            let end = region
                .guest_address
                .checked_add(region.length)
                .ok_or_else(|| invalid("RAM address overflow"))?;
            total = total
                .checked_add(region.length)
                .ok_or_else(|| invalid("RAM size overflow"))?;
            if region.length == 0 || region.guest_address < previous_end || total > MAX_RAM {
                return Err(invalid("overlapping or oversized RAM regions"));
            }
            previous_end = end;
        }
        Ok(())
    }

    pub fn logical_bytes(&self) -> u64 {
        self.regions.iter().map(|region| region.length).sum()
    }

    pub fn block_count(&self) -> u64 {
        self.regions
            .iter()
            .map(|region| region.length.div_ceil(BLOCK_BYTES as u64))
            .sum()
    }

    /// Translate a Guest address to a stable block and byte offset. Address
    /// holes are errors, not implicit zero RAM.
    pub fn guest_location(&self, address: u64) -> io::Result<(u64, usize)> {
        self.validate()?;
        let mut first = 0;
        for region in &self.regions {
            if let Some(within) = address
                .checked_sub(region.guest_address)
                .filter(|within| *within < region.length)
            {
                return Ok((
                    first + within / BLOCK_BYTES as u64,
                    (within % BLOCK_BYTES as u64) as usize,
                ));
            }
            first += region.length.div_ceil(BLOCK_BYTES as u64);
        }
        Err(invalid("Guest address outside RAM"))
    }

    /// Compact logical-file offset and size of a block. Blocks never cross regions.
    pub fn block_span(&self, mut block: u64) -> io::Result<(u64, usize)> {
        let mut offset = 0;
        for region in &self.regions {
            let count = region.length.div_ceil(BLOCK_BYTES as u64);
            if block < count {
                let within = block * BLOCK_BYTES as u64;
                return Ok((
                    offset + within,
                    (region.length - within).min(BLOCK_BYTES as u64) as usize,
                ));
            }
            block -= count;
            offset += region.length;
        }
        Err(invalid("block outside RAM layout"))
    }

    fn locate(&self, mut offset: u64) -> io::Result<(u64, usize)> {
        let mut block = 0;
        for region in &self.regions {
            if offset < region.length {
                return Ok((
                    block + offset / BLOCK_BYTES as u64,
                    (offset % BLOCK_BYTES as u64) as usize,
                ));
            }
            offset -= region.length;
            block += region.length.div_ceil(BLOCK_BYTES as u64);
        }
        Err(invalid("offset outside RAM layout"))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    block: u64,
    frame: Option<u32>,
    fill: Option<u8>,
    checksum: ImageId,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    version: u32,
    block_bytes: u32,
    parent: Option<ImageId>,
    layout: RamLayout,
    entries: Vec<Entry>,
}

struct Layer {
    file: File,
    id: ImageId,
    metadata: Metadata,
    frames: Vec<(u64, u32, u32)>, // offset, compressed length, decoded length
}

fn name(id: &ImageId) -> String {
    let hex: String = id.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{hex}.pvdelta")
}

impl Layer {
    fn open(directory: &Path, id: ImageId) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(directory.join(name(&id)))?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid("RAM generation is not a regular file"));
        }
        let length = metadata.len();
        let mut footer = [0; FOOTER_BYTES as usize];
        file.read_exact_at(
            &mut footer,
            length
                .checked_sub(FOOTER_BYTES)
                .ok_or_else(|| invalid("truncated RAM image"))?,
        )?;
        let index_offset = u64_at(&footer, 8);
        let index_bytes = u64_at(&footer, 16);
        let seek_bytes = u64_at(&footer, 24);
        if &footer[..8] != FOOTER_MAGIC
            || &footer[32..] != id.as_slice()
            || index_bytes > MAX_METADATA
            || seek_bytes < 17
            || index_offset < seek_bytes
            || index_offset
                .checked_add(index_bytes)
                .and_then(|end| end.checked_add(FOOTER_BYTES))
                != Some(length)
        {
            return Err(invalid("invalid RAM image footer"));
        }
        let mut bytes = vec![0; index_bytes as usize];
        file.read_exact_at(&mut bytes, index_offset)?;
        if hash(&bytes) != id {
            return Err(invalid("RAM image metadata checksum mismatch"));
        }
        let metadata: Metadata =
            serde_json::from_slice(&bytes).map_err(|_| invalid("invalid RAM image metadata"))?;
        metadata.layout.validate()?;
        if metadata.version != 2
            || metadata.block_bytes != BLOCK_BYTES as u32
            || metadata.entries.len() as u64 > metadata.layout.block_count()
        {
            return Err(invalid("unsupported RAM image geometry"));
        }

        let mut seek_footer = [0; 9];
        file.read_exact_at(&mut seek_footer, index_offset - 9)?;
        let count = u32_at(&seek_footer, 0) as u64;
        if seek_footer[4] != 0
            || u32_at(&seek_footer, 5) != SEEK_FOOTER_MAGIC
            || count > metadata.entries.len() as u64
            || seek_bytes != 17 + count * 8
        {
            return Err(invalid("invalid Zstd seek footer"));
        }
        let mut seek_header = [0; 8];
        let seek_start = index_offset - seek_bytes;
        file.read_exact_at(&mut seek_header, seek_start)?;
        if u32_at(&seek_header, 0) != SEEK_MAGIC || u32_at(&seek_header, 4) as u64 != seek_bytes - 8
        {
            return Err(invalid("invalid Zstd seek table"));
        }
        let mut frames = Vec::with_capacity(count as usize);
        let mut position = 0u64;
        for frame in 0..count {
            let mut entry = [0; 8];
            file.read_exact_at(&mut entry, seek_start + 8 + frame * 8)?;
            let stored = u32_at(&entry, 0);
            let decoded = u32_at(&entry, 4);
            if stored == 0
                || stored as usize > BLOCK_BYTES + 1024
                || decoded == 0
                || decoded as usize > BLOCK_BYTES
            {
                return Err(invalid("invalid Zstd frame bounds"));
            }
            frames.push((position, stored, decoded));
            position += stored as u64;
        }
        if position != seek_start {
            return Err(invalid("Zstd frames do not cover payload"));
        }
        let mut previous = None;
        let mut next_frame = 0;
        for entry in &metadata.entries {
            if previous.is_some_and(|block| entry.block <= block) {
                return Err(invalid("unsorted or duplicate block index"));
            }
            let (_, decoded) = metadata.layout.block_span(entry.block)?;
            match (entry.frame, entry.fill) {
                (Some(frame), None)
                    if frame == next_frame
                        && frames
                            .get(frame as usize)
                            .is_some_and(|item| item.2 as usize == decoded) =>
                {
                    next_frame += 1
                }
                (None, Some(_)) => {}
                _ => return Err(invalid("invalid RAM block descriptor")),
            }
            previous = Some(entry.block);
        }
        if next_frame as usize != frames.len() {
            return Err(invalid("unreferenced frame"));
        }
        Ok(Self {
            file,
            id,
            metadata,
            frames,
        })
    }

    fn read(&self, entry: &Entry) -> io::Result<Vec<u8>> {
        let (_, length) = self.metadata.layout.block_span(entry.block)?;
        let output = if let Some(fill) = entry.fill {
            vec![fill; length]
        } else {
            let (offset, stored, _) = self.frames[entry.frame.unwrap() as usize];
            let mut input = vec![0; stored as usize];
            self.file.read_exact_at(&mut input, offset)?;
            if zstd::zstd_safe::find_frame_compressed_size(&input)
                .map_err(|_| invalid("invalid Zstd frame"))?
                != input.len()
            {
                return Err(invalid("trailing Zstd frame data"));
            }
            let mut decoder = zstd::bulk::Decompressor::new()?;
            decoder.window_log_max(16)?;
            let output = decoder
                .decompress(&input, length)
                .map_err(|_| invalid("invalid Zstd payload"))?;
            if output.len() != length {
                return Err(invalid("wrong decoded block size"));
            }
            output
        };
        if hash(&output) != entry.checksum {
            return Err(invalid("RAM block checksum mismatch"));
        }
        Ok(output)
    }
}

/// Resolved immutable chain. Callers retain its files and exclude external mutation.
pub struct SnapshotChain {
    directory: PathBuf,
    layers: Vec<Layer>,
    resolved: BTreeMap<u64, (usize, Entry)>,
}

impl SnapshotChain {
    pub fn open(directory: &Path, head: ImageId) -> io::Result<Self> {
        let mut layers = Vec::new();
        let mut seen = BTreeSet::new();
        let mut next = Some(head);
        while let Some(id) = next {
            if layers.len() >= MAX_CHAIN || !seen.insert(id) {
                return Err(invalid("RAM chain too deep or cyclic"));
            }
            let layer = Layer::open(directory, id)?;
            if layers
                .first()
                .is_some_and(|head: &Layer| head.metadata.layout != layer.metadata.layout)
            {
                return Err(invalid("parent RAM layout mismatch"));
            }
            next = layer.metadata.parent;
            layers.push(layer);
        }
        layers.reverse();
        let mut resolved = BTreeMap::new();
        for (index, layer) in layers.iter().enumerate() {
            if index == 0
                && (layer.metadata.entries.len() as u64 != layer.metadata.layout.block_count()
                    || layer
                        .metadata
                        .entries
                        .iter()
                        .enumerate()
                        .any(|(block, entry)| entry.block != block as u64))
            {
                return Err(invalid("base image is incomplete"));
            }
            for entry in &layer.metadata.entries {
                resolved.insert(entry.block, (index, entry.clone()));
            }
        }
        Ok(Self {
            directory: directory.canonicalize()?,
            layers,
            resolved,
        })
    }

    /// Keep this historical head across collection. Pin/unpin/capture/collection
    /// must be serialized by the directory owner; these are not interprocess locks.
    pub fn pin(&self) -> io::Result<()> {
        let path = self
            .directory
            .join(name(&self.head_id()))
            .with_extension("pvpin");
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(file) => file.sync_all()?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        File::open(&self.directory)?.sync_all()
    }

    pub fn unpin(&self) -> io::Result<()> {
        let path = self
            .directory
            .join(name(&self.head_id()))
            .with_extension("pvpin");
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        File::open(&self.directory)?.sync_all()
    }

    /// Collect only canonical generation files unreachable from this head or
    /// durable pins. Resolve every root before deleting anything. The caller
    /// must publish/fsync this head first and exclusively own the directory.
    pub fn collect_unreachable(&self) -> io::Result<usize> {
        let entries = std::fs::read_dir(&self.directory)?.collect::<io::Result<Vec<_>>>()?;
        let mut live: BTreeSet<_> = self.layers.iter().map(|layer| layer.id).collect();
        for entry in &entries {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "pvpin") {
                let id = parse_id(&path).ok_or_else(|| invalid("invalid RAM pin name"))?;
                let chain = Self::open(&self.directory, id)?;
                live.extend(chain.layers.iter().map(|layer| layer.id));
            }
        }
        let mut removed = 0;
        for entry in entries {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "pvdelta")
                && let Some(id) = parse_id(&path)
                && !live.contains(&id)
            {
                std::fs::remove_file(path)?;
                removed += 1;
            }
        }
        File::open(&self.directory)?.sync_all()?;
        Ok(removed)
    }

    /// `dirty` must include every changed block, including device writes. The
    /// reader must observe a stable epoch. Commit does not quiesce a VM itself.
    pub fn capture(
        directory: &Path,
        layout: &RamLayout,
        parent: Option<&Self>,
        dirty: &BTreeSet<u64>,
        mut read: impl FnMut(u64, &mut [u8]) -> io::Result<()>,
    ) -> io::Result<Self> {
        layout.validate()?;
        let directory = directory.canonicalize()?;
        if parent.is_some_and(|chain| chain.layout() != layout || chain.directory != directory)
            || dirty.iter().any(|block| *block >= layout.block_count())
        {
            return Err(invalid("invalid parent or dirty block set"));
        }
        // ponytail: flatten every eight layers; tune after measuring restore cost.
        // Old generations remain owned by their caller; no implicit deletion.
        let delta = parent.filter(|chain| chain.depth() < COMPACT_DEPTH);
        let blocks: Box<dyn Iterator<Item = u64> + '_> = if delta.is_some() {
            Box::new(dirty.iter().copied())
        } else {
            Box::new(0..layout.block_count())
        };
        let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd;
            if unsafe { libc::fcntl(temporary.as_file().as_raw_fd(), libc::F_NOCACHE, 1) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let mut entries = Vec::new();
        let mut sizes = Vec::new();
        let mut payload_bytes = 0;
        for block in blocks {
            let (_, length) = layout.block_span(block)?;
            let mut data = vec![0; length];
            read(block, &mut data)?;
            let checksum = hash(&data);
            if delta.is_some_and(|chain| chain.resolved[&block].1.checksum == checksum) {
                continue;
            }
            let (frame, fill) = if data.iter().all(|byte| *byte == data[0]) {
                (None, Some(data[0]))
            } else {
                // Zstd also encodes incompressible data with raw internal blocks;
                // keeping every payload a frame preserves Seekable compatibility.
                let encoded = zstd::bulk::compress(&data, 1)?;
                temporary.write_all(&encoded)?;
                let frame = sizes.len() as u32;
                sizes.push((encoded.len() as u32, length as u32));
                payload_bytes += encoded.len() as u64;
                (Some(frame), None)
            };
            entries.push(Entry {
                block,
                frame,
                fill,
                checksum,
            });
        }
        if entries.is_empty()
            && let Some(delta) = delta
        {
            return Self::open(&directory, delta.head_id());
        }
        let seek_bytes = 17 + sizes.len() as u64 * 8;
        temporary.write_all(&SEEK_MAGIC.to_le_bytes())?;
        temporary.write_all(&((seek_bytes - 8) as u32).to_le_bytes())?;
        for (stored, decoded) in &sizes {
            temporary.write_all(&stored.to_le_bytes())?;
            temporary.write_all(&decoded.to_le_bytes())?;
        }
        temporary.write_all(&(sizes.len() as u32).to_le_bytes())?;
        temporary.write_all(&[0])?; // Seekable checksum flag off; outer SHA-256 covers blocks.
        temporary.write_all(&SEEK_FOOTER_MAGIC.to_le_bytes())?;
        let metadata = Metadata {
            version: 2,
            block_bytes: BLOCK_BYTES as u32,
            parent: delta.map(Self::head_id),
            layout: layout.clone(),
            entries,
        };
        let bytes = serde_json::to_vec(&metadata).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_METADATA {
            return Err(invalid("RAM index exceeds metadata budget"));
        }
        let id = hash(&bytes);
        temporary.write_all(&bytes)?;
        let mut footer = [0; FOOTER_BYTES as usize];
        footer[..8].copy_from_slice(FOOTER_MAGIC);
        footer[8..16].copy_from_slice(&(payload_bytes + seek_bytes).to_le_bytes());
        footer[16..24].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
        footer[24..32].copy_from_slice(&seek_bytes.to_le_bytes());
        footer[32..].copy_from_slice(&id);
        temporary.write_all(&footer)?;
        temporary.as_file().sync_all()?;
        // Never replace a committed generation. Identical content may reuse one.
        match temporary.persist_noclobber(directory.join(name(&id))) {
            Ok(_) => {}
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                let existing = Layer::open(&directory, id)?;
                for entry in &existing.metadata.entries {
                    existing.read(entry)?;
                }
            }
            Err(error) => return Err(error.error),
        }
        File::open(&directory)?.sync_all()?;
        Self::open(&directory, id)
    }

    pub fn head_id(&self) -> ImageId {
        self.layers.last().unwrap().id
    }
    pub fn depth(&self) -> usize {
        self.layers.len()
    }
    pub fn layout(&self) -> &RamLayout {
        &self.layers[0].metadata.layout
    }
    pub fn changed_blocks(&self) -> usize {
        self.layers.last().unwrap().metadata.entries.len()
    }
    pub fn allocated_bytes(&self) -> io::Result<u64> {
        self.layers.iter().try_fold(0, |total, layer| {
            Ok(total + layer.file.metadata()?.blocks() * 512)
        })
    }
    pub fn read_block(&self, block: u64) -> io::Result<Vec<u8>> {
        let (layer, entry) = self
            .resolved
            .get(&block)
            .ok_or_else(|| invalid("missing RAM block"))?;
        self.layers[*layer].read(entry)
    }
    pub fn read_at(&self, offset: u64, output: &mut [u8]) -> io::Result<usize> {
        let size = self
            .layout()
            .logical_bytes()
            .saturating_sub(offset)
            .min(output.len() as u64) as usize;
        let mut done = 0;
        while done < size {
            let (block, within) = self.layout().locate(offset + done as u64)?;
            let data = self.read_block(block)?;
            let length = (size - done).min(data.len() - within);
            output[done..done + length].copy_from_slice(&data[within..within + length]);
            done += length;
        }
        Ok(size)
    }
    /// Restore compact RAM bytes. The caller must quiesce all destination access.
    pub fn restore_to(&self, file: &File) -> io::Result<()> {
        file.set_len(self.layout().logical_bytes())?;
        for block in 0..self.layout().block_count() {
            file.write_all_at(&self.read_block(block)?, self.layout().block_span(block)?.0)?;
        }
        file.sync_all()
    }
}

fn parse_id(path: &Path) -> Option<ImageId> {
    let stem = path.file_stem()?.to_str()?;
    if stem.len() != 64
        || !stem
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut id = [0; 32];
    for (index, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&stem[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(id)
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn layout() -> RamLayout {
        RamLayout {
            page_bytes: 4096,
            regions: vec![
                RamRegion {
                    guest_address: 0,
                    length: BLOCK_BYTES as u64 + 7,
                },
                RamRegion {
                    guest_address: 1 << 32,
                    length: BLOCK_BYTES as u64,
                },
            ],
        }
    }
    fn capture(
        root: &Path,
        parent: Option<&SnapshotChain>,
        dirty: &[u64],
        data: &[u8],
    ) -> SnapshotChain {
        SnapshotChain::capture(
            root,
            &layout(),
            parent,
            &dirty.iter().copied().collect(),
            |block, out| {
                let offset = layout().block_span(block)?.0 as usize;
                out.copy_from_slice(&data[offset..offset + out.len()]);
                Ok(())
            },
        )
        .unwrap()
    }
    #[test]
    fn delta_inherits_zero_overrides_and_restores_across_regions() {
        let directory = tempfile::tempdir().unwrap();
        let mut data = (0..layout().logical_bytes())
            .map(|i| (i % 251) as u8)
            .collect::<Vec<_>>();
        let base = capture(directory.path(), None, &[], &data);
        let base_id = base.head_id();
        data[..BLOCK_BYTES].fill(0);
        let delta = capture(directory.path(), Some(&base), &[0], &data);
        assert_eq!(delta.depth(), 2);
        assert_eq!(delta.changed_blocks(), 1);
        let mut actual = vec![0; data.len()];
        delta.read_at(0, &mut actual).unwrap();
        assert_eq!(actual, data);
        assert_ne!(base.read_block(0).unwrap(), delta.read_block(0).unwrap());
        let reopened = SnapshotChain::open(directory.path(), delta.head_id()).unwrap();
        let output = tempfile::tempfile().unwrap();
        reopened.restore_to(&output).unwrap();
        output.read_exact_at(&mut actual, 0).unwrap();
        assert_eq!(actual, data);
        assert_eq!(
            SnapshotChain::open(directory.path(), base_id)
                .unwrap()
                .depth(),
            1
        );
        let unchanged = capture(directory.path(), Some(&delta), &[0], &data);
        assert_eq!(unchanged.head_id(), delta.head_id());
        assert_eq!(layout().guest_location(1 << 32).unwrap(), (2, 0));
        assert!(layout().guest_location(1 << 30).is_err());
    }

    #[test]
    fn failed_capture_preserves_parent_and_missing_ancestor_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let mut data = vec![0; layout().logical_bytes() as usize];
        let base = capture(directory.path(), None, &[], &data);
        let result = SnapshotChain::capture(
            directory.path(),
            &layout(),
            Some(&base),
            &BTreeSet::from([0]),
            |_, _| Err(io::Error::other("injected source failure")),
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        assert!(SnapshotChain::open(directory.path(), base.head_id()).is_ok());
        data[0] = 1;
        let delta = capture(directory.path(), Some(&base), &[0], &data);
        std::fs::remove_file(directory.path().join(name(&base.head_id()))).unwrap();
        assert!(SnapshotChain::open(directory.path(), delta.head_id()).is_err());
    }

    #[test]
    fn corrupted_seek_index_is_rejected_before_decoding() {
        let directory = tempfile::tempdir().unwrap();
        let data = (0..layout().logical_bytes())
            .map(|i| (i % 251) as u8)
            .collect::<Vec<_>>();
        let chain = capture(directory.path(), None, &[], &data);
        let layer = &chain.layers[0];
        let file = OpenOptions::new()
            .write(true)
            .open(directory.path().join(name(&chain.head_id())))
            .unwrap();
        let mut footer = [0; 64];
        layer
            .file
            .read_exact_at(&mut footer, file.metadata().unwrap().len() - 64)
            .unwrap();
        let index = u64_at(&footer, 8);
        file.write_all_at(&[1], index - 5).unwrap(); // unsupported seek checksum flag
        assert!(SnapshotChain::open(directory.path(), chain.head_id()).is_err());
    }
    #[test]
    fn bounded_chain_compacts_without_deleting_old_generations() {
        let directory = tempfile::tempdir().unwrap();
        let mut data = vec![0; layout().logical_bytes() as usize];
        let mut chain = capture(directory.path(), None, &[], &data);
        let original = chain.head_id();
        for i in 1..=8 {
            data[0] = i;
            chain = capture(directory.path(), Some(&chain), &[0], &data);
        }
        assert_eq!(chain.depth(), 1);
        assert_eq!(chain.read_block(0).unwrap()[0], 8);
        assert!(SnapshotChain::open(directory.path(), original).is_ok());
    }
    #[test]
    fn collection_preserves_pinned_ancestors_and_rejects_missing_pin_roots() {
        let directory = tempfile::tempdir().unwrap();
        let mut data = vec![0; layout().logical_bytes() as usize];
        let mut chain = capture(directory.path(), None, &[], &data);
        let base = chain.head_id();
        let mut pinned = None;
        for i in 1..=8 {
            data[0] = i;
            chain = capture(directory.path(), Some(&chain), &[0], &data);
            if i == 3 {
                chain.pin().unwrap();
                pinned = Some(chain.head_id());
            }
        }
        let pinned = SnapshotChain::open(directory.path(), pinned.unwrap()).unwrap();
        assert_eq!(chain.collect_unreachable().unwrap(), 4);
        assert!(SnapshotChain::open(directory.path(), base).is_ok());
        assert_eq!(pinned.read_block(0).unwrap()[0], 3);
        pinned.unpin().unwrap();
        let missing = directory
            .path()
            .join(name(&[99; 32]))
            .with_extension("pvpin");
        File::create(&missing).unwrap();
        assert!(chain.collect_unreachable().is_err());
        assert!(SnapshotChain::open(directory.path(), base).is_ok());
        std::fs::remove_file(missing).unwrap();
        assert_eq!(chain.collect_unreachable().unwrap(), 4);
        assert!(SnapshotChain::open(directory.path(), base).is_err());
        assert_eq!(
            SnapshotChain::open(directory.path(), chain.head_id())
                .unwrap()
                .read_block(0)
                .unwrap()[0],
            8
        );
    }

    #[test]
    fn malformed_payload_missing_parent_and_truncated_footer_fail() {
        let directory = tempfile::tempdir().unwrap();
        let data = (0..layout().logical_bytes())
            .map(|i| (i % 251) as u8)
            .collect::<Vec<_>>();
        let base = capture(directory.path(), None, &[], &data);
        let path = directory.path().join(name(&base.head_id()));
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.write_all_at(b"!", 0).unwrap();
        assert!(base.read_block(0).is_err());
        file.set_len(8).unwrap();
        assert!(SnapshotChain::open(directory.path(), base.head_id()).is_err());
        assert!(SnapshotChain::open(directory.path(), [99; 32]).is_err());
        let mut bad = layout();
        bad.regions[1].guest_address = 0;
        assert!(bad.validate().is_err());
    }
    #[test]
    fn seekable_section_has_standard_table_and_independent_frames() {
        let directory = tempfile::tempdir().unwrap();
        let data = (0..layout().logical_bytes())
            .map(|i| (i % 251) as u8)
            .collect::<Vec<_>>();
        let chain = capture(directory.path(), None, &[], &data);
        let layer = &chain.layers[0];
        assert_eq!(layer.frames.len(), 3);
        for (block, entry) in &chain.resolved {
            let decoded = layer.read(&entry.1).unwrap();
            let start = layout().block_span(*block).unwrap().0 as usize;
            assert_eq!(decoded, data[start..start + decoded.len()]);
        }
        let mut footer = [0; 64];
        let length = layer.file.metadata().unwrap().len();
        layer.file.read_exact_at(&mut footer, length - 64).unwrap();
        let mut seek = [0; 9];
        layer
            .file
            .read_exact_at(&mut seek, u64_at(&footer, 8) - 9)
            .unwrap();
        assert_eq!(u32_at(&seek, 0), 3);
        assert_eq!(u32_at(&seek, 5), SEEK_FOOTER_MAGIC);
    }

    #[test]
    fn incompressible_frames_and_short_regions_stay_within_reader_bounds() {
        let directory = tempfile::tempdir().unwrap();
        let mut random = 0x713a_529du32;
        let data = (0..layout().logical_bytes())
            .map(|_| {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                random as u8
            })
            .collect::<Vec<_>>();
        let chain = capture(directory.path(), None, &[], &data);
        let reopened = SnapshotChain::open(directory.path(), chain.head_id()).unwrap();
        assert_eq!(reopened.layers[0].frames.len(), 3);
        for (_, stored, decoded) in &reopened.layers[0].frames {
            assert!(*stored as usize <= BLOCK_BYTES + 1024);
            assert!(*decoded as usize <= BLOCK_BYTES);
        }
        assert_eq!(reopened.read_block(1).unwrap().len(), 7);
        let mut actual = vec![0; data.len()];
        assert_eq!(reopened.read_at(0, &mut actual).unwrap(), data.len());
        assert_eq!(actual, data);
    }

    #[test]
    fn capture_does_not_replace_a_damaged_existing_content_id() {
        let directory = tempfile::tempdir().unwrap();
        let data = (0..layout().logical_bytes())
            .map(|i| (i % 251) as u8)
            .collect::<Vec<_>>();
        let chain = capture(directory.path(), None, &[], &data);
        let path = directory.path().join(name(&chain.head_id()));
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.write_all_at(b"!", 0).unwrap();
        let result = SnapshotChain::capture(
            directory.path(),
            &layout(),
            None,
            &BTreeSet::new(),
            |block, output| {
                let start = layout().block_span(block)?.0 as usize;
                output.copy_from_slice(&data[start..start + output.len()]);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(chain.read_block(0).is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
