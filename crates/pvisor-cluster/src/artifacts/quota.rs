//! Single-owner inventory with short reservations; object fsyncs run concurrently.
use super::{ArtifactStore, BlobRef, TEMP_ID};
use crate::{ARTIFACT_CHUNK_BYTES, ArtifactStorageLimits, ArtifactStorageUsage, CLUSTER_VERSION};
use anyhow::{Context, ensure};
use fs2::FileExt;
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock, Weak, atomic::Ordering},
};

static OWNERS: OnceLock<Mutex<HashMap<PathBuf, Weak<Shared>>>> = OnceLock::new();
#[derive(Debug)]
pub(super) struct Shared {
    pub gc: Arc<super::gc::State>,
    pub controller_claimed: std::sync::atomic::AtomicBool,
    _owner: File,
    pid: u32,
    inventory: Mutex<Inventory>,
}
#[derive(Debug)]
struct Inventory {
    limits: ArtifactStorageLimits,
    stored_bytes: u64,
    stored_objects: u64,
    reserved_bytes: u64,
    failed_reserved_bytes: u64,
    failed_reserved_objects: u64,
    pending: HashMap<String, Arc<Publication>>,
}
#[derive(Debug, Default)]
pub(super) struct Publication {
    outcome: Mutex<Option<Result<(), PublicationFailure>>>,
    changed: Condvar,
}
impl Publication {
    pub fn wait(&self) -> anyhow::Result<()> {
        let mut result = self
            .outcome
            .lock()
            .map_err(|_| anyhow::anyhow!("publication unavailable"))?;
        while result.is_none() {
            result = self
                .changed
                .wait(result)
                .map_err(|_| anyhow::anyhow!("publication unavailable"))?;
        }
        result.as_ref().unwrap().clone().map_err(anyhow::Error::msg)
    }
}
#[derive(Debug, Clone)]
pub(crate) struct PublicationFailure {
    message: String,
    pub retryable: bool,
}
impl std::fmt::Display for PublicationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for PublicationFailure {}

#[derive(Debug)]
pub(crate) struct QuotaExceeded {
    dimension: &'static str,
    requested: u64,
    limit: u64,
}
impl std::fmt::Display for QuotaExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "artifact storage {} quota exceeded: {} > {}",
            self.dimension, self.requested, self.limit
        )
    }
}
impl std::error::Error for QuotaExceeded {}

pub(super) enum Admission {
    Existing,
    Wait(Arc<Publication>),
    Reserved(Reservation),
}
pub(super) struct Reservation {
    shared: Arc<Shared>,
    reference: BlobRef,
    path: PathBuf,
    publication: Arc<Publication>,
    done: bool,
    linked: bool,
    temporary: Option<PathBuf>,
}
impl Reservation {
    pub fn mark_temporary(&mut self, path: &Path) {
        self.temporary = Some(path.to_owned());
    }
    pub fn mark_published(&mut self) {
        self.linked = true;
    }
    pub fn finish(&mut self, outcome: &anyhow::Result<()>) {
        let published = self.linked
            || match fs::symlink_metadata(&self.path) {
                Ok(_) => true,
                Err(error) => error.kind() != std::io::ErrorKind::NotFound,
            };
        let temporary_left =
            self.temporary
                .as_ref()
                .is_some_and(|path| match fs::symlink_metadata(path) {
                    Ok(_) => true,
                    Err(error) => error.kind() != std::io::ErrorKind::NotFound,
                });
        if let Ok(mut inventory) = self.shared.inventory.lock() {
            inventory.pending.remove(&self.reference.digest);
            inventory.reserved_bytes -= self.reference.bytes;
            if published {
                inventory.stored_bytes += self.reference.bytes;
                inventory.stored_objects += 1;
            } else if temporary_left {
                // An unlink failure cannot refund space still held by our
                // unpublished temporary. Recovery removes it under ownership.
                inventory.failed_reserved_bytes += self.reference.bytes;
                inventory.failed_reserved_objects += 1;
            }
        }
        if let Ok(mut status) = self.publication.outcome.lock() {
            *status = Some(
                outcome
                    .as_ref()
                    .map(|_| ())
                    .map_err(|e| PublicationFailure {
                        message: format!("{e:#}"),
                        retryable: e.downcast_ref::<std::io::Error>().is_some(),
                    }),
            );
            self.publication.changed.notify_all();
        }
        self.done = true;
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.done {
            self.finish(&Err(std::io::Error::other(
                "artifact publication abandoned",
            )
            .into()));
        }
    }
}

pub(super) fn temporary(name: &str) -> bool {
    [".upload-", ".limits-", ".metadata-"].iter().any(|prefix| name.strip_prefix(prefix).is_some_and(|tail| {
        let mut components = tail.split('-');
        matches!((components.next(), components.next(), components.next()),
            (Some(pid), Some(id), None) if !pid.is_empty() && !id.is_empty()
                && pid.bytes().all(|b| b.is_ascii_digit()) && id.bytes().all(|b| b.is_ascii_digit()))
    }))
}
pub(super) fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn scan(root: &Path) -> anyhow::Result<(u64, u64)> {
    let mut bytes = 0u64;
    let mut objects = 0u64;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().context("non-UTF8 artifact entry")?;
        if name == ".store-owner" || name == ".limits.json" || name == ".authority.json" {
            continue;
        }
        if name == ".lease-pins" || name == ".downloads" {
            ensure!(
                entry.file_type()?.is_dir(),
                "invalid artifact pin namespace"
            );
            continue;
        }
        if temporary(name) {
            ensure!(
                entry.file_type()?.is_file(),
                "artifact temporary is not a regular file"
            );
            fs::remove_file(entry.path())?;
            continue;
        }
        ensure!(
            hex(name, 2) && entry.file_type()?.is_dir(),
            "invalid artifact shard"
        );
        for object in fs::read_dir(entry.path())? {
            let object = object?;
            let object_name = object.file_name();
            let object_name = object_name.to_str().context("non-UTF8 artifact object")?;
            ensure!(
                object.file_type()?.is_file(),
                "artifact object/temporary is not a regular file"
            );
            if temporary(object_name) {
                fs::remove_file(object.path())?;
                continue;
            }
            ensure!(
                hex(object_name, 64) && object_name.starts_with(name),
                "invalid artifact object name"
            );
            let size = object.metadata()?.len();
            ensure!(
                size <= ARTIFACT_CHUNK_BYTES as u64,
                "oversized artifact object"
            );
            bytes = bytes
                .checked_add(size)
                .context("artifact byte inventory overflow")?;
            objects = objects
                .checked_add(1)
                .context("artifact object inventory overflow")?;
        }
        File::open(entry.path())?.sync_all()?;
    }
    File::open(root)?.sync_all()?;
    Ok((bytes, objects))
}
pub(super) fn open(root: &Path) -> anyhow::Result<Arc<Shared>> {
    let mut owners = OWNERS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| anyhow::anyhow!("artifact owner registry unavailable"))?;
    owners.retain(|_, owner| owner.strong_count() > 0);
    if let Some(shared) = owners.get(root).and_then(Weak::upgrade)
        && shared.pid == std::process::id()
    {
        return Ok(shared);
    }
    let owner = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(".store-owner"))?;
    ensure!(
        owner.metadata()?.is_file(),
        "artifact owner lock is not a regular file"
    );
    owner
        .try_lock_exclusive()
        .context("artifact store already owned by another process")?;
    let limits_path = root.join(".limits.json");
    let limits = match super::read_regular(&limits_path, 8192) {
        Ok(bytes) => serde_json::from_slice::<ArtifactStorageLimits>(&bytes)?,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            ArtifactStorageLimits::default()
        }
        Err(error) => return Err(error),
    };
    limits.validate()?;
    let (stored_bytes, stored_objects) = scan(root)?;
    let shared = Arc::new(Shared {
        controller_claimed: std::sync::atomic::AtomicBool::new(false),
        gc: super::gc::State::recover(root)?,
        _owner: owner,
        pid: std::process::id(),
        inventory: Mutex::new(Inventory {
            limits,
            stored_bytes,
            stored_objects,
            reserved_bytes: 0,
            failed_reserved_bytes: 0,
            failed_reserved_objects: 0,
            pending: HashMap::new(),
        }),
    });
    owners.insert(root.to_owned(), Arc::downgrade(&shared));
    Ok(shared)
}
impl Shared {
    pub fn reclaimed(&self, reference: &BlobRef) -> anyhow::Result<()> {
        let mut inventory = self
            .inventory
            .lock()
            .map_err(|_| anyhow::anyhow!("artifact inventory unavailable"))?;
        inventory.stored_bytes = inventory
            .stored_bytes
            .checked_sub(reference.bytes)
            .context("artifact byte inventory underflow")?;
        inventory.stored_objects = inventory
            .stored_objects
            .checked_sub(1)
            .context("artifact object inventory underflow")?;
        Ok(())
    }
    pub fn reserve(
        self: &Arc<Self>,
        reference: &BlobRef,
        path: &Path,
    ) -> anyhow::Result<Admission> {
        let mut inventory = self
            .inventory
            .lock()
            .map_err(|_| anyhow::anyhow!("artifact inventory unavailable"))?;
        // Join a producer before inspecting its newly published link: its
        // directory fsync and accounting may still be in progress.
        if let Some(publication) = inventory.pending.get(&reference.digest) {
            return Ok(Admission::Wait(publication.clone()));
        }
        if path.try_exists()? {
            return Ok(Admission::Existing);
        }
        let requested_bytes = inventory
            .stored_bytes
            .checked_add(inventory.reserved_bytes)
            .and_then(|bytes| bytes.checked_add(inventory.failed_reserved_bytes))
            .and_then(|bytes| bytes.checked_add(reference.bytes))
            .context("artifact reservation overflow")?;
        let requested_objects = inventory
            .stored_objects
            .checked_add(inventory.pending.len() as u64)
            .and_then(|objects| objects.checked_add(inventory.failed_reserved_objects))
            .and_then(|objects| objects.checked_add(1))
            .context("artifact reservation overflow")?;
        for (dimension, requested, limit) in [
            ("bytes", requested_bytes, inventory.limits.max_bytes),
            ("objects", requested_objects, inventory.limits.max_objects),
        ] {
            if let Some(limit) = limit
                && requested > limit
            {
                return Err(QuotaExceeded {
                    dimension,
                    requested,
                    limit,
                }
                .into());
            }
        }
        let publication = Arc::new(Publication::default());
        inventory.reserved_bytes += reference.bytes;
        inventory
            .pending
            .insert(reference.digest.clone(), publication.clone());
        Ok(Admission::Reserved(Reservation {
            shared: self.clone(),
            reference: reference.clone(),
            path: path.to_owned(),
            publication,
            done: false,
            linked: false,
            temporary: None,
        }))
    }
}
fn usage(inventory: &Inventory) -> ArtifactStorageUsage {
    ArtifactStorageUsage {
        version: CLUSTER_VERSION,
        limits: inventory.limits.clone(),
        stored_bytes: inventory.stored_bytes,
        stored_objects: inventory.stored_objects,
        reserved_bytes: inventory.reserved_bytes,
        reserved_objects: inventory.pending.len() as u64,
        failed_reserved_bytes: inventory.failed_reserved_bytes,
        failed_reserved_objects: inventory.failed_reserved_objects,
    }
}
impl ArtifactStore {
    pub fn storage_usage(&self) -> anyhow::Result<ArtifactStorageUsage> {
        let inventory = self
            .quota
            .inventory
            .lock()
            .map_err(|_| anyhow::anyhow!("artifact inventory unavailable"))?;
        Ok(usage(&inventory))
    }
    pub(crate) fn storage_has_headroom(&self) -> bool {
        self.storage_usage().is_ok_and(|usage| {
            let bytes = usage
                .stored_bytes
                .checked_add(usage.reserved_bytes)
                .and_then(|bytes| bytes.checked_add(usage.failed_reserved_bytes));
            let objects = usage
                .stored_objects
                .checked_add(usage.reserved_objects)
                .and_then(|objects| objects.checked_add(usage.failed_reserved_objects));
            bytes.is_some_and(|bytes| usage.limits.max_bytes.is_none_or(|limit| bytes < limit))
                && objects.is_some_and(|objects| {
                    usage.limits.max_objects.is_none_or(|limit| objects < limit)
                })
        })
    }

    pub fn set_storage_limits(&self, limits: ArtifactStorageLimits) -> anyhow::Result<()> {
        self.update_storage_limits(limits).map(|_| ())
    }
    pub fn update_storage_limits(
        &self,
        limits: ArtifactStorageLimits,
    ) -> anyhow::Result<ArtifactStorageUsage> {
        limits.validate()?;
        let mut inventory = self
            .quota
            .inventory
            .lock()
            .map_err(|_| anyhow::anyhow!("artifact inventory unavailable"))?;
        let temporary = self.root.join(format!(
            ".limits-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut renamed = false;
        let write = (|| -> anyhow::Result<()> {
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temporary)?;
            output.write_all(&serde_json::to_vec(&limits)?)?;
            output.sync_all()?;
            fs::rename(&temporary, self.root.join(".limits.json"))?;
            renamed = true;
            File::open(&self.root)?.sync_all()?;
            Ok(())
        })();
        if write.is_ok() {
            inventory.limits = limits.clone();
        } else if renamed {
            // A directory-fsync failure makes installation uncertain. Enforce
            // both policies until restart; never let an uncertain update relax
            // a quota or ignore the newly requested smaller bound.
            fn stricter(old: Option<u64>, new: Option<u64>) -> Option<u64> {
                match (old, new) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (Some(a), None) | (None, Some(a)) => Some(a),
                    (None, None) => None,
                }
            }
            inventory.limits.max_bytes = stricter(inventory.limits.max_bytes, limits.max_bytes);
            inventory.limits.max_objects =
                stricter(inventory.limits.max_objects, limits.max_objects);
        }
        if temporary.try_exists()? {
            fs::remove_file(&temporary)?;
        }
        write?;
        Ok(usage(&inventory))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limits(bytes: u64, objects: u64) -> ArtifactStorageLimits {
        ArtifactStorageLimits {
            version: CLUSTER_VERSION,
            max_bytes: Some(bytes),
            max_objects: Some(objects),
        }
    }
    #[test]
    fn concurrent_unique_and_duplicate_uploads_reserve_once_and_never_exceed_either_limit() {
        let temp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(temp.path()).unwrap();
        store.set_storage_limits(limits(8192, 2)).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(32));
        let mut threads = vec![];
        for index in 0..32 {
            let store = ArtifactStore::open(temp.path()).unwrap(); // Independent opens share ownership/accounting.
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                store.put(&vec![(index % 4) as u8; 4096])
            }));
        }
        let results = threads
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect::<Vec<_>>();
        let published = results
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .map(|reference| (reference.digest.clone(), reference))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(published.len(), 2);
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 16);
        assert!(
            results
                .iter()
                .filter_map(|r| r.as_ref().err())
                .all(|e| e.downcast_ref::<QuotaExceeded>().is_some())
        );
        let usage = store.storage_usage().unwrap();
        assert_eq!(
            (
                usage.stored_bytes,
                usage.stored_objects,
                usage.reserved_bytes,
                usage.reserved_objects
            ),
            (8192, 2, 0, 0)
        );
        for reference in published.values() {
            assert_eq!(store.get(reference).unwrap().len(), 4096);
        }
        store.set_storage_limits(limits(1, 1)).unwrap(); // Shrinking limits keeps published evidence.
        assert!(
            store
                .put(b"new")
                .unwrap_err()
                .downcast_ref::<QuotaExceeded>()
                .is_some()
        );
        for reference in results.iter().filter_map(|r| r.as_ref().ok()) {
            assert_eq!(
                store.put(&store.get(reference).unwrap()).unwrap(),
                *reference
            );
        }
    }
    #[test]
    fn failed_and_abandoned_publication_refund_unwritten_reservations_but_count_published_objects()
    {
        let temp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(temp.path()).unwrap();
        store.set_storage_limits(limits(3, 1)).unwrap();
        let reference = BlobRef {
            digest: super::super::digest(b"one"),
            bytes: 3,
        };
        let path = store.path(&reference).unwrap();
        let Admission::Reserved(reservation) = store.quota.reserve(&reference, &path).unwrap()
        else {
            panic!()
        };
        assert_eq!(store.storage_usage().unwrap().reserved_bytes, 3);
        assert!(store.put(b"two").is_err());
        drop(reservation);
        assert_eq!(store.storage_usage().unwrap().reserved_bytes, 0);
        let shard = path.parent().unwrap();
        std::os::unix::fs::symlink(temp.path(), shard).unwrap();
        assert!(store.put(b"one").is_err());
        assert_eq!(store.storage_usage().unwrap().reserved_bytes, 0);
        fs::remove_file(shard).unwrap();
        let Admission::Reserved(reservation) = store.quota.reserve(&reference, &path).unwrap()
        else {
            panic!()
        };
        fs::create_dir(shard).unwrap();
        fs::write(&path, b"one").unwrap(); // Crash after link, before in-memory finish.
        drop(reservation);
        let usage = store.storage_usage().unwrap();
        assert_eq!(
            (
                usage.stored_bytes,
                usage.stored_objects,
                usage.reserved_bytes
            ),
            (3, 1, 0)
        );
        assert!(store.put(b"two").is_err());
        assert_eq!(store.put(b"one").unwrap(), reference);
    }
    #[test]
    fn exclusive_recovery_rebuilds_usage_cleans_only_owned_temporaries_and_persists_limits() {
        let temp = tempfile::tempdir().unwrap();
        let reference = {
            let store = ArtifactStore::open(temp.path()).unwrap();
            store.set_storage_limits(limits(8, 1)).unwrap();
            store.put(b"evidence").unwrap()
        };
        let shard = temp.path().join(&reference.digest[..2]);
        fs::write(shard.join(".upload-123-456"), b"crash debris").unwrap();
        fs::write(temp.path().join(".limits-123-456"), b"partial config").unwrap();
        let store = ArtifactStore::open(temp.path()).unwrap();
        assert_eq!(store.storage_usage().unwrap().limits, limits(8, 1));
        assert_eq!(store.storage_usage().unwrap().stored_bytes, 8);
        assert!(!shard.join(".upload-123-456").exists());
        assert!(!temp.path().join(".limits-123-456").exists());
        assert_eq!(store.get(&reference).unwrap(), b"evidence");
        assert!(store.put(b"new").is_err());
        drop(store);
        // Unexpected names and symlinks fail closed, without touching their data.
        fs::write(shard.join("user-document"), b"preserve").unwrap();
        assert!(ArtifactStore::open(temp.path()).is_err());
        assert_eq!(fs::read(shard.join("user-document")).unwrap(), b"preserve");
        fs::remove_file(shard.join("user-document")).unwrap();
        std::os::unix::fs::symlink(temp.path().join("outside"), shard.join(".upload-123-456"))
            .unwrap();
        assert!(ArtifactStore::open(temp.path()).is_err());
        assert!(
            fs::symlink_metadata(shard.join(".upload-123-456"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    #[test]
    fn empty_objects_consume_object_budget_and_invalid_or_corrupt_limits_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(temp.path()).unwrap();
        store.set_storage_limits(limits(1, 1)).unwrap();
        let empty = store.put(b"").unwrap();
        assert_eq!(store.storage_usage().unwrap().stored_objects, 1);
        assert_eq!(store.storage_usage().unwrap().stored_bytes, 0);
        assert!(store.put(b"x").is_err());
        assert_eq!(store.put(b"").unwrap(), empty);
        for invalid in [
            limits(0, 1),
            limits(1, 0),
            ArtifactStorageLimits {
                version: 99,
                max_bytes: None,
                max_objects: None,
            },
        ] {
            assert!(store.set_storage_limits(invalid).is_err());
            assert_eq!(store.storage_usage().unwrap().limits, limits(1, 1));
        }
        drop(store);
        fs::write(
            temp.path().join(".limits.json"),
            br#"{"version":1,"max_bytes":0,"max_objects":1}"#,
        )
        .unwrap();
        assert!(ArtifactStore::open(temp.path()).is_err());
    }
    #[test]
    fn leftover_unpublished_temporary_keeps_its_budget_until_exclusive_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(temp.path()).unwrap();
        store.set_storage_limits(limits(3, 1)).unwrap();
        let reference = BlobRef {
            digest: super::super::digest(b"one"),
            bytes: 3,
        };
        let path = store.path(&reference).unwrap();
        let Admission::Reserved(mut reservation) = store.quota.reserve(&reference, &path).unwrap()
        else {
            panic!()
        };
        let shard = path.parent().unwrap();
        fs::create_dir(shard).unwrap();
        let temporary = shard.join(".upload-9999-42");
        fs::write(&temporary, b"o").unwrap();
        reservation.mark_temporary(&temporary);
        reservation.finish(&Err(anyhow::anyhow!("injected unlink failure")));
        let usage = store.storage_usage().unwrap();
        assert_eq!(
            (
                usage.stored_bytes,
                usage.stored_objects,
                usage.reserved_bytes,
                usage.failed_reserved_bytes,
                usage.failed_reserved_objects
            ),
            (0, 0, 0, 3, 1)
        );
        assert!(
            store
                .put(b"two")
                .unwrap_err()
                .downcast_ref::<QuotaExceeded>()
                .is_some()
        );
        drop(reservation);
        drop(store);
        let recovered = ArtifactStore::open(temp.path()).unwrap();
        assert!(!temporary.exists());
        assert_eq!(recovered.storage_usage().unwrap().failed_reserved_bytes, 0);
        assert_eq!(recovered.put(b"two").unwrap().bytes, 3);
    }
    #[test]
    fn independent_objects_publish_while_duplicates_wait_for_the_original_durability_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(temp.path()).unwrap();
        store.set_storage_limits(limits(6, 2)).unwrap();
        let reference = BlobRef {
            digest: super::super::digest(b"one"),
            bytes: 3,
        };
        let path = store.path(&reference).unwrap();
        let Admission::Reserved(mut reservation) = store.quota.reserve(&reference, &path).unwrap()
        else {
            panic!()
        };
        let duplicate = store.clone();
        let (sent, received) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            sent.send(duplicate.put(b"one")).unwrap();
        });
        // The first object's publication is blocked; a distinct producer is not.
        assert_eq!(store.put(b"two").unwrap().bytes, 3);
        assert!(
            received
                .recv_timeout(std::time::Duration::from_millis(20))
                .is_err()
        );
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"one").unwrap();
        File::open(&path).unwrap().sync_all().unwrap();
        File::open(path.parent().unwrap())
            .unwrap()
            .sync_all()
            .unwrap();
        assert!(
            received
                .recv_timeout(std::time::Duration::from_millis(20))
                .is_err()
        );
        reservation.mark_published();
        reservation.finish(&Ok(()));
        assert_eq!(
            received
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .unwrap(),
            reference
        );
        thread.join().unwrap();
        assert_eq!(store.storage_usage().unwrap().stored_objects, 2);
        assert_eq!(store.storage_usage().unwrap().reserved_bytes, 0);
    }
    #[test]
    fn waiting_duplicate_preserves_the_original_transient_error_class() {
        let temp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(temp.path()).unwrap();
        let reference = BlobRef {
            digest: super::super::digest(b"one"),
            bytes: 3,
        };
        let path = store.path(&reference).unwrap();
        let Admission::Reserved(mut reservation) = store.quota.reserve(&reference, &path).unwrap()
        else {
            panic!()
        };
        let Admission::Wait(publication) = store.quota.reserve(&reference, &path).unwrap() else {
            panic!()
        };
        reservation.finish(&Err(
            std::io::Error::from(std::io::ErrorKind::BrokenPipe).into()
        ));
        assert!(
            publication
                .wait()
                .unwrap_err()
                .downcast_ref::<PublicationFailure>()
                .unwrap()
                .retryable
        );
        assert_eq!(store.storage_usage().unwrap().reserved_bytes, 0);
        assert_eq!(store.put(b"one").unwrap(), reference);
    }
}
