//! Durable lease pins and bounded, reviewable reclamation. File I/O never
//! acquires the scheduler mutex. The barrier permits parallel publications and
//! reads; each unlink briefly excludes them to close the pin/open race.
use super::*;
use crate::{
    ArtifactDownload, ArtifactGcPlan, ArtifactGcReport, ArtifactGcRequest, ArtifactRetirement,
    CLUSTER_VERSION,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    os::unix::fs::MetadataExt,
    sync::{Arc, Mutex, RwLock},
};
const MAX_REFS: usize = 1025;
const PLAN_TTL: u64 = 300_000;
const DOWNLOAD_TTL: u64 = 300_000;
const DOWNLOAD_MAX: u64 = 3_600_000;

#[derive(Debug, Default)]
pub(super) struct State {
    pub barrier: RwLock<()>,
    inner: Mutex<Inner>,
    pub maintenance: Mutex<()>,
    downloads_io: Mutex<()>,
}
#[derive(Debug, Default)]
struct Inner {
    sequence: u64,
    pins: HashMap<String, u64>,
    births: HashMap<String, u64>,
    leases: HashMap<String, Arc<Mutex<LeasePins>>>,
    downloads: BTreeMap<String, DownloadRecord>,
    plans: BTreeMap<String, StoredPlan>,
}
#[derive(Debug)]
struct LeasePins {
    key: LeaseKey,
    refs: BTreeMap<String, BlobRef>,
    sequence: u64,
    failed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadRecord {
    version: u32,
    id: String,
    created_at_ms: u64,
    expires_at_ms: u64,
    reference: BlobRef,
    refs: Vec<BlobRef>,
}
#[derive(Debug, Clone)]
pub(crate) struct Snapshot {
    pub live: BTreeSet<String>,
    pub retained: Vec<BlobRef>,
    pub retire: Vec<ArtifactRetirement>,
    pub legacy: bool,
}
#[derive(Debug, Clone)]
pub(crate) struct StoredPlan {
    pub plan: ArtifactGcPlan,
    sequence: u64,
    snapshot: Snapshot,
    candidates: Vec<Candidate>,
    lease_cleanup_limit: usize,
    report: Option<ArtifactGcReport>,
}
#[derive(Debug, Clone)]
struct Candidate {
    reference: BlobRef,
    device: u64,
    inode: u64,
    birth: u64,
}

#[derive(Debug)]
pub(crate) struct Pins {
    state: Arc<State>,
    refs: Vec<String>,
}
impl Drop for Pins {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.state.inner.lock() {
            for digest in &self.refs {
                unpin(&mut inner, digest);
            }
        }
    }
}
fn pin(inner: &mut Inner, digest: &str) {
    *inner.pins.entry(digest.into()).or_default() += 1;
}
fn unpin(inner: &mut Inner, digest: &str) {
    if let Some(count) = inner.pins.get_mut(digest) {
        *count -= 1;
        if *count == 0 {
            inner.pins.remove(digest);
        }
    }
}
pub(crate) fn lease_id(key: &LeaseKey) -> String {
    // LeaseKey serialization has a fixed field order and no optional fields.
    digest(&serde_json::to_vec(key).expect("serializable lease key"))
}
fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn refs(reference: &BlobRef, manifest: &ArtifactManifest) -> anyhow::Result<Vec<BlobRef>> {
    let mut values = BTreeMap::from([(reference.digest.clone(), reference.clone())]);
    for chunk in manifest.files.iter().flat_map(|file| &file.chunks) {
        if let Some(previous) = values.insert(chunk.digest.clone(), chunk.clone()) {
            ensure!(
                previous.bytes == chunk.bytes,
                "inconsistent artifact reference size"
            );
        }
    }
    Ok(values.into_values().collect())
}
fn add_refs(inner: &mut Inner, refs: &[BlobRef]) {
    for r in refs {
        pin(inner, &r.digest);
    }
}
fn remove_refs(inner: &mut Inner, refs: &[BlobRef]) {
    for r in refs {
        unpin(inner, &r.digest);
    }
}
fn directory(root: &Path, name: &str) -> anyhow::Result<PathBuf> {
    let path = root.join(name);
    let created = match fs::create_dir(&path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error.into()),
    };
    ensure!(
        fs::symlink_metadata(&path)?.is_dir(),
        "artifact pin directory is not a directory"
    );
    if created {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        File::open(root)?.sync_all()?;
    }
    Ok(path)
}
fn atomic_metadata(directory: &Path, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let temporary = directory.join(format!(
        ".metadata-{}-{}",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, directory.join(name))?;
        File::open(directory)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}
fn frame<T: Serialize>(value: &T) -> anyhow::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value)?;
    let mut line = digest(&bytes).into_bytes();
    line.push(b' ');
    line.extend(bytes);
    line.push(b'\n');
    Ok(line)
}
fn parse_frame<T: for<'de> Deserialize<'de>>(line: &[u8]) -> anyhow::Result<T> {
    ensure!(
        line.len() > 65 && line[64] == b' ',
        "invalid artifact pin frame"
    );
    ensure!(
        digest(&line[65..]).as_bytes() == &line[..64],
        "artifact pin checksum mismatch"
    );
    Ok(serde_json::from_slice(&line[65..])?)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseHeader {
    version: u32,
    key: LeaseKey,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Authority {
    version: u32,
    id: String,
}

impl State {
    pub(super) fn recover(root: &Path) -> anyhow::Result<Arc<Self>> {
        let state = Arc::new(Self::default());
        let mut inner = state
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("GC state unavailable"))?;
        for namespace in [".lease-pins", ".downloads"] {
            let path = root.join(namespace);
            if !path.try_exists()? {
                continue;
            }
            ensure!(
                fs::symlink_metadata(&path)?.is_dir(),
                "invalid artifact pin namespace"
            );
            for entry in fs::read_dir(&path)? {
                let entry = entry?;
                let id = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("invalid pin filename"))?;
                ensure!(
                    entry.file_type()?.is_file(),
                    "artifact pin is not a regular file"
                );
                if super::quota::temporary(&id) {
                    fs::remove_file(entry.path())?;
                    continue;
                }
                ensure!(valid_id(&id), "invalid artifact pin filename");
                let bytes = read_regular(&entry.path(), 256 * 1024)?;
                if namespace == ".lease-pins" {
                    let complete = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |p| p + 1);
                    let mut lines = bytes[..complete]
                        .split(|b| *b == b'\n')
                        .filter(|line| !line.is_empty());
                    let header: LeaseHeader =
                        parse_frame(lines.next().context("missing artifact lease header")?)?;
                    ensure!(
                        header.version == CLUSTER_VERSION && lease_id(&header.key) == id,
                        "invalid artifact lease header"
                    );
                    let mut values = BTreeMap::new();
                    for line in lines {
                        let reference: BlobRef = parse_frame(line)?;
                        reference.validate()?;
                        if let Some(previous) =
                            values.insert(reference.digest.clone(), reference.clone())
                        {
                            ensure!(previous == reference, "conflicting artifact lease pin");
                        }
                        ensure!(values.len() <= MAX_REFS, "too many artifact lease pins");
                    }
                    // An interrupted final append was never acknowledged. Earlier
                    // checksummed frames remain durable under exclusive ownership.
                    if complete < bytes.len() {
                        let file = OpenOptions::new()
                            .write(true)
                            .custom_flags(libc::O_NOFOLLOW)
                            .open(entry.path())?;
                        file.set_len(complete as u64)?;
                        file.sync_all()?;
                    }
                    for reference in values.values() {
                        pin(&mut inner, &reference.digest);
                    }
                    inner.leases.insert(
                        id,
                        Arc::new(Mutex::new(LeasePins {
                            key: header.key,
                            refs: values,
                            sequence: 0,
                            failed: false,
                        })),
                    );
                } else {
                    let record: DownloadRecord = serde_json::from_slice(&bytes)?;
                    ensure!(
                        record.version == CLUSTER_VERSION
                            && record.id == id
                            && record.refs.len() <= MAX_REFS
                            && record.expires_at_ms >= record.created_at_ms
                            && record.expires_at_ms
                                <= record.created_at_ms.saturating_add(DOWNLOAD_MAX),
                        "invalid artifact download record"
                    );
                    record.reference.validate()?;
                    let mut unique = BTreeSet::new();
                    for reference in &record.refs {
                        reference.validate()?;
                        ensure!(unique.insert(&reference.digest), "duplicate download pin");
                    }
                    ensure!(
                        record.refs.contains(&record.reference),
                        "download omits manifest pin"
                    );
                    if record.expires_at_ms <= pvisor_core::unix_now_ms() {
                        fs::remove_file(entry.path())?;
                    } else {
                        add_refs(&mut inner, &record.refs);
                        ensure!(
                            inner.downloads.len() < 128,
                            "too many durable artifact downloads"
                        );
                        inner.downloads.insert(id, record);
                    }
                }
            }
            File::open(path)?.sync_all()?;
        }
        drop(inner);
        Ok(state)
    }
    pub(crate) fn sequence(&self) -> anyhow::Result<u64> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("GC state unavailable"))?
            .sequence)
    }
    pub(super) fn published(&self, reference: &BlobRef) -> anyhow::Result<()> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("GC state unavailable"))?;
        inner.sequence += 1;
        let sequence = inner.sequence;
        inner.births.insert(reference.digest.clone(), sequence);
        Ok(())
    }
    pub(crate) fn pins(self: &Arc<Self>, references: &[BlobRef]) -> anyhow::Result<Pins> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("GC state unavailable"))?;
        let values: BTreeSet<_> = references.iter().map(|r| r.digest.clone()).collect();
        for digest in &values {
            pin(&mut inner, digest);
        }
        Ok(Pins {
            state: self.clone(),
            refs: values.into_iter().collect(),
        })
    }
}
impl ArtifactStore {
    /// Bind object reachability to a durable WAL identity, not a process or
    /// pathname. Backups can move together without permitting a replacement
    /// or unrelated WAL to reclaim the original controller's evidence.
    pub(crate) fn bind_authority(
        &self,
        wal_id: Option<&str>,
        persist_wal_id: impl FnOnce(&str) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let _maintenance = self
            .quota
            .gc
            .maintenance
            .lock()
            .map_err(|_| anyhow::anyhow!("GC maintenance unavailable"))?;
        let stored = match read_regular(&self.root.join(".authority.json"), 256) {
            Ok(bytes) => Some(serde_json::from_slice::<Authority>(&bytes)?),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        if let Some(stored) = &stored {
            ensure!(
                stored.version == CLUSTER_VERSION && valid_id(&stored.id),
                "invalid artifact WAL authority"
            );
            ensure!(
                wal_id == Some(stored.id.as_str()),
                "artifact store belongs to a different WAL; restore the matching WAL and object metadata"
            );
            return Ok(());
        }
        let id = match wal_id {
            Some(id) => {
                ensure!(valid_id(id), "invalid artifact WAL authority");
                id.to_owned()
            }
            None => {
                let id = digest(
                    format!(
                        "{}:{}:{:?}:{}",
                        self.root.display(),
                        std::process::id(),
                        std::time::SystemTime::now(),
                        TEMP_ID.fetch_add(1, Ordering::Relaxed)
                    )
                    .as_bytes(),
                );
                // The WAL identity is durable first. A failed metadata fsync can
                // then be retried with that same identity after restart.
                persist_wal_id(&id)?;
                id
            }
        };
        atomic_metadata(
            &self.root,
            ".authority.json",
            &serde_json::to_vec(&Authority {
                version: CLUSTER_VERSION,
                id,
            })?,
        )
    }
    pub(crate) fn gc_sequence(&self) -> anyhow::Result<u64> {
        self.quota.gc.sequence()
    }
    pub(crate) fn pin_manifest(
        &self,
        reference: &BlobRef,
    ) -> anyhow::Result<(ArtifactManifest, Pins)> {
        let _barrier = self
            .quota
            .gc
            .barrier
            .read()
            .map_err(|_| anyhow::anyhow!("GC barrier unavailable"))?;
        let manifest = self.read_manifest_unpinned(reference)?;
        let references = refs(reference, &manifest)?;
        let pins = self.quota.gc.pins(&references)?;
        // No bulk hashing here: completion verified immutable bodies, while the
        // download client verifies chunks and whole files as it streams them.
        for reference in &references {
            let metadata = fs::symlink_metadata(self.path(reference)?)?;
            ensure!(
                metadata.is_file() && metadata.len() == reference.bytes,
                "missing or invalid pinned artifact"
            );
        }
        Ok((manifest, pins))
    }
    pub fn put_for_lease(&self, key: &LeaseKey, bytes: &[u8]) -> anyhow::Result<BlobRef> {
        let _barrier = self
            .quota
            .gc
            .barrier
            .read()
            .map_err(|_| anyhow::anyhow!("GC barrier unavailable"))?;
        ensure!(
            bytes.len() <= ARTIFACT_CHUNK_BYTES,
            "artifact object exceeds chunk limit"
        );
        let expected = BlobRef {
            digest: digest(bytes),
            bytes: bytes.len() as u64,
        };
        let id = lease_id(key);
        let entry = {
            let mut inner = self
                .quota
                .gc
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("GC state unavailable"))?;
            inner
                .leases
                .entry(id.clone())
                .or_insert_with(|| {
                    Arc::new(Mutex::new(LeasePins {
                        key: key.clone(),
                        refs: BTreeMap::new(),
                        sequence: u64::MAX,
                        failed: false,
                    }))
                })
                .clone()
        };
        let mut lease = entry
            .lock()
            .map_err(|_| anyhow::anyhow!("lease pins unavailable"))?;
        ensure!(
            lease.key == *key && !lease.failed,
            "artifact lease pins require recovery"
        );
        if let Some(previous) = lease.refs.get(&expected.digest) {
            ensure!(*previous == expected, "conflicting artifact lease pin");
            return self.put_unpinned(bytes);
        }
        ensure!(
            lease.refs.len() < MAX_REFS,
            "artifact lease pin limit exceeded"
        );
        let reference = self.put_unpinned(bytes)?;
        let directory = directory(&self.root, ".lease-pins")?;
        let path = directory.join(&id);
        if !path.try_exists()? {
            atomic_metadata(
                &directory,
                &id,
                &frame(&LeaseHeader {
                    version: CLUSTER_VERSION,
                    key: key.clone(),
                })?,
            )?;
        }
        let mut file = OpenOptions::new()
            .append(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)?;
        ensure!(
            file.metadata()?.is_file(),
            "artifact lease pin is not a regular file"
        );
        let line = frame(&reference)?;
        let previous_len = file.metadata()?.len();
        if let Err(error) = file.write_all(&line).and_then(|()| file.sync_all()) {
            // Do not append behind an incomplete frame on the next retry.
            // Failure to truncate leaves the lease fail-closed until restart.
            lease.failed = true;
            file.set_len(previous_len)?;
            file.sync_all()?;
            lease.failed = false;
            return Err(error.into());
        }
        lease
            .refs
            .insert(reference.digest.clone(), reference.clone());
        let mut inner = self
            .quota
            .gc
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("GC state unavailable"))?;
        inner.sequence += 1;
        lease.sequence = inner.sequence;
        pin(&mut inner, &reference.digest);
        Ok(reference)
    }
    pub(crate) fn gc_plan(
        &self,
        request: ArtifactGcRequest,
        snapshot: Snapshot,
        sequence: u64,
        now: u64,
    ) -> anyhow::Result<ArtifactGcPlan> {
        request.validate()?;
        let _maintenance = self
            .quota
            .gc
            .maintenance
            .lock()
            .map_err(|_| anyhow::anyhow!("GC maintenance unavailable"))?;
        let mut marked = BTreeSet::new();
        for reference in &snapshot.retained {
            let (manifest, _pins) = self.pin_manifest(reference)?;
            for reference in refs(reference, &manifest)? {
                marked.insert(reference.digest);
            }
        }
        self.expire_downloads(now)?;
        let leases: Vec<_> = {
            let inner = self
                .quota
                .gc
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("GC state unavailable"))?;
            for download in inner.downloads.values() {
                for reference in &download.refs {
                    marked.insert(reference.digest.clone());
                }
            }
            inner
                .leases
                .iter()
                .filter(|(id, _)| snapshot.live.contains(*id))
                .map(|(_, entry)| entry.clone())
                .collect()
        };
        for entry in leases {
            let lease = entry
                .lock()
                .map_err(|_| anyhow::anyhow!("lease pins unavailable"))?;
            marked.extend(lease.refs.keys().cloned());
        }
        let mut candidates = Vec::new();
        if !snapshot.legacy {
            // The barrier closes metadata sampling/publication races without
            // serializing independent uploads with one another.
            for shard in fs::read_dir(&self.root)? {
                let shard = shard?;
                let name = shard.file_name();
                let name = name.to_str().context("invalid artifact shard")?;
                if [
                    ".store-owner",
                    ".limits.json",
                    ".authority.json",
                    ".lease-pins",
                    ".downloads",
                ]
                .contains(&name)
                    || super::quota::temporary(name)
                {
                    continue;
                }
                ensure!(
                    super::quota::hex(name, 2) && shard.file_type()?.is_dir(),
                    "invalid artifact shard"
                );
                for object in fs::read_dir(shard.path())? {
                    let object = object?;
                    let name = object.file_name();
                    let name = name.to_str().context("invalid object filename")?;
                    if super::quota::temporary(name) {
                        continue;
                    }
                    ensure!(
                        valid_id(name)
                            && name.starts_with(shard.file_name().to_str().unwrap())
                            && object.file_type()?.is_file(),
                        "invalid artifact object"
                    );
                    if marked.contains(name) {
                        continue;
                    }
                    let _barrier = self
                        .quota
                        .gc
                        .barrier
                        .read()
                        .map_err(|_| anyhow::anyhow!("GC barrier unavailable"))?;
                    let metadata = match fs::symlink_metadata(object.path()) {
                        Ok(m) => m,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(e) => return Err(e.into()),
                    };
                    ensure!(
                        metadata.is_file() && metadata.len() <= ARTIFACT_CHUNK_BYTES as u64,
                        "invalid artifact object"
                    );
                    let inner = self
                        .quota
                        .gc
                        .inner
                        .lock()
                        .map_err(|_| anyhow::anyhow!("GC state unavailable"))?;
                    let birth = inner.births.get(name).copied().unwrap_or(0);
                    if birth > sequence {
                        continue;
                    }
                    // Retiring evidence can still be pinned now; applying the
                    // plan releases its durable root before checking live pins.
                    candidates.push(Candidate {
                        reference: BlobRef {
                            digest: name.into(),
                            bytes: metadata.len(),
                        },
                        device: metadata.dev(),
                        inode: metadata.ino(),
                        birth,
                    });
                    if candidates.len() >= request.max_objects as usize {
                        break;
                    }
                }
                if candidates.len() >= request.max_objects as usize {
                    break;
                }
            }
        }
        candidates.sort_by(|a, b| a.reference.digest.cmp(&b.reference.digest));
        let id = digest(
            format!(
                "{}:{}:{}:{}",
                self.root.display(),
                now,
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            )
            .as_bytes(),
        );
        let plan = ArtifactGcPlan {
            version: CLUSTER_VERSION,
            id: id.clone(),
            created_at_ms: now,
            expires_at_ms: now.saturating_add(PLAN_TTL),
            retire: snapshot.retire.clone(),
            objects: candidates.iter().map(|c| c.reference.clone()).collect(),
            bytes: candidates.iter().map(|c| c.reference.bytes).sum(),
            blocked_by_legacy_leases: snapshot.legacy,
        };
        let mut inner = self
            .quota
            .gc
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("GC state unavailable"))?;
        inner.plans.retain(|_, p| p.plan.expires_at_ms > now);
        ensure!(inner.plans.len() < 16, "too many pending artifact GC plans");
        inner.plans.insert(
            id,
            StoredPlan {
                plan: plan.clone(),
                sequence,
                snapshot,
                candidates,
                lease_cleanup_limit: request.max_objects as usize,
                report: None,
            },
        );
        Ok(plan)
    }
    pub(crate) fn stored_plan(&self, id: &str, now: u64) -> anyhow::Result<StoredPlan> {
        ensure!(valid_id(id), "invalid artifact GC plan ID");
        let inner = self
            .quota
            .gc
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("GC state unavailable"))?;
        let plan = inner
            .plans
            .get(id)
            .context("unknown artifact GC plan; preview again after controller restart")?;
        ensure!(plan.plan.expires_at_ms > now, "artifact GC plan expired");
        Ok(plan.clone())
    }
    pub(crate) fn apply_gc(
        &self,
        id: &str,
        now: u64,
        retire: impl FnOnce(&[ArtifactRetirement]) -> anyhow::Result<BTreeSet<String>>,
    ) -> anyhow::Result<ArtifactGcReport> {
        let _maintenance = self
            .quota
            .gc
            .maintenance
            .lock()
            .map_err(|_| anyhow::anyhow!("GC maintenance unavailable"))?;
        let plan = self.stored_plan(id, now)?;
        if let Some(report) = &plan.report {
            return Ok(report.clone());
        }
        let live = retire(&plan.plan.retire)?;
        self.sweep(plan, live, now)
    }
    pub(crate) fn sweep(
        &self,
        plan: StoredPlan,
        live: BTreeSet<String>,
        now: u64,
    ) -> anyhow::Result<ArtifactGcReport> {
        if let Some(report) = plan.report {
            return Ok(report);
        }
        // Caller serializes retire-WAL and sweep with the maintenance mutex.
        self.expire_downloads(now)?;
        let leases: Vec<_> = {
            let inner = self
                .quota
                .gc
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("GC state unavailable"))?;
            inner
                .leases
                .iter()
                .filter(|(id, _)| !live.contains(*id) && !plan.snapshot.live.contains(*id))
                .take(plan.lease_cleanup_limit)
                .map(|(id, entry)| (id.clone(), entry.clone()))
                .collect()
        };
        for (id, entry) in leases {
            if live.contains(&id) || plan.snapshot.live.contains(&id) {
                continue;
            }
            let _barrier = self
                .quota
                .gc
                .barrier
                .write()
                .map_err(|_| anyhow::anyhow!("GC barrier unavailable"))?;
            let lease = entry
                .lock()
                .map_err(|_| anyhow::anyhow!("lease pins unavailable"))?;
            if lease.sequence > plan.sequence {
                continue;
            }
            let directory = self.root.join(".lease-pins");
            match fs::remove_file(directory.join(&id)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            if directory.exists() {
                File::open(directory)?.sync_all()?;
            }
            let mut inner = self
                .quota
                .gc
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("GC state unavailable"))?;
            inner.leases.remove(&id);
            for reference in lease.refs.values() {
                unpin(&mut inner, &reference.digest);
            }
        }
        let mut report = ArtifactGcReport {
            version: CLUSTER_VERSION,
            plan_id: plan.plan.id.clone(),
            retired_tasks: plan.plan.retire.len() as u64,
            deleted_objects: 0,
            deleted_bytes: 0,
            skipped_objects: 0,
        };
        for candidate in &plan.candidates {
            let _barrier = self
                .quota
                .gc
                .barrier
                .write()
                .map_err(|_| anyhow::anyhow!("GC barrier unavailable"))?;
            let removable = {
                let inner = self
                    .quota
                    .gc
                    .inner
                    .lock()
                    .map_err(|_| anyhow::anyhow!("GC state unavailable"))?;
                inner
                    .pins
                    .get(&candidate.reference.digest)
                    .copied()
                    .unwrap_or(0)
                    == 0
                    && inner
                        .births
                        .get(&candidate.reference.digest)
                        .copied()
                        .unwrap_or(0)
                        == candidate.birth
            };
            if !removable {
                report.skipped_objects += 1;
                continue;
            }
            let path = self.path(&candidate.reference)?;
            ensure!(
                fs::symlink_metadata(path.parent().unwrap())?.is_dir(),
                "GC refuses a symlink shard"
            );
            let metadata = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    report.skipped_objects += 1;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            ensure!(metadata.is_file(), "GC refuses nonregular artifact object");
            if metadata.dev() != candidate.device
                || metadata.ino() != candidate.inode
                || metadata.len() != candidate.reference.bytes
            {
                report.skipped_objects += 1;
                continue;
            }
            fs::remove_file(&path)?;
            // Accounting remains conservative if directory durability is uncertain.
            File::open(path.parent().unwrap())?.sync_all()?;
            self.quota.reclaimed(&candidate.reference)?;
            self.quota
                .gc
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("GC state unavailable"))?
                .births
                .remove(&candidate.reference.digest);
            report.deleted_objects += 1;
            report.deleted_bytes += candidate.reference.bytes;
        }
        if let Some(stored) = self
            .quota
            .gc
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("GC state unavailable"))?
            .plans
            .get_mut(&report.plan_id)
        {
            stored.report = Some(report.clone());
        }
        Ok(report)
    }
    pub(crate) fn begin_download(
        &self,
        reference: &BlobRef,
        now: u64,
    ) -> anyhow::Result<ArtifactDownload> {
        let _io = self
            .quota
            .gc
            .downloads_io
            .lock()
            .map_err(|_| anyhow::anyhow!("download metadata unavailable"))?;
        self.expire_downloads_locked(now)?;
        let (manifest, _pins) = self.pin_manifest(reference)?;
        let references = refs(reference, &manifest)?;
        let id = digest(
            format!(
                "{}:{}:{}",
                now,
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            )
            .as_bytes(),
        );
        let record = DownloadRecord {
            version: CLUSTER_VERSION,
            id: id.clone(),
            created_at_ms: now,
            expires_at_ms: now.saturating_add(DOWNLOAD_TTL),
            reference: reference.clone(),
            refs: references,
        };
        ensure!(
            self.quota
                .gc
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("GC state unavailable"))?
                .downloads
                .len()
                < 128,
            "too many artifact downloads"
        );
        let directory = directory(&self.root, ".downloads")?;
        // Install memory protection before metadata I/O. On an uncertain fsync
        // retain the pins until expiry or exclusive restart recovery.
        {
            let mut inner = self
                .quota
                .gc
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("GC state unavailable"))?;
            add_refs(&mut inner, &record.refs);
            inner.downloads.insert(id.clone(), record.clone());
        }
        atomic_metadata(&directory, &id, &serde_json::to_vec(&record)?)?;
        Ok(ArtifactDownload {
            version: CLUSTER_VERSION,
            id,
            expires_at_ms: record.expires_at_ms,
            reference: reference.clone(),
            manifest,
        })
    }
    pub(crate) fn renew_download(&self, id: &str, now: u64) -> anyhow::Result<ArtifactDownload> {
        ensure!(valid_id(id), "invalid artifact download ID");
        let _io = self
            .quota
            .gc
            .downloads_io
            .lock()
            .map_err(|_| anyhow::anyhow!("download metadata unavailable"))?;
        let mut record = self
            .quota
            .gc
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("GC state unavailable"))?
            .downloads
            .get(id)
            .cloned()
            .context("unknown artifact download")?;
        ensure!(
            record.expires_at_ms > now && record.created_at_ms.saturating_add(DOWNLOAD_MAX) > now,
            "artifact download expired"
        );
        record.expires_at_ms = now
            .saturating_add(DOWNLOAD_TTL)
            .min(record.created_at_ms.saturating_add(DOWNLOAD_MAX));
        let manifest = self.read_manifest(&record.reference)?;
        atomic_metadata(
            &self.root.join(".downloads"),
            id,
            &serde_json::to_vec(&record)?,
        )?;
        self.quota
            .gc
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("GC state unavailable"))?
            .downloads
            .insert(id.into(), record.clone());
        Ok(ArtifactDownload {
            version: CLUSTER_VERSION,
            id: id.into(),
            expires_at_ms: record.expires_at_ms,
            reference: record.reference,
            manifest,
        })
    }
    pub(crate) fn release_download(&self, id: &str) -> anyhow::Result<()> {
        ensure!(valid_id(id), "invalid artifact download ID");
        let _io = self
            .quota
            .gc
            .downloads_io
            .lock()
            .map_err(|_| anyhow::anyhow!("download metadata unavailable"))?;
        self.release_download_locked(id)
    }
    fn release_download_locked(&self, id: &str) -> anyhow::Result<()> {
        let directory = self.root.join(".downloads");
        match fs::remove_file(directory.join(id)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        if directory.exists() {
            File::open(directory)?.sync_all()?;
        }
        let mut inner = self
            .quota
            .gc
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("GC state unavailable"))?;
        if let Some(record) = inner.downloads.remove(id) {
            remove_refs(&mut inner, &record.refs);
        }
        Ok(())
    }
    fn expire_downloads(&self, now: u64) -> anyhow::Result<()> {
        let _io = self
            .quota
            .gc
            .downloads_io
            .lock()
            .map_err(|_| anyhow::anyhow!("download metadata unavailable"))?;
        self.expire_downloads_locked(now)
    }
    fn expire_downloads_locked(&self, now: u64) -> anyhow::Result<()> {
        let expired: Vec<_> = self
            .quota
            .gc
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("GC state unavailable"))?
            .downloads
            .values()
            .filter(|d| d.expires_at_ms <= now)
            .map(|d| d.id.clone())
            .collect();
        for id in expired {
            self.release_download_locked(&id)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
