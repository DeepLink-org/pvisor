//! Durable RAM content using the resident pool's codec and decoded identity.
//! Hard links are persistent references; GC never needs a separate refcount ledger.
use crate::ram_backing::{BLOCK_BYTES, resident::CompressedObject};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RamBlocks {
    pub length: u64,
    pub blocks: Vec<BlockRef>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockRef {
    pub id: String,
    pub length: u32,
}

/// Validated native dirty-block inventory, bound to the live restore baseline.
#[derive(Clone, Copy)]
pub(crate) struct RamDelta<'a> {
    pub(crate) version: u32,
    pub(crate) length: u64,
    pub(crate) block_bytes: u32,
    pub(crate) base_sha256: &'a str,
    pub(crate) changed_blocks: &'a [u64],
}

/// The same durable frame references used by a live RAM reader. Retaining this
/// owner permits incremental recapture even after the original snapshot is
/// deleted. Children retain their own references and never need an ancestor.
pub(crate) struct PinnedRamBlocks {
    pub(crate) blocks: RamBlocks,
    pub(crate) references: super::store::PendingEnvironment,
    pub(crate) sha256: String,
}
fn id(object: &CompressedObject) -> String {
    object.id().iter().map(|b| format!("{b:02x}")).collect()
}
pub(super) fn read_object(path: &Path) -> anyhow::Result<CompressedObject> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.len() <= (45 + BLOCK_BYTES) as u64,
        "invalid RAM content object"
    );
    let mut bytes = Vec::new();
    file.take((46 + BLOCK_BYTES) as u64)
        .read_to_end(&mut bytes)?;
    Ok(CompressedObject::from_frame(&bytes)?)
}
impl RamBlocks {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.length > 0 && self.blocks.len() as u64 == self.length.div_ceil(BLOCK_BYTES as u64),
            "invalid RAM block inventory"
        );
        for (index, block) in self.blocks.iter().enumerate() {
            super::store::valid_id(&block.id)?;
            let remaining = self.length - index as u64 * BLOCK_BYTES as u64;
            ensure!(
                block.length as u64 == remaining.min(BLOCK_BYTES as u64),
                "RAM block length mismatch"
            );
        }
        Ok(())
    }

    pub(super) fn read_block(&self, references: &Path, index: usize) -> anyhow::Result<Vec<u8>> {
        let block = &self.blocks[index];
        let object =
            read_object(&references.join(&block.id)).context("load persistent RAM block")?;
        ensure!(
            id(&object) == block.id && object.length() == block.length as usize,
            "RAM block identity mismatch"
        );
        let mut bytes = vec![0; block.length as usize];
        object.restore(&mut bytes)?;
        Ok(bytes)
    }
    /// Caller holds the store's shared gate through blob publication and linking.
    pub(super) fn capture(
        store: &Path,
        references: &Path,
        input: &mut File,
    ) -> anyhow::Result<Self> {
        fs::create_dir(references)?;
        let length = input.metadata()?.len();
        ensure!(length > 0, "empty captured RAM");
        let mut result = Self {
            length,
            blocks: Vec::new(),
        };
        let mut offset = 0;
        while offset < length {
            let mut bytes = vec![0; (length - offset).min(BLOCK_BYTES as u64) as usize];
            input.read_exact(&mut bytes)?;
            result
                .blocks
                .push(retain_bytes(store, references, &bytes)?.reference);
            offset += bytes.len() as u64;
        }
        File::open(store.join("content"))?.sync_all()?;
        File::open(references)?.sync_all()?;
        Ok(result)
    }
    pub(super) fn capture_delta(
        store: &Path,
        references: &Path,
        input: &File,
        base: &PinnedRamBlocks,
        delta: &RamDelta<'_>,
    ) -> anyhow::Result<(Self, String)> {
        use std::os::unix::fs::FileExt;
        ensure!(
            delta.version == 1
                && delta.block_bytes as usize == BLOCK_BYTES
                && delta.changed_blocks.len() as u64 <= delta.length.div_ceil(BLOCK_BYTES as u64)
                && delta
                    .changed_blocks
                    .iter()
                    .all(|index| *index < delta.length.div_ceil(BLOCK_BYTES as u64))
                && delta
                    .changed_blocks
                    .windows(2)
                    .all(|pair| pair[0] < pair[1]),
            "invalid incremental RAM inventory"
        );
        base.blocks.validate()?;
        ensure!(
            delta.length == base.blocks.length
                && input.metadata()?.len() == delta.length
                && delta.block_bytes as usize == BLOCK_BYTES
                && delta.base_sha256 == base.sha256,
            "incremental RAM baseline mismatch"
        );
        fs::create_dir(references)?;
        let mut blocks = Vec::with_capacity(base.blocks.blocks.len());
        let mut changed = delta.changed_blocks.iter().copied().peekable();
        let mut hash = Sha256::new();
        let source = base.references.directory().join("ram-blocks");
        for (index, reference) in base.blocks.blocks.iter().enumerate() {
            let mut bytes = vec![0; reference.length as usize];
            input.read_exact_at(&mut bytes, index as u64 * BLOCK_BYTES as u64)?;
            let block = if changed.peek() == Some(&(index as u64)) {
                changed.next();
                retain_bytes(store, references, &bytes)?.reference
            } else {
                ensure!(
                    bytes.iter().all(|byte| *byte == 0),
                    "incremental RAM contains undeclared data"
                );
                let origin = source.join(&reference.id);
                let object = read_object(&origin)?;
                ensure!(
                    id(&object) == reference.id && object.length() == bytes.len(),
                    "incremental RAM inherited frame mismatch"
                );
                object.restore(&mut bytes)?;
                retain_frame(store, references, &object, &bytes, Some(&origin))?
            };
            hash.update(&bytes);
            blocks.push(block);
        }
        ensure!(
            changed.next().is_none(),
            "incremental RAM inventory exceeds its baseline"
        );
        File::open(store.join("content"))?.sync_all()?;
        File::open(references)?.sync_all()?;
        Ok((
            Self {
                length: delta.length,
                blocks,
            },
            hash.finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        ))
    }
    pub(super) fn decode(
        &self,
        references: &Path,
        mut output: impl Write,
        expected_hash: &str,
    ) -> anyhow::Result<()> {
        self.validate()?;
        let mut digest = Sha256::new();
        for index in 0..self.blocks.len() {
            let bytes = self.read_block(references, index)?;
            digest.update(&bytes);
            output.write_all(&bytes)?;
        }
        let actual: String = digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        ensure!(actual == expected_hash, "environment RAM digest mismatch");
        Ok(())
    }
}
pub(super) struct RetainedFrame {
    pub reference: BlockRef,
    /// True only when this call invoked the encoder after a content miss.
    // Native private-file diagnostics are Linux-only; RAM callers use the ref.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub encoded: bool,
}

/// Caller holds the pool's shared gate. Compute the existing decoded identity
/// before encoding; a hit must still decode and match ALL supplied bytes.
/// Frames are immutable, and each destination retains its own reference.
pub(super) fn retain_bytes(
    store: &Path,
    references: &Path,
    decoded: &[u8],
) -> anyhow::Result<RetainedFrame> {
    ensure!(
        !decoded.is_empty() && decoded.len() <= BLOCK_BYTES,
        "invalid content length"
    );
    let identity = crate::util::encode_hex(&crate::ram_backing::resident::identity(decoded));
    let path = store.join("content").join(&identity);
    match read_object(&path) {
        Ok(stored) => {
            ensure!(
                id(&stored) == identity && stored.length() == decoded.len(),
                "reused content frame identity/length mismatch"
            );
            let mut bytes = vec![0; decoded.len()];
            stored.restore(&mut bytes)?;
            ensure!(
                bytes == decoded,
                "reused content frame collision or corruption"
            );
            link_frame(&path, references, &identity)?;
            Ok(RetainedFrame {
                reference: BlockRef {
                    id: identity,
                    length: decoded.len() as u32,
                },
                encoded: false,
            })
        }
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            let object = CompressedObject::from_bytes(decoded)?;
            Ok(RetainedFrame {
                reference: retain_frame(store, references, &object, decoded, None)?,
                encoded: true,
            })
        }
        Err(error) => Err(error),
    }
}

pub(super) fn retain_frame(
    store: &Path,
    references: &Path,
    object: &CompressedObject,
    decoded: &[u8],
    origin: Option<&Path>,
) -> anyhow::Result<BlockRef> {
    let identity = id(object);
    let path = store.join("content").join(&identity);
    let mut copy = origin.is_none();
    if let Some(origin) = origin {
        match fs::hard_link(origin, &path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) if error.raw_os_error() == Some(libc::EXDEV) => copy = true,
            Err(error) => return Err(error.into()),
        }
    }
    if copy && !path.try_exists()? {
        let mut temporary = tempfile::NamedTempFile::new_in(store.join("content"))?;
        temporary.write_all(&object.frame())?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(&path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.error.into()),
        }
    }
    let stored = read_object(&path)?;
    let mut bytes = vec![0; decoded.len()];
    stored.restore(&mut bytes)?;
    ensure!(
        id(&stored) == identity && bytes == decoded,
        "RAM content collision or corruption"
    );
    link_frame(&path, references, &identity)?;
    Ok(BlockRef {
        id: identity,
        length: decoded.len() as u32,
    })
}

fn link_frame(path: &Path, references: &Path, identity: &str) -> anyhow::Result<()> {
    let reference = references.join(identity);
    match fs::hard_link(path, &reference) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            use std::os::unix::fs::MetadataExt;
            let target = fs::symlink_metadata(&reference)?;
            let content = fs::symlink_metadata(path)?;
            ensure!(
                target.is_file() && target.dev() == content.dev() && target.ino() == content.ino(),
                "RAM reference mismatch"
            );
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Exclusive store gate excludes publishers and readers. Live staging links pin content.
pub(super) fn collect(store: &Path) -> anyhow::Result<usize> {
    let mut removed = 0;
    for entry in fs::read_dir(store.join("content"))? {
        let path = entry?.path();
        let meta = fs::symlink_metadata(&path)?;
        ensure!(meta.is_file(), "invalid RAM content entry");
        if meta.nlink() == 1 {
            fs::remove_file(path)?;
            removed += 1;
        }
    }
    File::open(store.join("content"))?.sync_all()?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, symlink};

    #[test]
    fn decoded_content_hits_retain_existing_codec_bytes_without_encoding_again() {
        let temp = tempfile::tempdir().unwrap();
        let store = super::super::SnapshotStore::new(temp.path()).unwrap();
        let pending = store.begin().unwrap();
        let references = pending.directory().join("frames");
        fs::create_dir(&references).unwrap();
        let _gate = super::super::store::gate(temp.path(), false).unwrap();
        let data = vec![37; BLOCK_BYTES];
        let identity = crate::ram_backing::resident::identity(&data);
        let name = crate::util::encode_hex(&identity);
        // Valid existing raw encoding of fill bytes: recompressing would choose
        // a different codec. Hits must preserve the original encoded inode.
        let mut frame = b"PVBLK1\0\0".to_vec();
        frame.extend_from_slice(&identity);
        frame.extend_from_slice(&(data.len() as u32).to_le_bytes());
        frame.push(1);
        frame.extend_from_slice(&data);
        let path = temp.path().join("content").join(&name);
        fs::write(&path, &frame).unwrap();
        let before = fs::metadata(&path).unwrap().ino();
        let hit = retain_bytes(temp.path(), &references, &data).unwrap();
        assert!(!hit.encoded);
        assert_eq!(hit.reference.id, name);
        assert_eq!(fs::read(&path).unwrap(), frame);
        assert_eq!(fs::metadata(references.join(&name)).unwrap().ino(), before);
        // Idempotent references also validate the physical target.
        assert!(
            !retain_bytes(temp.path(), &references, &data)
                .unwrap()
                .encoded
        );
        assert_eq!(fs::metadata(&path).unwrap().nlink(), 2);
        let different = vec![41; BLOCK_BYTES];
        let miss = retain_bytes(temp.path(), &references, &different).unwrap();
        assert!(miss.encoded);
        assert_ne!(miss.reference.id, name);
        assert!(
            !retain_bytes(temp.path(), &references, &different)
                .unwrap()
                .encoded
        );
    }

    #[test]
    fn content_hits_reject_corrupt_frames_symlinks_fifos_and_wrong_references() {
        for fault in [
            "truncated",
            "checksum",
            "header-id",
            "length",
            "symlink",
            "dangling",
            "fifo",
            "reference",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let store = super::super::SnapshotStore::new(temp.path()).unwrap();
            let pending = store.begin().unwrap();
            let references = pending.directory().join("frames");
            fs::create_dir(&references).unwrap();
            let data = vec![53; BLOCK_BYTES];
            let object = CompressedObject::from_bytes(&data).unwrap();
            let name = id(&object);
            let path = temp.path().join("content").join(&name);
            fs::write(&path, object.frame()).unwrap();
            match fault {
                "truncated" => fs::write(&path, b"PVBLK1\0\0").unwrap(),
                "checksum" => {
                    let mut frame = object.frame();
                    *frame.last_mut().unwrap() = 71;
                    fs::write(&path, frame).unwrap();
                }
                "header-id" => {
                    let mut frame = object.frame();
                    frame[8] ^= 1;
                    fs::write(&path, frame).unwrap();
                }
                "length" => {
                    let mut frame = object.frame();
                    frame[40..44].copy_from_slice(&17u32.to_le_bytes());
                    fs::write(&path, frame).unwrap();
                }
                "symlink" | "dangling" => {
                    let origin = temp.path().join("outside");
                    fs::rename(&path, &origin).unwrap();
                    symlink(&origin, &path).unwrap();
                    if fault == "dangling" {
                        fs::remove_file(origin).unwrap();
                    }
                }
                "fifo" => {
                    fs::remove_file(&path).unwrap();
                    let native = super::super::native_path(&path).unwrap();
                    assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
                }
                "reference" => fs::write(references.join(&name), b"unrelated reference").unwrap(),
                _ => unreachable!(),
            }
            let _gate = super::super::store::gate(temp.path(), false).unwrap();
            assert!(
                retain_bytes(temp.path(), &references, &data).is_err(),
                "{fault}"
            );
            if fault != "reference" {
                assert_eq!(fs::read_dir(&references).unwrap().count(), 0);
            }
            if fault == "reference" {
                assert_eq!(
                    fs::read(references.join(&name)).unwrap(),
                    b"unrelated reference"
                );
            }
        }
    }
}
