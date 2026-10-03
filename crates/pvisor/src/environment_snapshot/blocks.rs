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
fn id(object: &CompressedObject) -> String {
    object.id().iter().map(|b| format!("{b:02x}")).collect()
}
fn read_object(path: &Path) -> anyhow::Result<CompressedObject> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
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
            let object = CompressedObject::from_bytes(&bytes)?;
            let identity = id(&object);
            let path = store.join("content").join(&identity);
            if !path.exists() {
                let mut temp = tempfile::NamedTempFile::new_in(store.join("content"))?;
                temp.write_all(&object.frame())?;
                temp.as_file().sync_all()?;
                match temp.persist_noclobber(&path) {
                    Ok(_) => {}
                    Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.error.into()),
                }
            }
            // Compare content, not just digest, before granting sharing.
            let stored = read_object(&path)?;
            let mut decoded = vec![0; bytes.len()];
            stored.restore(&mut decoded)?;
            ensure!(
                id(&stored) == identity && decoded == bytes,
                "RAM content collision or corruption"
            );
            let reference = references.join(&identity);
            match fs::hard_link(&path, &reference) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    ensure!(
                        fs::symlink_metadata(&reference)?.ino()
                            == fs::symlink_metadata(&path)?.ino(),
                        "RAM reference mismatch"
                    );
                }
                Err(error) => return Err(error.into()),
            }
            result.blocks.push(BlockRef {
                id: identity,
                length: bytes.len() as u32,
            });
            offset += bytes.len() as u64;
        }
        File::open(store.join("content"))?.sync_all()?;
        File::open(references)?.sync_all()?;
        Ok(result)
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
