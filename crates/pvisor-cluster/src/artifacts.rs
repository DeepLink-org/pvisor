//! Durable immutable artifact objects. Bulk I/O stays outside the scheduler lock.
use crate::{ARTIFACT_CHUNK_BYTES, ArtifactManifest, BlobRef, LeaseKey};
use anyhow::{Context, ensure};
use fs2::FileExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) mod gc;
mod quota;
pub(crate) use quota::{PublicationFailure, QuotaExceeded};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);
pub fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
    max_bytes: u64,
    quota: std::sync::Arc<quota::Shared>,
}
pub(crate) struct ControllerOwner(std::sync::Arc<quota::Shared>);
impl Drop for ControllerOwner {
    fn drop(&mut self) {
        self.0.controller_claimed.store(false, Ordering::Release);
    }
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
    pub(crate) pins: gc::Pins,
    reference: BlobRef,
    key: LeaseKey,
    run: BundleRunIdentity,
    files: std::collections::BTreeSet<String>,
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
    pub(crate) fn satisfies(&self, retention: &crate::ArtifactRetention) -> bool {
        retention
            .filenames()
            .iter()
            .all(|name| self.files.contains(*name))
    }
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
fn read_regular(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
    let input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(
        input.metadata()?.is_file() && input.metadata()?.len() <= limit,
        "invalid artifact metadata file"
    );
    let mut bytes = Vec::new();
    input.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "artifact metadata file too large"
    );
    Ok(bytes)
}
impl ArtifactStore {
    pub(crate) fn claim_controller(&self) -> anyhow::Result<ControllerOwner> {
        ensure!(
            self.quota
                .controller_claimed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "artifact store already owned by a controller in this process"
        );
        Ok(ControllerOwner(self.quota.clone()))
    }
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
        let root = root.canonicalize()?;
        let quota = quota::open(&root)?;
        let store = Self {
            root,
            max_bytes,
            quota,
        };
        let usage = store.storage_usage()?;
        ensure!(
            usage
                .stored_bytes
                .checked_add(usage.reserved_bytes)
                .and_then(|bytes| bytes.checked_add(usage.failed_reserved_bytes))
                .is_some_and(|bytes| bytes <= max_bytes),
            "existing artifacts exceed configured byte quota; increase quota before restart"
        );
        ensure!(
            usage
                .stored_objects
                .checked_add(usage.reserved_objects)
                .and_then(|objects| objects.checked_add(usage.failed_reserved_objects))
                .is_some_and(|objects| objects <= MAX_OBJECTS),
            "artifact object count exceeds limit"
        );
        // Migrate/reconcile the legacy capacity marker under the owner lock.
        // Publication accounting is shared by all opens in this process.
        let mut capacity = store.quota_file()?;
        write_usage(&mut capacity, usage.stored_bytes, usage.stored_objects)?;
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
        let _barrier = self
            .quota
            .gc
            .barrier
            .read()
            .map_err(|_| anyhow::anyhow!("GC barrier unavailable"))?;
        self.put_unpinned(bytes)
    }
    fn put_unpinned(&self, bytes: &[u8]) -> anyhow::Result<BlobRef> {
        ensure!(
            bytes.len() <= ARTIFACT_CHUNK_BYTES,
            "artifact object exceeds chunk limit"
        );
        let reference = BlobRef {
            digest: blake3::hash(bytes).to_hex().to_string(),
            bytes: bytes.len() as u64,
        };
        let path = self.path(&reference)?;
        // A legacy interrupted reservation remains fenced until an explicit
        // open reconciles it against the shared inventory.
        let mut capacity = self.quota_file()?;
        ensure!(
            capacity.metadata()?.len() == 24,
            "invalid artifact quota record"
        );
        capacity.seek(SeekFrom::Start(16))?;
        let mut dirty = [0; 8];
        capacity.read_exact(&mut dirty)?;
        if u64::from_le_bytes(dirty) != 0 {
            return Err(CapacityExceeded(
                "artifact reservation uncertain; reopen store to reconcile retained inodes",
            )
            .into());
        }
        drop(capacity);
        let mut reservation =
            match self
                .quota
                .reserve_with_capacity(&reference, &path, self.max_bytes)?
            {
                quota::Admission::Existing => {
                    self.get_unpinned(&reference)?;
                    return Ok(reference);
                }
                quota::Admission::Wait(publication) => {
                    publication.wait()?;
                    self.get_unpinned(&reference)?;
                    return Ok(reference);
                }
                quota::Admission::Reserved(reservation) => reservation,
            };
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
            reservation.mark_temporary(&temporary);
            file.write_all(bytes)?;
            file.sync_all()?;
            match fs::hard_link(&temporary, &path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    self.get_unpinned(&reference)?;
                }
                Err(error) => return Err(error.into()),
            }
            reservation.mark_published();
            File::open(directory)?.sync_all()?;
            Ok(())
        })();
        let removed = if created {
            fs::remove_file(&temporary)
        } else {
            Ok(())
        };
        let outcome = published
            .and(removed.map_err(Into::into))
            .and_then(|()| File::open(directory)?.sync_all().map_err(Into::into));
        reservation.finish(&outcome);
        // Record a new birth before leaving the publication barrier, even when
        // fsync failed after a link. Old GC plans cannot delete a replacement.
        if path.try_exists()? {
            self.quota.gc.published(&reference)?;
        }
        outcome?;
        Ok(reference)
    }
    pub fn get(&self, reference: &BlobRef) -> anyhow::Result<Vec<u8>> {
        let _barrier = self
            .quota
            .gc
            .barrier
            .read()
            .map_err(|_| anyhow::anyhow!("GC barrier unavailable"))?;
        self.get_unpinned(reference)
    }
    fn get_unpinned(&self, reference: &BlobRef) -> anyhow::Result<Vec<u8>> {
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
        let _barrier = self
            .quota
            .gc
            .barrier
            .read()
            .map_err(|_| anyhow::anyhow!("GC barrier unavailable"))?;
        self.read_manifest_unpinned(reference)
    }
    fn read_manifest_unpinned(&self, reference: &BlobRef) -> anyhow::Result<ArtifactManifest> {
        let manifest: ArtifactManifest = serde_json::from_slice(&self.get_unpinned(reference)?)
            .context("invalid artifact manifest JSON")?;
        manifest.validate()?;
        Ok(manifest)
    }
    pub(crate) fn verify(
        &self,
        reference: &BlobRef,
        key: &LeaseKey,
    ) -> anyhow::Result<VerifiedArtifacts> {
        let (manifest, pins) = self.pin_manifest(reference)?;
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
            pins,
            reference: reference.clone(),
            key: key.clone(),
            run: identity.run,
            files: manifest.files.into_iter().map(|file| file.name).collect(),
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
