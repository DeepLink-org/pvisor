//! Durable immutable artifact objects. Bulk I/O stays outside the scheduler lock.
use crate::{ARTIFACT_CHUNK_BYTES, ArtifactManifest, BlobRef, LeaseKey};
use anyhow::{Context, ensure};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);
pub fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
}
pub(crate) struct VerifiedArtifacts {
    reference: BlobRef,
    key: LeaseKey,
    run: BundleRunIdentity,
}
#[derive(serde::Deserialize)]
struct BundleRunIdentity {
    run_id: pvisor_core::RunId,
    attempt_id: pvisor_core::AttemptId,
    state: pvisor_core::RunState,
    started_at_unix_ms: u64,
    finished_at_unix_ms: u64,
    exit_code: Option<i32>,
}
impl VerifiedArtifacts {
    pub(crate) fn matches(
        &self,
        reference: &BlobRef,
        key: &LeaseKey,
        result: Option<&pvisor_core::RunResult>,
    ) -> bool {
        self.reference == *reference
            && self.key == *key
            && result.is_some_and(|r| {
                self.run.run_id == r.run_id
                    && self.run.attempt_id == r.attempt_id
                    && self.run.state == r.state
                    && self.run.started_at_unix_ms == r.started_at_unix_ms
                    && self.run.finished_at_unix_ms == r.finished_at_unix_ms
                    && self.run.exit_code == r.exit_code
            })
    }
}
impl ArtifactStore {
    pub fn open(root: &Path) -> anyhow::Result<Self> {
        fs::create_dir_all(root)?;
        ensure!(
            fs::symlink_metadata(root)?.is_dir(),
            "artifact store must be a directory, not a symlink"
        );
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        File::open(root)?.sync_all()?;
        File::open(
            root.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?
        .sync_all()?;
        Ok(Self {
            root: root.to_owned(),
        })
    }
    fn path(&self, reference: &BlobRef) -> anyhow::Result<PathBuf> {
        reference.validate()?;
        Ok(self
            .root
            .join(&reference.digest[..2])
            .join(&reference.digest))
    }
    pub fn put(&self, bytes: &[u8]) -> anyhow::Result<BlobRef> {
        ensure!(
            bytes.len() <= ARTIFACT_CHUNK_BYTES,
            "artifact object exceeds chunk limit"
        );
        let reference = BlobRef {
            digest: blake3::hash(bytes).to_hex().to_string(),
            bytes: bytes.len() as u64,
        };
        let path = self.path(&reference)?;
        if path.try_exists()? {
            self.get(&reference)?;
            return Ok(reference);
        }
        let directory = path.parent().unwrap();
        fs::create_dir_all(directory)?;
        ensure!(
            fs::symlink_metadata(directory)?.is_dir(),
            "artifact shard is not a directory"
        );
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        File::open(&self.root)?.sync_all()?;
        let temporary = directory.join(format!(
            ".upload-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut created = false;
        let published = (|| -> anyhow::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o400)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temporary)?;
            created = true;
            file.write_all(bytes)?;
            file.sync_all()?;
            match fs::hard_link(&temporary, &path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    self.get(&reference)?;
                }
                Err(error) => return Err(error.into()),
            }
            File::open(directory)?.sync_all()?;
            Ok(())
        })();
        let removed = if created {
            fs::remove_file(&temporary)
        } else {
            Ok(())
        };
        published?;
        removed?;
        File::open(directory)?.sync_all()?;
        Ok(reference)
    }
    pub fn get(&self, reference: &BlobRef) -> anyhow::Result<Vec<u8>> {
        let path = self.path(reference)?;
        ensure!(
            fs::symlink_metadata(path.parent().unwrap())?.is_dir(),
            "artifact shard is not a directory"
        );
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        ensure!(
            file.metadata()?.is_file() && file.metadata()?.len() == reference.bytes,
            "artifact size/type mismatch"
        );
        let mut bytes = Vec::with_capacity(reference.bytes as usize);
        file.take(ARTIFACT_CHUNK_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 == reference.bytes
                && blake3::hash(&bytes).to_hex().as_str() == reference.digest,
            "artifact integrity check failed"
        );
        Ok(bytes)
    }
    pub fn read_manifest(&self, reference: &BlobRef) -> anyhow::Result<ArtifactManifest> {
        let manifest: ArtifactManifest = serde_json::from_slice(&self.get(reference)?)
            .context("invalid artifact manifest JSON")?;
        manifest.validate()?;
        Ok(manifest)
    }
    pub(crate) fn verify(
        &self,
        reference: &BlobRef,
        key: &LeaseKey,
    ) -> anyhow::Result<VerifiedArtifacts> {
        let manifest = self.read_manifest(reference)?;
        ensure!(
            manifest.key == *key,
            "artifacts belong to a different lease"
        );
        ensure!(
            manifest.files.iter().any(|f| f.name == "run-bundle.json"),
            "missing native Run Bundle artifact"
        );
        let mut bundle = Vec::new();
        for file in &manifest.files {
            let mut hash = blake3::Hasher::new();
            for chunk in &file.chunks {
                let bytes = self.get(chunk)?;
                hash.update(&bytes);
                if file.name == "run-bundle.json" {
                    bundle.extend_from_slice(&bytes);
                }
            }
            ensure!(
                hash.finalize().to_hex().as_str() == file.digest,
                "whole artifact file integrity check failed"
            );
        }
        // Validate attempt identity independently of the worker. The worker
        // validates the complete native schema; this crate has no dependency
        // on the executor or its evolving Bundle schema.
        #[derive(serde::Deserialize)]
        struct BundleIdentity {
            schema_version: u32,
            run: BundleRunIdentity,
        }
        let identity: BundleIdentity = serde_json::from_slice(&bundle)?;
        ensure!(identity.schema_version > 0, "invalid Bundle schema version");
        Ok(VerifiedArtifacts {
            reference: reference.clone(),
            key: key.clone(),
            run: identity.run,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publication_is_idempotent_durable_and_checks_corruption_and_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(&temp.path().join("objects")).unwrap();
        let reference = store.put(b"evidence").unwrap();
        assert_eq!(store.put(b"evidence").unwrap(), reference);
        let reopened = ArtifactStore::open(&temp.path().join("objects")).unwrap();
        assert_eq!(reopened.get(&reference).unwrap(), b"evidence");
        assert!(store.put(&vec![0; ARTIFACT_CHUNK_BYTES + 1]).is_err());
        let path = store.path(&reference).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&path, b"modified").unwrap();
        assert!(store.get(&reference).is_err());
        assert!(store.put(b"evidence").is_err());
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(temp.path().join("outside"), &path).unwrap();
        assert!(store.get(&reference).is_err());
        assert!(store.put(b"evidence").is_err());
    }
}
