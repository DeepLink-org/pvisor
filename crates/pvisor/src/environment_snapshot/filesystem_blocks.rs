//! Private files retain encoded content, never another snapshot's data inode.
//! Children own every frame reference; restoration does not resolve ancestors.
use super::{TreeInventory, TreeObject, blocks, store};
use crate::ram_backing::BLOCK_BYTES;
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::unix::{
        ffi::OsStrExt,
        fs::{FileExt, OpenOptionsExt, symlink},
    },
    path::Path,
};

const MAX_BLOCKS: u64 = 1_048_576;

/// Encoding-stream work only; integrity inventory/verification reads are extra.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CaptureStats {
    pub payload_bytes: u64,
    pub encoded_frames: u64,
    pub reused_frames: u64,
    pub zero_frames: u64,
}
impl CaptureStats {
    fn add(&mut self, other: Self) {
        self.payload_bytes += other.payload_bytes;
        self.encoded_frames += other.encoded_frames;
        self.reused_frames += other.reused_frames;
        self.zero_frames += other.zero_frames;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemBlocks {
    pub version: u32,
    pub files: Vec<PrivateFileBlocks>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateFileBlocks {
    pub path: Vec<u8>,
    // A zero frame has no object. Lengths and complete file digests come only
    // from the authenticated tree inventory, including empty files.
    pub blocks: Vec<Option<String>>,
}

impl FilesystemBlocks {
    /// Native-only assembly from authenticated, frozen private roots. No guest
    /// filesystem data inode is created in the capture or pending forest.
    pub(super) fn capture_sources(
        pool: &Path,
        root: &TreeInventory,
        sources: &[super::CapturedFilesystemSource],
        references: &Path,
    ) -> anyhow::Result<(TreeInventory, Self, CaptureStats)> {
        ensure!(
            root.entries.len() == 1
                && root.entries[0].path.is_empty()
                && root.entries[0].object == TreeObject::Directory,
            "direct capture must have an empty owned forest"
        );
        ensure!(
            !sources.is_empty() && sources.len() <= 128,
            "invalid direct private source count"
        );
        fs::create_dir(references)?;
        let mut inventory = root.clone();
        let mut index = Self {
            version: 1,
            files: Vec::new(),
        };
        let mut paths = std::collections::BTreeSet::new();
        let mut roots = std::collections::BTreeSet::new();
        let mut blocks = 0u64;
        let mut stats = CaptureStats::default();
        for source in sources {
            ensure!(
                source.path.components().count() == 1
                    && source
                        .path
                        .components()
                        .all(|part| matches!(part, std::path::Component::Normal(_)))
                    && source.path.as_os_str().as_bytes().len() <= 255
                    && source.path.as_os_str().as_bytes()
                        == source.path.file_name().unwrap().as_bytes()
                    && source.source.is_absolute()
                    && source.source != Path::new("/")
                    && source.source.canonicalize()? == source.source
                    && paths.insert(source.path.clone())
                    && roots.insert(source.source.clone()),
                "ambiguous direct private filesystem source"
            );
            let tree = super::inventory(&source.source)?;
            for entry in &tree.entries {
                if let TreeObject::File {
                    bytes, hardlink, ..
                } = &entry.object
                    && *hardlink == entry.path
                {
                    blocks = blocks
                        .checked_add(bytes.div_ceil(BLOCK_BYTES as u64))
                        .context("direct private frame count overflow")?;
                    ensure!(
                        blocks <= MAX_BLOCKS,
                        "direct private forest exceeds block limit"
                    );
                }
            }
            ensure!(
                inventory
                    .entries
                    .len()
                    .checked_add(tree.entries.len())
                    .is_some_and(|count| count <= 65_536),
                "direct private forest exceeds inventory limit"
            );
            let local = references.join(&source.path);
            let (captured, current) =
                Self::capture_with_stats(pool, &source.source, &tree, &local)?;
            stats.add(current);
            for entry in fs::read_dir(&local)? {
                let entry = entry?;
                match fs::hard_link(entry.path(), references.join(entry.file_name())) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        use std::os::unix::fs::MetadataExt;
                        let old = fs::symlink_metadata(entry.path())?;
                        let new = fs::symlink_metadata(references.join(entry.file_name()))?;
                        ensure!(
                            old.is_file()
                                && new.is_file()
                                && old.dev() == new.dev()
                                && old.ino() == new.ino(),
                            "direct private frame reference collision"
                        );
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            fs::remove_dir_all(&local)?;
            let prefix = |path: &[u8]| {
                if path.is_empty() {
                    source.path.as_os_str().as_bytes().to_vec()
                } else {
                    [source.path.as_os_str().as_bytes(), b"/", path].concat()
                }
            };
            for mut entry in tree.entries {
                entry.path = prefix(&entry.path);
                match &mut entry.object {
                    TreeObject::File { hardlink, .. } | TreeObject::Symlink { hardlink, .. } => {
                        *hardlink = prefix(hardlink)
                    }
                    TreeObject::Directory => {}
                }
                inventory.entries.push(entry);
            }
            for mut file in captured.files {
                file.path = prefix(&file.path);
                index.files.push(file);
            }
        }
        inventory.entries.sort_by(|a, b| {
            a.path
                .split(|byte| *byte == b'/')
                .cmp(b.path.split(|byte| *byte == b'/'))
        });
        index.files.sort_by(|a, b| {
            a.path
                .split(|byte| *byte == b'/')
                .cmp(b.path.split(|byte| *byte == b'/'))
        });
        index.validate(&inventory)?;
        File::open(references)?.sync_all()?;
        Ok((inventory, index, stats))
    }
    pub(super) fn validate(&self, tree: &TreeInventory) -> anyhow::Result<()> {
        super::transfer::validate_tree_metadata(tree)?;
        ensure!(
            self.version == 1 && tree.entries.len() <= 65_536,
            "invalid private filesystem inventory"
        );
        ensure!(
            tree.entries.windows(2).all(|pair| pair[0]
                .path
                .split(|byte| *byte == b'/')
                .cmp(pair[1].path.split(|byte| *byte == b'/'))
                .is_lt()),
            "private filesystem paths must be in canonical traversal order"
        );
        let mut files = self.files.iter();
        let mut count = 0u64;
        for entry in &tree.entries {
            if let TreeObject::File {
                bytes, hardlink, ..
            } = &entry.object
                && *hardlink == entry.path
            {
                let file = files
                    .next()
                    .context("missing private file block inventory")?;
                ensure!(
                    file.path == entry.path
                        && file.blocks.len() as u64 == bytes.div_ceil(BLOCK_BYTES as u64),
                    "private file block geometry mismatch"
                );
                count = count
                    .checked_add(file.blocks.len() as u64)
                    .context("private filesystem block count overflow")?;
                ensure!(
                    count <= MAX_BLOCKS,
                    "private filesystem exceeds block limit"
                );
                for id in file.blocks.iter().flatten() {
                    store::valid_id(id)?;
                }
            }
        }
        ensure!(
            files.next().is_none(),
            "unexpected private file block inventory"
        );
        ensure!(
            serde_json::to_vec(self)?.len() <= 16 * 1024 * 1024,
            "private filesystem block metadata exceeds limit"
        );
        Ok(())
    }

    pub(super) fn capture(
        pool: &Path,
        source: &Path,
        tree: &TreeInventory,
        references: &Path,
    ) -> anyhow::Result<Self> {
        Self::capture_with_stats(pool, source, tree, references).map(|(index, _)| index)
    }

    pub(super) fn capture_with_stats(
        pool: &Path,
        source: &Path,
        tree: &TreeInventory,
        references: &Path,
    ) -> anyhow::Result<(Self, CaptureStats)> {
        super::transfer::validate_tree_metadata(tree)?;
        ensure!(
            tree.entries.len() <= 65_536,
            "private filesystem exceeds inventory limit"
        );
        let count = tree.entries.iter().try_fold(0u64, |total, entry| {
            let blocks = match &entry.object {
                TreeObject::File {
                    bytes, hardlink, ..
                } if *hardlink == entry.path => bytes.div_ceil(BLOCK_BYTES as u64),
                _ => 0,
            };
            total
                .checked_add(blocks)
                .context("private filesystem block count overflow")
        })?;
        ensure!(
            count <= MAX_BLOCKS,
            "private filesystem exceeds block limit"
        );
        let _publishing = store::gate(pool, false)?;
        fs::create_dir(references)?;
        let mut result = Self {
            version: 1,
            files: Vec::new(),
        };
        let mut stats = CaptureStats::default();
        for entry in &tree.entries {
            let TreeObject::File {
                bytes,
                sha256,
                hardlink,
            } = &entry.object
            else {
                continue;
            };
            if *hardlink != entry.path {
                continue;
            }
            let mut input = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(source.join(OsStr::from_bytes(&entry.path)))?;
            let before = input.metadata()?;
            ensure!(
                before.is_file() && before.len() == *bytes,
                "private file changed before sealing"
            );
            let mut hash = Sha256::new();
            let mut file = PrivateFileBlocks {
                path: entry.path.clone(),
                blocks: Vec::new(),
            };
            let mut remaining = *bytes;
            while remaining > 0 {
                let mut data = vec![0; remaining.min(BLOCK_BYTES as u64) as usize];
                input.read_exact(&mut data)?;
                hash.update(&data);
                remaining -= data.len() as u64;
                stats.payload_bytes += data.len() as u64;
                file.blocks.push(if data.iter().all(|byte| *byte == 0) {
                    stats.zero_frames += 1;
                    None
                } else {
                    let retained = blocks::retain_bytes(pool, references, &data)?;
                    if retained.encoded {
                        stats.encoded_frames += 1;
                    } else {
                        stats.reused_frames += 1;
                    }
                    Some(retained.reference.id)
                });
            }
            ensure!(
                crate::util::encode_hex(&hash.finalize()) == *sha256
                    && input.metadata()?.len() == *bytes,
                "private file changed during sealing"
            );
            result.files.push(file);
        }
        result.validate(tree)?;
        File::open(pool.join("content"))?.sync_all()?;
        File::open(references)?.sync_all()?;
        // Unvisited metadata and hard-link topology must also remain exact.
        super::verify_tree(source, tree)?;
        Ok((result, stats))
    }

    pub(super) fn reader(
        &self,
        references: &Path,
        path: &[u8],
        bytes: u64,
    ) -> anyhow::Result<impl Read + '_> {
        let file = self.file(path).context("missing private file payload")?;
        Ok(BlockReader {
            file,
            references: references.to_owned(),
            length: bytes,
            offset: 0,
            buffer: Vec::new(),
            position: 0,
        })
    }
    pub(super) fn file(&self, path: &[u8]) -> Option<&PrivateFileBlocks> {
        let index = self
            .files
            .binary_search_by(|file| {
                file.path
                    .split(|byte| *byte == b'/')
                    .cmp(path.split(|byte| *byte == b'/'))
            })
            .ok()?;
        self.files.get(index)
    }

    pub(super) fn verify(&self, references: &Path, tree: &TreeInventory) -> anyhow::Result<()> {
        self.validate(tree)?;
        ensure!(
            fs::symlink_metadata(references)?.is_dir(),
            "invalid private filesystem references"
        );
        for entry in &tree.entries {
            if let TreeObject::File {
                bytes,
                sha256,
                hardlink,
            } = &entry.object
                && *hardlink == entry.path
            {
                let mut reader = self.reader(references, &entry.path, *bytes)?;
                let mut hash = Sha256::new();
                let mut buffer = vec![0; BLOCK_BYTES];
                loop {
                    let n = reader.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    hash.update(&buffer[..n]);
                }
                ensure!(
                    crate::util::encode_hex(&hash.finalize()) == *sha256,
                    "private filesystem file digest mismatch"
                );
            }
        }
        Ok(())
    }

    pub(super) fn materialize(
        &self,
        references: &Path,
        tree: &TreeInventory,
        prefix: Option<&[u8]>,
        destination: &Path,
    ) -> anyhow::Result<()> {
        self.validate(tree)?;
        let mut selected = Vec::new();
        for entry in &tree.entries {
            let relative = match prefix {
                None => entry.path.as_slice(),
                Some(prefix) if entry.path == prefix => b"",
                Some(prefix) => {
                    let Some(tail) = entry.path.strip_prefix(prefix) else {
                        continue;
                    };
                    let Some(tail) = tail.strip_prefix(b"/") else {
                        continue;
                    };
                    tail
                }
            };
            selected.push((entry, relative));
        }
        ensure!(
            selected
                .first()
                .is_some_and(|(entry, relative)| relative.is_empty()
                    && entry.object == TreeObject::Directory),
            "missing private filesystem root"
        );
        fs::create_dir(destination)?;
        let result = (|| -> anyhow::Result<()> {
            let mut origins = BTreeMap::new();
            for (entry, relative) in &selected {
                let path = destination.join(OsStr::from_bytes(relative));
                match &entry.object {
                    TreeObject::Directory => {
                        if !relative.is_empty() {
                            fs::create_dir(&path)?;
                        }
                    }
                    TreeObject::File { hardlink, .. } | TreeObject::Symlink { hardlink, .. }
                        if *hardlink != entry.path =>
                    {
                        fs::hard_link(
                            origins
                                .get(hardlink)
                                .context("private hardlink escapes materialized root")?,
                            &path,
                        )?;
                    }
                    TreeObject::File { bytes, sha256, .. } => {
                        let output = OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(0o600)
                            .open(&path)?;
                        let mut reader = self.reader(references, &entry.path, *bytes)?;
                        let mut hash = Sha256::new();
                        let mut buffer = vec![0; BLOCK_BYTES];
                        let mut offset = 0;
                        loop {
                            let n = reader.read(&mut buffer)?;
                            if n == 0 {
                                break;
                            }
                            hash.update(&buffer[..n]);
                            if buffer[..n].iter().any(|byte| *byte != 0) {
                                output.write_all_at(&buffer[..n], offset)?;
                            }
                            offset += n as u64;
                        }
                        ensure!(
                            offset == *bytes
                                && crate::util::encode_hex(&hash.finalize()) == *sha256,
                            "private file digest mismatch during restore"
                        );
                        output.set_len(*bytes)?;
                    }
                    TreeObject::Symlink { target, .. } => {
                        symlink(OsStr::from_bytes(target), &path)?
                    }
                }
                origins.insert(entry.path.clone(), path);
            }
            for (entry, relative) in selected.iter().rev() {
                let path = destination.join(OsStr::from_bytes(relative));
                super::transfer::restore_metadata(&path, entry)?;
                if !matches!(entry.object, TreeObject::Symlink { .. }) {
                    File::open(&path)?.sync_all()?;
                }
            }
            Ok(())
        })();
        if result.is_err() {
            super::layers::remove_private_tree(destination)?;
        }
        result
    }
}

struct BlockReader<'a> {
    file: &'a PrivateFileBlocks,
    references: std::path::PathBuf,
    length: u64,
    offset: u64,
    buffer: Vec<u8>,
    position: usize,
}
impl Read for BlockReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.offset == self.length {
            return Ok(0);
        }
        if self.position == self.buffer.len() {
            let index = (self.offset / BLOCK_BYTES as u64) as usize;
            let length = (self.length - self.offset).min(BLOCK_BYTES as u64) as usize;
            self.buffer = vec![0; length];
            if let Some(id) = self
                .file
                .blocks
                .get(index)
                .ok_or_else(|| io::Error::other("missing private block"))?
            {
                let object =
                    blocks::read_object(&self.references.join(id)).map_err(io::Error::other)?;
                if crate::util::encode_hex(object.id().as_ref()) != *id || object.length() != length
                {
                    return Err(io::Error::other(
                        "private filesystem block identity mismatch",
                    ));
                }
                object.restore(&mut self.buffer)?;
            }
            self.position = 0;
        }
        let n = output.len().min(self.buffer.len() - self.position);
        output[..n].copy_from_slice(&self.buffer[self.position..self.position + n]);
        self.position += n;
        self.offset += n as u64;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment_snapshot::{Compatibility, SnapshotStore};
    use std::{io::Write, os::unix::fs::MetadataExt};

    #[test]
    fn private_capture_encodes_only_misses_and_owns_reused_frames_after_gc() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let mut data = (0..3 * BLOCK_BYTES + 129)
            .map(|index| ((index % 251 + 17 * (index / BLOCK_BYTES)) % 256) as u8)
            .collect::<Vec<_>>();
        fs::write(source.join("data"), &data).unwrap();
        fs::hard_link(source.join("data"), source.join("z-alias")).unwrap();
        fs::write(source.join("zero"), vec![0; BLOCK_BYTES + 9]).unwrap();
        fs::write(source.join("empty"), []).unwrap();
        let pool = temp.path().join("pool");
        let store = SnapshotStore::new(&pool).unwrap();
        let capture = |owner: &store::PendingEnvironment| {
            let tree = super::super::inventory(&source).unwrap();
            let references = owner.directory().join("filesystem-blocks");
            let (index, stats) =
                FilesystemBlocks::capture_with_stats(&pool, &source, &tree, &references).unwrap();
            (tree, index, stats, references)
        };
        let first_owner = store.begin().unwrap();
        let (first_tree, first, stats, _) = capture(&first_owner);
        assert_eq!(
            stats,
            CaptureStats {
                payload_bytes: (4 * BLOCK_BYTES + 138) as u64,
                encoded_frames: 4,
                reused_frames: 0,
                zero_frames: 2,
            }
        );
        let first_inodes = first
            .files
            .iter()
            .flat_map(|file| file.blocks.iter().flatten())
            .map(|id| {
                (
                    id.clone(),
                    fs::metadata(pool.join("content").join(id)).unwrap().ino(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let second_owner = store.begin().unwrap();
        let (second_tree, second, second_stats, _) = capture(&second_owner);
        assert_eq!(first_tree, second_tree);
        assert_eq!(first, second);
        assert_eq!(
            second_stats,
            CaptureStats {
                encoded_frames: 0,
                reused_frames: 4,
                ..stats
            }
        );
        data[BLOCK_BYTES + 23] ^= 0x80;
        fs::write(source.join("data"), &data).unwrap();
        let third_owner = store.begin().unwrap();
        let (third_tree, third, third_stats, third_refs) = capture(&third_owner);
        assert_eq!(
            third_stats,
            CaptureStats {
                encoded_frames: 1,
                reused_frames: 3,
                ..stats
            }
        );
        drop(first_owner);
        drop(second_owner);
        store.collect_abandoned().unwrap();
        third.verify(&third_refs, &third_tree).unwrap();
        for id in third
            .files
            .iter()
            .flat_map(|file| file.blocks.iter().flatten())
        {
            if let Some(inode) = first_inodes.get(id) {
                assert_eq!(
                    fs::metadata(pool.join("content").join(id)).unwrap().ino(),
                    *inode
                );
            }
        }
        assert_eq!(fs::read(source.join("z-alias")).unwrap(), data);
        let old_pending = store.begin().unwrap();
        let old_refs = old_pending.directory().join("filesystem-blocks");
        assert!(
            FilesystemBlocks::capture_with_stats(&pool, &source, &first_tree, &old_refs).is_err(),
            "reused frames must not bypass complete source content validation"
        );
        drop(old_pending);
        drop(third_owner);
        store.collect_abandoned().unwrap();
        assert_eq!(fs::read_dir(pool.join("content")).unwrap().count(), 0);
    }

    #[test]
    fn live_private_frame_owner_survives_parent_store_removal_and_preserves_pool_inodes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("data"), vec![17; 131_073]).unwrap();
        let pool = root.join("pool");
        let parent = SnapshotStore::with_filesystem_pool(&root.join("parent"), &pool).unwrap();
        let compatibility = Compatibility {
            host_boot: "boot".into(),
            build: "build".into(),
            firmware: "kernel".into(),
            profile: "profile".into(),
        };
        let publish = |store: &SnapshotStore| {
            let pending = store.begin().unwrap();
            pending
                .create_ram()
                .unwrap()
                .write_all(&[29; 65536])
                .unwrap();
            pending
                .publish_chunked_filesystem(&source, b"machine", compatibility.clone(), true)
                .unwrap()
        };
        let parent_id = publish(&parent);
        let snapshot = parent.open(&parent_id, &compatibility).unwrap();
        let blocks = snapshot.manifest().filesystem_blocks.clone().unwrap();
        let live = SnapshotStore::with_filesystem_pool(&root.join("live"), &pool).unwrap();
        let owner = snapshot.pin_private_files(&live).unwrap().unwrap();
        let inodes = blocks.files[0]
            .blocks
            .iter()
            .map(|id| {
                fs::metadata(pool.join("content").join(id.as_ref().unwrap()))
                    .unwrap()
                    .ino()
            })
            .collect::<Vec<_>>();
        drop(snapshot);
        parent.delete(&parent_id).unwrap();
        drop(parent);
        fs::remove_dir_all(root.join("parent")).unwrap();
        SnapshotStore::new(&pool)
            .unwrap()
            .collect_abandoned()
            .unwrap();
        // An active staging owner also prevents the live store's collector
        // from removing its independent persistent reference directory.
        live.collect_abandoned().unwrap();
        let child_id = publish(&live);
        let child = live.open(&child_id, &compatibility).unwrap();
        assert_eq!(
            child.manifest().filesystem_blocks.as_ref().unwrap(),
            &blocks
        );
        for (index, id) in blocks.files[0].blocks.iter().enumerate() {
            assert_eq!(
                fs::metadata(pool.join("content").join(id.as_ref().unwrap()))
                    .unwrap()
                    .ino(),
                inodes[index]
            );
        }
        drop(child);
        live.delete(&child_id).unwrap();
        drop(owner);
        live.collect_abandoned().unwrap();
        assert_eq!(fs::read_dir(pool.join("content")).unwrap().count(), 0);
    }
}
