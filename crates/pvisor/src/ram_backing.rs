//! Immutable base/delta RAM storage plus a bounded writable staging adapter.
//! The adapter preserves live mmap semantics; generation commit is not a VM checkpoint.
pub mod image;
pub use image::{ImageId, RamLayout, RamRegion, SnapshotChain};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, ErrorKind};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

pub const MAGIC: &[u8; 8] = b"PVZRAM\0\0";
pub const BLOCK_BYTES: usize = 64 * 1024;
const PAGE_BYTES: usize = 4096;
const HEAD_MAGIC: &[u8; 8] = b"PVHEAD2\0";
const HEAD_BYTES: usize = 80;

pub(super) fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}
pub(super) fn hash(data: &[u8]) -> ImageId {
    Sha256::digest(data).into()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    version: u32,
    directory: PathBuf,
}

/// One serialized writer. Dirty blocks are staged on disk, not in a full-RAM
/// userspace cache. `open` inspects a committed head; exclude concurrent writes.
pub struct CompressedRam {
    manifest: File,
    directory: PathBuf,
    staging: Option<tempfile::NamedTempFile>,
    dirty: BTreeMap<u64, u16>,
    chain: Option<SnapshotChain>,
    logical_bytes: u64,
    failed: bool,
}

impl CompressedRam {
    /// The manifest and private sidecar directory must be exclusively owned.
    /// Generations are immutable files; the manifest only appends head records.
    pub fn create(manifest: File, directory: &Path) -> io::Result<Self> {
        let metadata = manifest.metadata()?;
        if !metadata.is_file() || metadata.len() != 0 {
            return Err(invalid("RAM manifest must be a new empty regular file"));
        }
        let directory = directory.canonicalize()?;
        let descriptor = serde_json::to_vec(&Descriptor {
            version: 2,
            directory: directory.clone(),
        })
        .map_err(io::Error::other)?;
        if descriptor.len() > 8192 {
            return Err(invalid("RAM sidecar path too long"));
        }
        let mut header = MAGIC.to_vec();
        header.extend_from_slice(&(descriptor.len() as u32).to_le_bytes());
        header.extend_from_slice(&descriptor);
        header.extend_from_slice(&hash(&descriptor));
        manifest.write_all_at(&header, 0)?;
        manifest.sync_all()?;
        let staging = tempfile::NamedTempFile::new_in(&directory)?;
        Ok(Self {
            manifest,
            directory,
            staging: Some(staging),
            dirty: BTreeMap::new(),
            chain: None,
            logical_bytes: 0,
            failed: false,
        })
    }

    pub fn open(manifest: File) -> io::Result<Self> {
        let length = manifest.metadata()?.len();
        let mut header = [0; 12];
        manifest.read_exact_at(&mut header, 0)?;
        let descriptor_bytes = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
        if &header[..8] != MAGIC || descriptor_bytes > 8192 {
            return Err(invalid("invalid RAM manifest header"));
        }
        let mut bytes = vec![0; descriptor_bytes];
        manifest.read_exact_at(&mut bytes, 12)?;
        let mut checksum = [0; 32];
        manifest.read_exact_at(&mut checksum, 12 + descriptor_bytes as u64)?;
        if hash(&bytes) != checksum {
            return Err(invalid("RAM descriptor checksum mismatch"));
        }
        let descriptor: Descriptor =
            serde_json::from_slice(&bytes).map_err(|_| invalid("invalid RAM descriptor"))?;
        if descriptor.version != 2 || !descriptor.directory.is_absolute() {
            return Err(invalid("unsupported RAM manifest"));
        }
        let mut offset = 44 + descriptor_bytes as u64;
        if length < offset || (length - offset) % HEAD_BYTES as u64 != 0 {
            return Err(invalid("incomplete RAM head record"));
        }
        let mut head = None;
        let mut logical_bytes = 0;
        // Bound startup work even if an untrusted manifest contains many records.
        if (length - offset) / HEAD_BYTES as u64 > 1_000_000 {
            return Err(invalid("RAM head log exceeds budget"));
        }
        while offset < length {
            let mut record = [0; HEAD_BYTES];
            manifest.read_exact_at(&mut record, offset)?;
            let size = u64::from_le_bytes(record[8..16].try_into().unwrap());
            if &record[..8] != HEAD_MAGIC
                || hash(&record[..48]).as_slice() != &record[48..]
                || (head.is_some() && logical_bytes != size)
            {
                return Err(invalid("invalid RAM head record"));
            }
            logical_bytes = size;
            head = Some(record[16..48].try_into().unwrap());
            offset += HEAD_BYTES as u64;
        }
        let chain = head
            .map(|id| SnapshotChain::open(&descriptor.directory, id))
            .transpose()?;
        if chain
            .as_ref()
            .is_some_and(|chain| chain.layout().logical_bytes() != logical_bytes)
        {
            return Err(invalid("RAM head layout mismatch"));
        }
        Ok(Self {
            manifest,
            directory: descriptor.directory,
            staging: None,
            dirty: BTreeMap::new(),
            chain,
            logical_bytes,
            failed: false,
        })
    }

    pub fn logical_bytes(&self) -> u64 {
        self.logical_bytes
    }
    pub fn head_id(&self) -> Option<ImageId> {
        self.chain.as_ref().map(SnapshotChain::head_id)
    }
    pub fn chain_depth(&self) -> usize {
        self.chain.as_ref().map_or(0, SnapshotChain::depth)
    }
    pub fn changed_blocks(&self) -> usize {
        self.chain.as_ref().map_or(0, SnapshotChain::changed_blocks)
    }
    /// All files in the bundle, including retained generations and writer staging.
    pub fn allocated_bytes(&self) -> io::Result<u64> {
        use std::os::unix::fs::MetadataExt;
        let mut total = (self.manifest.metadata()?.blocks()
            + std::fs::metadata(&self.directory)?.blocks())
            * 512;
        for entry in std::fs::read_dir(&self.directory)? {
            let metadata = entry?.metadata()?;
            if metadata.is_file() {
                total += metadata.blocks() * 512;
            }
        }
        Ok(total)
    }

    pub fn set_len(&mut self, size: u64) -> io::Result<()> {
        self.writable()?;
        if size == self.logical_bytes {
            return Ok(());
        }
        if self.logical_bytes != 0 {
            return Err(invalid("cannot resize initialized RAM"));
        }
        let layout = RamLayout {
            page_bytes: 4096,
            regions: vec![RamRegion {
                guest_address: 0,
                length: size,
            }],
        };
        layout.validate()?;
        self.logical_bytes = size;
        Ok(())
    }

    fn read_block(&self, block: u64) -> io::Result<Vec<u8>> {
        if self.failed {
            return Err(invalid("RAM storage failed"));
        }
        let offset = block
            .checked_mul(BLOCK_BYTES as u64)
            .ok_or_else(|| invalid("RAM offset overflow"))?;
        if offset >= self.logical_bytes {
            return Err(invalid("block outside RAM"));
        }
        let length = (self.logical_bytes - offset).min(BLOCK_BYTES as u64) as usize;
        let full_mask = ((1u32 << length.div_ceil(PAGE_BYTES)) - 1) as u16;
        let fully_staged = self.dirty.get(&block) == Some(&full_mask);
        let mut output = if fully_staged || self.chain.is_none() {
            vec![0; length]
        } else {
            self.chain.as_ref().unwrap().read_block(block)?
        };
        if let Some(mask) = self.dirty.get(&block) {
            let staging = self
                .staging
                .as_ref()
                .ok_or_else(|| invalid("missing RAM staging"))?;
            for page in 0..length.div_ceil(PAGE_BYTES) {
                if mask & (1 << page) != 0 {
                    let start = page * PAGE_BYTES;
                    let end = (start + PAGE_BYTES).min(length);
                    staging
                        .as_file()
                        .read_exact_at(&mut output[start..end], offset + start as u64)?;
                }
            }
        }
        Ok(output)
    }

    pub fn read_at(&self, offset: u64, output: &mut [u8]) -> io::Result<usize> {
        if self.failed {
            return Err(invalid("RAM storage failed"));
        }
        let size = self
            .logical_bytes
            .saturating_sub(offset)
            .min(output.len() as u64) as usize;
        let mut done = 0;
        while done < size {
            let position = offset + done as u64;
            let data = self.read_block(position / BLOCK_BYTES as u64)?;
            let within = (position % BLOCK_BYTES as u64) as usize;
            let length = (size - done).min(data.len() - within);
            output[done..done + length].copy_from_slice(&data[within..within + length]);
            done += length;
        }
        Ok(size)
    }

    pub fn write_at(&mut self, offset: u64, input: &[u8]) -> io::Result<()> {
        self.writable()?;
        if offset
            .checked_add(input.len() as u64)
            .is_none_or(|end| end > self.logical_bytes)
        {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "RAM write outside logical size",
            ));
        }
        let result = (|| {
            let mut done = 0;
            while done < input.len() {
                let position = offset + done as u64;
                let block = position / BLOCK_BYTES as u64;
                let page_offset = position / PAGE_BYTES as u64 * PAGE_BYTES as u64;
                let within_page = (position - page_offset) as usize;
                let page = (page_offset % BLOCK_BYTES as u64) as usize / PAGE_BYTES;
                let page_length =
                    (self.logical_bytes - page_offset).min(PAGE_BYTES as u64) as usize;
                let length = (input.len() - done).min(page_length - within_page);
                let bit = 1u16 << page;
                let initialized = self.dirty.get(&block).is_some_and(|mask| mask & bit != 0);
                if !initialized && (within_page != 0 || length != page_length) {
                    let data = self.read_block(block)?;
                    let start = page * PAGE_BYTES;
                    self.staging
                        .as_ref()
                        .unwrap()
                        .as_file()
                        .write_all_at(&data[start..start + page_length], page_offset)?;
                }
                self.staging
                    .as_ref()
                    .unwrap()
                    .as_file()
                    .write_all_at(&input[done..done + length], position)?;
                *self.dirty.entry(block).or_default() |= bit;
                done += length;
            }
            Ok(())
        })();
        self.failed = result.is_err();
        result
    }

    /// Flush writable staging without producing a generation. The offload
    /// coordinator commits only after CPU/device accesses and mmap writes drain.
    pub fn flush_writes(&self) -> io::Result<()> {
        if self.failed {
            return Err(invalid("RAM storage failed"));
        }
        if let Some(staging) = &self.staging {
            staging.as_file().sync_all()?;
        }
        self.manifest.sync_all()
    }

    /// Commit all adapter writes as one immutable generation. Kernel mmap dirty
    /// pages must be written back by the caller first. No VM-wide epoch is implied.
    pub fn sync_all(&mut self) -> io::Result<()> {
        if self.staging.is_none() {
            return self.manifest.sync_all();
        }
        self.writable()?;
        if self.logical_bytes == 0 || (self.chain.is_some() && self.dirty.is_empty()) {
            return self.manifest.sync_all();
        }
        let result = (|| {
            // The adapter addresses compact backing offsets. It does not know
            // guest physical regions; direct SnapshotChain callers supply those.
            let layout = RamLayout {
                page_bytes: 4096,
                regions: vec![RamRegion {
                    guest_address: 0,
                    length: self.logical_bytes,
                }],
            };
            let chain = SnapshotChain::capture(
                &self.directory,
                &layout,
                self.chain.as_ref(),
                &self.dirty.keys().copied().collect::<BTreeSet<_>>(),
                |block, output| {
                    output.copy_from_slice(&self.read_block(block)?);
                    Ok(())
                },
            )?;
            if self.head_id() != Some(chain.head_id()) {
                let mut record = [0; HEAD_BYTES];
                record[..8].copy_from_slice(HEAD_MAGIC);
                record[8..16].copy_from_slice(&self.logical_bytes.to_le_bytes());
                record[16..48].copy_from_slice(&chain.head_id());
                let checksum = hash(&record[..48]);
                record[48..].copy_from_slice(&checksum);
                self.manifest
                    .write_all_at(&record, self.manifest.metadata()?.len())?;
                self.manifest.sync_all()?;
            }
            self.chain = Some(chain);
            self.dirty.clear();
            self.staging.as_ref().unwrap().as_file().set_len(0)?;
            // Publication is already durable. Failed housekeeping must not turn
            // a valid committed generation into a reported storage failure.
            if let Err(error) = self.chain.as_ref().unwrap().collect_unreachable() {
                tracing::warn!(%error, "RAM generation collection deferred");
            }
            Ok(())
        })();
        self.failed = result.is_err();
        result
    }

    fn writable(&self) -> io::Result<()> {
        if self.staging.is_none() {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "read-only RAM inspector",
            ));
        }
        if self.failed {
            return Err(invalid("RAM storage failed"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn page_staging_preserves_neighbors_and_flush_does_not_publish() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = tempfile::tempfile().unwrap();
        let mut store = CompressedRam::create(manifest, directory.path()).unwrap();
        store.set_len(BLOCK_BYTES as u64).unwrap();
        store.write_at(4096, &[7; 4096]).unwrap();
        assert_eq!(store.dirty[&0], 2);
        assert_eq!(
            store
                .staging
                .as_ref()
                .unwrap()
                .as_file()
                .metadata()
                .unwrap()
                .len(),
            8192
        );
        store.write_at(4100, &[9; 3]).unwrap();
        store.flush_writes().unwrap();
        assert!(store.head_id().is_none());
        let mut expected = vec![0; BLOCK_BYTES];
        expected[4096..8192].fill(7);
        expected[4100..4103].fill(9);
        assert_eq!(store.read_block(0).unwrap(), expected);
        store.sync_all().unwrap();
        assert_eq!(store.read_block(0).unwrap(), expected);
        store.write_at(8193, &[11; 2]).unwrap();
        expected[8193..8195].fill(11);
        assert_eq!(store.dirty[&0], 4);
        assert_eq!(store.read_block(0).unwrap(), expected);
        store.sync_all().unwrap();
        assert_eq!(store.read_block(0).unwrap(), expected);
    }

    #[test]
    fn committed_adapter_collects_superseded_chains() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = tempfile::tempfile().unwrap();
        let mut store =
            CompressedRam::create(manifest.try_clone().unwrap(), directory.path()).unwrap();
        store.set_len(BLOCK_BYTES as u64).unwrap();
        store.sync_all().unwrap();
        for value in 1..=8 {
            store.write_at(0, &[value]).unwrap();
            store.sync_all().unwrap();
        }
        assert_eq!(store.chain_depth(), 1);
        let count = std::fs::read_dir(directory.path())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "pvdelta")
            })
            .count();
        assert_eq!(count, 1);
        let reader = CompressedRam::open(manifest).unwrap();
        let mut byte = [0];
        reader.read_at(0, &mut byte).unwrap();
        assert_eq!(byte, [8]);
    }

    #[test]
    fn staged_partial_writes_commit_deltas_and_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = tempfile::tempfile().unwrap();
        let mut store =
            CompressedRam::create(manifest.try_clone().unwrap(), directory.path()).unwrap();
        store.set_len((BLOCK_BYTES * 2 + 7) as u64).unwrap();
        store
            .write_at((BLOCK_BYTES - 2) as u64, b"cross-boundary")
            .unwrap();
        store.sync_all().unwrap();
        let base = store.head_id().unwrap();
        store
            .write_at((BLOCK_BYTES * 2) as u64, b"partial")
            .unwrap();
        store.sync_all().unwrap();
        assert_ne!(store.head_id().unwrap(), base);
        assert_eq!(store.chain_depth(), 2);
        assert_eq!(store.changed_blocks(), 1);
        let head = store.head_id();
        store.sync_all().unwrap();
        assert_eq!(store.head_id(), head);
        let reader = CompressedRam::open(manifest).unwrap();
        let mut output = [0; 14];
        reader
            .read_at((BLOCK_BYTES - 2) as u64, &mut output)
            .unwrap();
        assert_eq!(&output, b"cross-boundary");
        let mut tail = [0; 8];
        assert_eq!(
            reader.read_at((BLOCK_BYTES * 2) as u64, &mut tail).unwrap(),
            7
        );
        assert_eq!(&tail[..7], b"partial");
        assert_eq!(reader.read_at(u64::MAX, &mut tail).unwrap(), 0);
    }
    #[test]
    fn invalid_bounds_read_only_and_torn_manifest_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = tempfile::tempfile().unwrap();
        let mut store =
            CompressedRam::create(manifest.try_clone().unwrap(), directory.path()).unwrap();
        store.set_len(BLOCK_BYTES as u64).unwrap();
        assert!(store.write_at(u64::MAX, b"x").is_err());
        assert!(store.set_len(1).is_err());
        store.sync_all().unwrap();
        let mut reader = CompressedRam::open(manifest.try_clone().unwrap()).unwrap();
        assert!(reader.write_at(0, b"x").is_err());
        manifest
            .set_len(manifest.metadata().unwrap().len() - 1)
            .unwrap();
        assert!(CompressedRam::open(manifest).is_err());
    }

    #[test]
    fn failed_head_publication_retains_dirty_blocks_and_old_head() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = tempfile::NamedTempFile::new().unwrap();
        let mut store =
            CompressedRam::create(manifest.reopen().unwrap(), directory.path()).unwrap();
        store.set_len(BLOCK_BYTES as u64).unwrap();
        store.sync_all().unwrap();
        let original = store.head_id();
        store.write_at(0, b"changed").unwrap();
        store.manifest = File::open(manifest.path()).unwrap(); // force append failure
        assert!(store.sync_all().is_err());
        assert_eq!(store.head_id(), original);
        assert!(store.dirty.contains_key(&0));
        assert!(store.failed);
        let reader = CompressedRam::open(File::open(manifest.path()).unwrap()).unwrap();
        let mut output = [1; 7];
        reader.read_at(0, &mut output).unwrap();
        assert_eq!(output, [0; 7]);
    }

    #[test]
    fn mixed_page_and_block_writes_survive_repeated_compaction() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = tempfile::tempfile().unwrap();
        let mut store =
            CompressedRam::create(manifest.try_clone().unwrap(), directory.path()).unwrap();
        let mut expected = vec![0; BLOCK_BYTES * 2 + 7];
        store.set_len(expected.len() as u64).unwrap();
        let mut random = 0x713a_529du32;
        for round in 0..24usize {
            let (offset, length) = match round % 4 {
                0 => (0, BLOCK_BYTES),
                1 => (BLOCK_BYTES + PAGE_BYTES, PAGE_BYTES),
                2 => (BLOCK_BYTES - 3, PAGE_BYTES + 7),
                _ => (BLOCK_BYTES * 2 - 2, 9),
            };
            let input = (0..length)
                .map(|_| {
                    random ^= random << 13;
                    random ^= random >> 17;
                    random ^= random << 5;
                    if round % 8 == 0 { 0 } else { random as u8 }
                })
                .collect::<Vec<_>>();
            store.write_at(offset as u64, &input).unwrap();
            expected[offset..offset + length].copy_from_slice(&input);
            store.sync_all().unwrap();
            assert!(store.chain_depth() <= 8);
            let reopened = CompressedRam::open(manifest.try_clone().unwrap()).unwrap();
            let mut actual = vec![0; expected.len()];
            assert_eq!(reopened.read_at(0, &mut actual).unwrap(), expected.len());
            assert_eq!(actual, expected, "round {round}");
        }
    }

    #[test]
    fn complete_block_overwrite_recovers_corruption_without_reading_old_payload() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = tempfile::tempfile().unwrap();
        let mut store =
            CompressedRam::create(manifest.try_clone().unwrap(), directory.path()).unwrap();
        store.set_len(BLOCK_BYTES as u64).unwrap();
        let input = (0..BLOCK_BYTES).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        store.write_at(0, &input).unwrap();
        store.sync_all().unwrap();
        let hex = store.head_id().unwrap().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let payload = std::fs::OpenOptions::new()
            .write(true)
            .open(directory.path().join(format!("{hex}.pvdelta")))
            .unwrap();
        payload.write_all_at(b"!", 0).unwrap();
        assert!(store.read_block(0).is_err());
        // Every page is replaced; none of the damaged ancestor is required.
        store.write_at(0, &vec![42; BLOCK_BYTES]).unwrap();
        assert_eq!(store.read_block(0).unwrap(), vec![42; BLOCK_BYTES]);
        store.sync_all().unwrap();
        let reopened = CompressedRam::open(manifest).unwrap();
        assert_eq!(reopened.read_block(0).unwrap(), vec![42; BLOCK_BYTES]);
    }

    #[test]
    fn partial_write_requiring_corrupt_ancestor_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = tempfile::tempfile().unwrap();
        let mut store = CompressedRam::create(manifest, directory.path()).unwrap();
        store.set_len(BLOCK_BYTES as u64).unwrap();
        let input = (0..BLOCK_BYTES).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        store.write_at(0, &input).unwrap();
        store.sync_all().unwrap();
        let old_head = store.head_id();
        let hex = old_head.unwrap().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        std::fs::OpenOptions::new()
            .write(true)
            .open(directory.path().join(format!("{hex}.pvdelta")))
            .unwrap()
            .write_all_at(b"!", 0)
            .unwrap();
        assert!(store.write_at(17, b"replacement").is_err());
        assert_eq!(store.head_id(), old_head);
        assert!(store.failed);
        assert!(store.sync_all().is_err());
        assert!(store.write_at(0, &vec![42; BLOCK_BYTES]).is_err());
    }
    proptest::proptest! {
        #[test]
        fn random_partial_writes_and_multiple_commits_match_plain_ram(
            operations in proptest::collection::vec((0usize..BLOCK_BYTES * 2, proptest::collection::vec(proptest::prelude::any::<u8>(), 0..96)), 1..24)
        ) {
            let directory = tempfile::tempdir().unwrap(); let manifest = tempfile::tempfile().unwrap();
            let mut store = CompressedRam::create(manifest.try_clone().unwrap(), directory.path()).unwrap();
            let mut expected = vec![0; BLOCK_BYTES * 2]; store.set_len(expected.len() as u64).unwrap();
            for (i, (offset, data)) in operations.into_iter().enumerate() {
                let count = data.len().min(expected.len() - offset);
                expected[offset..offset + count].copy_from_slice(&data[..count]);
                store.write_at(offset as u64, &data[..count]).unwrap();
                if i % 3 == 0 { store.sync_all().unwrap(); }
            }
            store.sync_all().unwrap(); let reader = CompressedRam::open(manifest).unwrap();
            let mut actual = vec![0; expected.len()]; reader.read_at(0, &mut actual).unwrap();
            proptest::prop_assert_eq!(actual, expected);
        }
    }
}
