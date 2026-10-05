//! Durable immutable artifact objects. Bulk I/O stays outside the scheduler lock.
use crate::{ARTIFACT_CHUNK_BYTES, ArtifactManifest, BlobRef, LeaseKey};
use anyhow::{Context, ensure};
use fs2::FileExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);
pub fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
    max_bytes: u64,
    poisoned: Arc<AtomicBool>,
}
pub(crate) const DEFAULT_MAX_ARTIFACT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_OBJECTS: u64 = 1_000_000;
#[derive(Debug)]
pub(crate) struct CapacityExceeded(pub &'static str);
impl std::fmt::Display for CapacityExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for CapacityExceeded {}
fn reserve_usage(file: &mut File, bytes: u64, objects: u64) -> std::io::Result<()> {
    // Persist the dirty marker before changing counters. All other store
    // instances must refuse writes after a partial/uncertain reservation.
    file.seek(SeekFrom::Start(16))?;
    file.write_all(&1u64.to_le_bytes())?;
    file.sync_all()?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&bytes.to_le_bytes())?;
    file.write_all(&objects.to_le_bytes())?;
    file.set_len(24)?;
    file.sync_all()
}
fn clear_usage(file: &mut File) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(16))?;
    file.write_all(&0u64.to_le_bytes())?;
    file.sync_all()
}
fn write_usage(file: &mut File, bytes: u64, objects: u64) -> std::io::Result<()> {
    reserve_usage(file, bytes, objects)?;
    clear_usage(file)
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
        Self::open_with_quota(root, DEFAULT_MAX_ARTIFACT_BYTES)
    }
    pub fn open_with_quota(root: &Path, max_bytes: u64) -> anyhow::Result<Self> {
        ensure!(max_bytes > 0, "artifact quota must be positive");
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
        let store = Self {
            root: root.to_owned(),
            max_bytes,
            poisoned: Arc::new(AtomicBool::new(false)),
        };
        let mut quota = store.quota_file()?;
        // Rebuild under the same cross-instance writer lock after a crash.
        // Include abandoned upload inodes; do not delete unrooted evidence.
        let mut seen = std::collections::HashSet::new();
        let mut bytes = 0u64;
        for shard in fs::read_dir(root)? {
            let shard = shard?;
            if shard.file_name() == ".capacity" {
                continue;
            }
            let name = shard.file_name();
            ensure!(
                name.to_str()
                    .is_some_and(|s| s.len() == 2 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                    && fs::symlink_metadata(shard.path())?.is_dir(),
                "invalid artifact shard"
            );
            for entry in fs::read_dir(shard.path())? {
                let entry = entry?;
                let metadata = fs::symlink_metadata(entry.path())?;
                ensure!(
                    metadata.is_file() && metadata.len() <= ARTIFACT_CHUNK_BYTES as u64,
                    "invalid artifact object"
                );
                if seen.insert((metadata.dev(), metadata.ino())) {
                    ensure!(
                        seen.len() as u64 <= MAX_OBJECTS,
                        "artifact object count exceeds limit"
                    );
                    bytes = bytes
                        .checked_add(metadata.len())
                        .context("artifact usage overflow")?;
                }
            }
        }
        ensure!(
            bytes <= max_bytes,
            "existing artifacts exceed configured byte quota; increase quota before restart"
        );
        write_usage(&mut quota, bytes, seen.len() as u64)?;
        File::open(root)?.sync_all()?;
        Ok(store)
    }
    fn quota_file(&self) -> anyhow::Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.root.join(".capacity"))?;
        ensure!(
            file.metadata()?.is_file(),
            "artifact quota record must be a regular file"
        );
        file.lock_exclusive()?;
        Ok(file)
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
        let mut quota = self.quota_file()?;
        if path.try_exists()? {
            self.get(&reference)?;
            return Ok(reference);
        }
        if self.poisoned.load(Ordering::Acquire) {
            return Err(
                CapacityExceeded("artifact write uncertain; restart to reconcile quota").into(),
            );
        }
        ensure!(
            quota.metadata()?.len() == 24,
            "invalid artifact quota record"
        );
        let mut usage = [0; 24];
        quota.read_exact(&mut usage)?;
        let used = u64::from_le_bytes(usage[..8].try_into().unwrap());
        let objects = u64::from_le_bytes(usage[8..16].try_into().unwrap());
        if u64::from_le_bytes(usage[16..].try_into().unwrap()) != 0 {
            return Err(CapacityExceeded(
                "artifact reservation uncertain; reopen store to reconcile retained inodes",
            )
            .into());
        }
        let next = used
            .checked_add(reference.bytes)
            .context("artifact quota overflow")?;
        if next > self.max_bytes || objects >= MAX_OBJECTS {
            return Err(CapacityExceeded("artifact capacity reached; retain existing evidence and increase quota or perform offline rooted cleanup").into());
        }
        // Reserve before creating bytes; a failed/uncertain write fences further
        // writes by this instance. Startup reconciles actual retained inodes.
        self.poisoned.store(true, Ordering::Release);
        reserve_usage(&mut quota, next, objects + 1)?;
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
        clear_usage(&mut quota)?;
        self.poisoned.store(false, Ordering::Release);
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
    fn uncertain_reservation_fences_other_instances_until_inode_reconciliation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("objects");
        let first = ArtifactStore::open_with_quota(&root, 5).unwrap();
        first.put(b"abc").unwrap();
        let second = ArtifactStore::open_with_quota(&root, 5).unwrap();
        let mut quota = first.quota_file().unwrap();
        reserve_usage(&mut quota, 5, 2).unwrap();
        drop(quota);
        assert!(
            second
                .put(b"de")
                .unwrap_err()
                .downcast_ref::<CapacityExceeded>()
                .is_some()
        );
        let reconciled = ArtifactStore::open_with_quota(&root, 5).unwrap();
        assert_eq!(reconciled.put(b"de").unwrap().bytes, 2);
    }

    #[test]
    fn byte_quota_survives_reopen_and_deduplication_does_not_consume_capacity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("objects");
        let first = ArtifactStore::open_with_quota(&root, 5).unwrap();
        let reference = first.put(b"abc").unwrap();
        let second = ArtifactStore::open_with_quota(&root, 5).unwrap();
        assert_eq!(second.put(b"abc").unwrap(), reference);
        assert!(
            first
                .put(b"xyz")
                .unwrap_err()
                .downcast_ref::<CapacityExceeded>()
                .is_some()
        );
        assert_eq!(second.put(b"de").unwrap().bytes, 2);
        assert!(first.put(b"f").is_err());
        assert_eq!(
            ArtifactStore::open_with_quota(&root, 5)
                .unwrap()
                .get(&reference)
                .unwrap(),
            b"abc"
        );
        assert!(ArtifactStore::open_with_quota(&root, 4).is_err());
    }

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
