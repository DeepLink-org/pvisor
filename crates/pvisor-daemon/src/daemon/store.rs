use super::models::{Record, Registry};
use anyhow::{Context, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    version: u32,
    owner: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRecord {
    owner: String,
    record: Record,
}

#[derive(Serialize)]
struct RecordRef<'a> {
    owner: &'a str,
    record: &'a Record,
}

#[derive(Default)]
struct Layout {
    initialized: bool,
    sizes: BTreeMap<String, usize>,
    total: usize,
}

pub(super) struct Store {
    directory: PathBuf,
    owner: String,
    layout: Mutex<Layout>,
    _lock: File,
    #[cfg(test)]
    commit_hook: Mutex<Option<std::sync::Arc<CommitHook>>>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommitPoint {
    BeforeDisk,
    AfterRename,
    AfterUnlink,
}

#[cfg(test)]
type CommitHook = dyn Fn(CommitPoint) -> anyhow::Result<()> + Send + Sync;

#[cfg(test)]
fn invoke_commit_hook(hook: Option<&CommitHook>, point: CommitPoint) -> anyhow::Result<()> {
    if let Some(hook) = hook {
        hook(point)?;
    }
    Ok(())
}

impl Store {
    pub fn open(directory: &Path) -> anyhow::Result<(Self, Registry)> {
        fs::create_dir_all(directory)?;
        check_directory(directory)?;
        #[cfg(unix)]
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        let directory = directory.canonicalize()?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        let lock = options.open(directory.join("daemon.lock"))?;
        lock.try_lock_exclusive()
            .context("another daemon owns this state directory")?;
        let path = directory.join("sandboxes.json");
        let mut registry = match fs::symlink_metadata(&path) {
            Ok(_) => read_json::<Registry>(&path)?.0,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ensure!(
                    !directory.join("records").try_exists()?,
                    "registry header missing; refusing to forget individual records"
                );
                let registry = Registry {
                    version: 1,
                    owner: uuid::Uuid::new_v4().to_string(),
                    sandboxes: BTreeMap::new(),
                };
                replace_json(&directory, "sandboxes.json", &registry)?;
                registry
            }
            Err(error) => return Err(error.into()),
        };
        ensure!(
            matches!(registry.version, 1 | 2),
            "unsupported registry version"
        );
        uuid::Uuid::parse_str(&registry.owner).context("invalid node owner")?;
        let mut layout = Layout::default();
        if registry.version == 2 {
            ensure!(
                registry.sandboxes.is_empty(),
                "v2 header cannot contain records"
            );
            let (records, loaded) = load_records(&directory.join("records"), &registry.owner)?;
            registry.sandboxes = records;
            layout = loaded;
        } else {
            for (id, record) in &registry.sandboxes {
                validate_record(id, record)?;
            }
        }
        let store = Self {
            directory,
            owner: registry.owner.clone(),
            layout: Mutex::new(layout),
            _lock: lock,
            #[cfg(test)]
            commit_hook: Mutex::new(None),
        };
        Ok((store, registry))
    }

    #[cfg(test)]
    pub(super) fn set_commit_hook(
        &self,
        hook: impl Fn(CommitPoint) -> anyhow::Result<()> + Send + Sync + 'static,
    ) {
        *self.commit_hook.lock().unwrap() = Some(std::sync::Arc::new(hook));
    }

    /// Activate individual records only AFTER the runtime accepts the old owner.
    /// The root header is the activation boundary: a v1 header always owns its
    /// complete snapshot; a v2 header always requires its committed records tree.
    pub fn initialize(&self, registry: &mut Registry) -> anyhow::Result<()> {
        ensure!(registry.owner == self.owner, "registry owner changed");
        let mut layout = self
            .layout
            .lock()
            .map_err(|_| anyhow::anyhow!("store lock poisoned"))?;
        reclaim_migration_artifacts(&self.directory)?;
        if layout.initialized {
            ensure!(registry.version == 2, "registry layout changed");
            return Ok(());
        }
        ensure!(registry.version == 1, "invalid migration source");
        let records = self.directory.join("records");
        if records.try_exists()? {
            // v1 is authoritative. A disposable, partially reclaimed copy must
            // never become a prerequisite for recovering its intact snapshot.
            validate_migration_artifact(&records)?;
            let retired = self
                .directory
                .join(format!(".records-retired-{}", uuid::Uuid::new_v4()));
            fs::rename(&records, retired)?;
            File::open(&self.directory)?.sync_all()?;
            reclaim_migration_artifacts(&self.directory)?;
        }
        let temporary = self
            .directory
            .join(format!(".records-{}", uuid::Uuid::new_v4()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(&temporary)?;
        let result = (|| {
            let metadata = Metadata {
                version: 2,
                owner: self.owner.clone(),
            };
            write_new(
                &temporary.join("meta.json"),
                &serde_json::to_vec(&metadata)?,
            )?;
            let mut next = Layout {
                initialized: true,
                ..Default::default()
            };
            for (id, record) in &registry.sandboxes {
                validate_record(id, record)?;
                let bytes = serde_json::to_vec(&RecordRef {
                    owner: &self.owner,
                    record,
                })?;
                let size = record_weight(id, record)?;
                next.total = next
                    .total
                    .checked_add(size)
                    .context("registry size overflow")?;
                next.sizes.insert(id.clone(), size);
                check_budget(&self.owner, next.total, next.sizes.len())?;
                write_new(&temporary.join(format!("{id}.json")), &bytes)?;
            }
            File::open(&temporary)?.sync_all()?;
            fs::rename(&temporary, &records)?;
            File::open(&self.directory)?.sync_all()?;
            let header = Registry {
                version: 2,
                owner: self.owner.clone(),
                sandboxes: BTreeMap::new(),
            };
            replace_json(&self.directory, "sandboxes.json", &header)?;
            *layout = next;
            registry.version = 2;
            Ok(())
        })();
        if result.is_err() {
            // If the header rename committed, this open fails and the next open
            // rehydrates v2. Never continue writes with uncertain activation.
            let _ = fs::remove_dir_all(temporary);
        }
        result
    }

    /// One operation changes one sandbox. Atomic replacement/unlink plus a
    /// directory sync is its durability boundary; the header is never rewritten.
    pub fn commit_record(&self, id: &str, record: Option<&Record>) -> anyhow::Result<()> {
        validate_id(id)?;
        if let Some(record) = record {
            validate_record(id, record)?;
        }
        let bytes = record
            .map(|record| {
                serde_json::to_vec(&RecordRef {
                    owner: &self.owner,
                    record,
                })
            })
            .transpose()?;
        let mut layout = self
            .layout
            .lock()
            .map_err(|_| anyhow::anyhow!("store lock poisoned"))?;
        ensure!(
            layout.initialized,
            "individual record store not initialized"
        );
        let size = record
            .map(|record| record_weight(id, record))
            .transpose()?
            .unwrap_or(0);
        let next_total = layout
            .total
            .checked_sub(layout.sizes.get(id).copied().unwrap_or(0))
            .and_then(|n| n.checked_add(size))
            .context("registry size overflow")?;
        let count = layout.sizes.len() - usize::from(layout.sizes.contains_key(id))
            + usize::from(record.is_some());
        check_budget(&self.owner, next_total, count)?;
        #[cfg(test)]
        let hook = self.commit_hook.lock().unwrap().clone();
        #[cfg(test)]
        invoke_commit_hook(hook.as_deref(), CommitPoint::BeforeDisk)?;
        let records = self.directory.join("records");
        check_directory(&records)?;
        let name = format!("{id}.json");
        if let Some(bytes) = bytes {
            replace_bytes(
                &records,
                &name,
                &bytes,
                #[cfg(test)]
                hook.as_deref(),
            )?;
            layout.sizes.insert(id.to_owned(), size);
        } else {
            if layout.sizes.contains_key(id) {
                fs::remove_file(records.join(&name))?;
                #[cfg(test)]
                invoke_commit_hook(hook.as_deref(), CommitPoint::AfterUnlink)?;
                File::open(&records)?.sync_all()?;
                layout.sizes.remove(id);
            }
        }
        layout.total = next_total;
        Ok(())
    }
}

fn validate_id(id: &str) -> anyhow::Result<()> {
    let raw = id.strip_prefix("sb-").context("invalid sandbox identity")?;
    let uuid = uuid::Uuid::parse_str(raw).context("invalid sandbox identity")?;
    ensure!(id == format!("sb-{uuid}"), "invalid sandbox identity");
    Ok(())
}

fn validate_record(id: &str, record: &Record) -> anyhow::Result<()> {
    validate_id(id)?;
    ensure!(
        id == record.sandbox.id
            && record.cpu_millis > 0
            && record.memory_bytes > 0
            && record.endpoint_token.len() >= 32,
        "invalid sandbox reservation or credential"
    );
    Ok(())
}

fn check_directory(path: &Path) -> anyhow::Result<()> {
    ensure!(
        fs::symlink_metadata(path)?.is_dir(),
        "invalid registry directory"
    );
    Ok(())
}

fn load_records(path: &Path, owner: &str) -> anyhow::Result<(BTreeMap<String, Record>, Layout)> {
    check_directory(path)?;
    let (metadata, _) = read_json::<Metadata>(&path.join("meta.json"))?;
    ensure!(
        metadata.version == 2 && metadata.owner == owner,
        "record store owner/version mismatch"
    );
    let mut records = BTreeMap::new();
    let mut layout = Layout {
        initialized: true,
        ..Default::default()
    };
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().context("invalid record filename")?;
        if name == "meta.json" {
            continue;
        }
        if let Some(raw) = name.strip_prefix(".record-") {
            // A crash before rename can leave an uncommitted private temporary.
            let uuid = uuid::Uuid::parse_str(raw).context("invalid temporary record")?;
            ensure!(raw == uuid.to_string(), "invalid temporary record");
            ensure!(
                entry.file_type()?.is_file(),
                "invalid temporary record type"
            );
            fs::remove_file(entry.path())?;
            continue;
        }
        let id = name
            .strip_suffix(".json")
            .context("invalid record filename")?;
        validate_id(id)?;
        let (stored, _) = read_json::<StoredRecord>(&entry.path())?;
        ensure!(stored.owner == owner, "sandbox record owner mismatch");
        validate_record(id, &stored.record)?;
        let size = record_weight(id, &stored.record)?;
        layout.total = layout
            .total
            .checked_add(size)
            .context("registry size overflow")?;
        layout.sizes.insert(id.to_owned(), size);
        check_budget(owner, layout.total, layout.sizes.len())?;
        records.insert(id.to_owned(), stored.record);
    }
    Ok((records, layout))
}

// Account the logical snapshot, not repeated per-file owner envelopes, so a
// valid v1 snapshot at the existing limit remains migratable and manageable.
fn record_weight(id: &str, record: &Record) -> anyhow::Result<usize> {
    Ok(serde_json::to_vec(record)?.len() + id.len() + 4)
}

fn check_budget(owner: &str, total: usize, count: usize) -> anyhow::Result<()> {
    let header = Registry {
        version: 1,
        owner: owner.to_owned(),
        sandboxes: BTreeMap::new(),
    };
    let size = total
        .checked_add(serde_json::to_vec(&header)?.len())
        .and_then(|n| n.checked_sub(usize::from(count != 0)))
        .context("registry size overflow")?;
    ensure!(size <= MAX_BYTES, "registry size limit exceeded");
    Ok(())
}

fn validate_migration_artifact(path: &Path) -> anyhow::Result<()> {
    check_directory(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(path)?;
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() }
                && metadata.permissions().mode() & 0o077 == 0,
            "migration directory is not privately owned"
        );
    }
    // Payload may be incomplete after interrupted writes/reclamation. Validate
    // only the reserved file names and filesystem ownership/types, never follow
    // symlinks or require unactivated data to deserialize successfully.
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        ensure!(
            entry.file_type()?.is_file(),
            "unsafe migration artifact type"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = entry.metadata()?;
            ensure!(
                metadata.uid() == unsafe { libc::geteuid() }
                    && metadata.permissions().mode() & 0o077 == 0,
                "migration file is not privately owned"
            );
        }
        let name = entry.file_name();
        let name = name.to_str().context("invalid migration filename")?;
        if name == "meta.json" {
            continue;
        }
        if let Some(raw) = name.strip_prefix(".record-") {
            let uuid = uuid::Uuid::parse_str(raw)?;
            ensure!(raw == uuid.to_string(), "invalid migration temporary");
        } else {
            validate_id(
                name.strip_suffix(".json")
                    .context("invalid migration filename")?,
            )?;
        }
    }
    Ok(())
}

fn reclaim_migration_artifacts(directory: &Path) -> anyhow::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(raw) = name
            .strip_prefix(".records-retired-")
            .or_else(|| name.strip_prefix(".records-"))
        else {
            continue;
        };
        let uuid = uuid::Uuid::parse_str(raw).context("invalid migration directory name")?;
        ensure!(raw == uuid.to_string(), "invalid migration directory name");
        validate_migration_artifact(&entry.path())?;
        fs::remove_dir_all(entry.path())?;
        File::open(directory)?.sync_all()?;
    }
    Ok(())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> anyhow::Result<(T, usize)> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_BYTES as u64,
        "invalid or oversized registry file"
    );
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options.open(path)?;
    let size = usize::try_from(file.metadata()?.len())?;
    ensure!(size <= MAX_BYTES, "registry size limit exceeded");
    Ok((
        serde_json::from_reader(file)
            .context("invalid registry; refusing to forget native ownership")?,
        size,
    ))
}

fn write_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn replace_json<T: Serialize>(directory: &Path, name: &str, value: &T) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= MAX_BYTES, "registry size limit exceeded");
    replace_bytes(
        directory,
        name,
        &bytes,
        #[cfg(test)]
        None,
    )
}

fn replace_bytes(
    directory: &Path,
    name: &str,
    bytes: &[u8],
    #[cfg(test)] hook: Option<&CommitHook>,
) -> anyhow::Result<()> {
    let temporary = directory.join(format!(".record-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        write_new(&temporary, bytes)?;
        fs::rename(&temporary, directory.join(name))?;
        #[cfg(test)]
        invoke_commit_hook(hook, CommitPoint::AfterRename)?;
        File::open(directory)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::super::models::{Image, Sandbox, SandboxStatus};
    use super::*;
    use chrono::Utc;

    const ID: &str = "sb-12345678-1234-4234-8234-123456789abc";
    const OTHER: &str = "sb-22345678-1234-4234-8234-123456789abc";

    fn record(id: &str) -> Record {
        Record {
            sandbox: Sandbox {
                id: id.into(),
                status: SandboxStatus {
                    state: "Pending".into(),
                    message: None,
                    last_transition_at: None,
                },
                created_at: Utc::now(),
                expires_at: None,
                image: Image {
                    uri: "fixture".into(),
                    auth: None,
                },
                entrypoint: vec!["/work".into()],
                metadata: BTreeMap::new(),
            },
            env: BTreeMap::from([("SECRET".into(), "fixture-secret".into())]),
            cpu_millis: 1000,
            memory_bytes: 64 * 1024 * 1024,
            endpoint_token: "fixture-endpoint-token-at-least-32-bytes".into(),
        }
    }

    #[test]
    fn exclusive_owner_and_corruption_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let (store, first) = Store::open(directory.path()).unwrap();
        assert!(Store::open(directory.path()).is_err());
        drop(store);
        let (store, second) = Store::open(directory.path()).unwrap();
        assert_eq!(first.owner, second.owner);
        drop(store);
        fs::write(directory.path().join("sandboxes.json"), b"broken").unwrap();
        assert!(Store::open(directory.path()).is_err());
    }

    #[test]
    fn updates_and_deletes_touch_only_the_target_record_and_survive_restart() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut registry) = Store::open(directory.path()).unwrap();
        store.initialize(&mut registry).unwrap();
        let header = fs::read(directory.path().join("sandboxes.json")).unwrap();
        let first = record(ID);
        let mut second = record(OTHER);
        store.commit_record(ID, Some(&first)).unwrap();
        store.commit_record(OTHER, Some(&second)).unwrap();
        let first_bytes = fs::read(directory.path().join(format!("records/{ID}.json"))).unwrap();
        second.sandbox.status.state = "Running".into();
        store.commit_record(OTHER, Some(&second)).unwrap();
        assert_eq!(
            fs::read(directory.path().join("sandboxes.json")).unwrap(),
            header
        );
        assert_eq!(
            fs::read(directory.path().join(format!("records/{ID}.json"))).unwrap(),
            first_bytes
        );
        store.commit_record(ID, None).unwrap();
        drop(store);
        let (store, restored) = Store::open(directory.path()).unwrap();
        assert_eq!(restored.version, 2);
        assert_eq!(restored.sandboxes.len(), 1);
        assert_eq!(restored.sandboxes[OTHER].sandbox.status.state, "Running");
        assert_eq!(
            restored.sandboxes[OTHER].endpoint_token,
            second.endpoint_token
        );
        assert_eq!(restored.sandboxes[OTHER].env, second.env);
        drop(store);
    }

    #[test]
    fn migration_keeps_v1_authoritative_until_the_header_activates_v2() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut old) = Store::open(directory.path()).unwrap();
        old.sandboxes.insert(ID.into(), record(ID));
        replace_json(directory.path(), "sandboxes.json", &old).unwrap();
        drop(store);
        let (store, mut loaded) = Store::open(directory.path()).unwrap();
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.sandboxes.len(), 1);
        // Runtime construction takes place here, before migration; Store::open
        // has not cleared the old intention or installed a native owner marker.
        assert!(!directory.path().join("records").exists());
        store.initialize(&mut loaded).unwrap();
        let v2 = fs::read(directory.path().join("sandboxes.json")).unwrap();
        // Model a crash just before header activation with a prepared records tree.
        replace_json(directory.path(), "sandboxes.json", &old).unwrap();
        drop(store);
        let (store, mut retry) = Store::open(directory.path()).unwrap();
        assert_eq!(retry.version, 1);
        assert_eq!(
            retry.sandboxes[ID].endpoint_token,
            old.sandboxes[ID].endpoint_token
        );
        store.initialize(&mut retry).unwrap();
        assert_eq!(
            fs::read(directory.path().join("sandboxes.json")).unwrap(),
            v2
        );
        drop(store);
        let (_, loaded) = Store::open(directory.path()).unwrap();
        assert_eq!(loaded.version, 2);
        assert_eq!(loaded.sandboxes.len(), 1);
    }

    #[test]
    fn corrupt_record_or_missing_metadata_never_falls_back_to_the_old_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut registry) = Store::open(directory.path()).unwrap();
        store.initialize(&mut registry).unwrap();
        store.commit_record(ID, Some(&record(ID))).unwrap();
        drop(store);
        let path = directory.path().join(format!("records/{ID}.json"));
        fs::write(&path, b"broken").unwrap();
        assert!(Store::open(directory.path()).is_err());
        fs::remove_file(&path).unwrap();
        fs::remove_file(directory.path().join("records/meta.json")).unwrap();
        assert!(Store::open(directory.path()).is_err());
    }

    #[test]
    fn interrupted_disposal_and_partial_staging_do_not_block_v1_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut registry) = Store::open(directory.path()).unwrap();
        registry.sandboxes.insert(ID.into(), record(ID));
        replace_json(directory.path(), "sandboxes.json", &registry).unwrap();
        let mut activated = registry.clone();
        store.initialize(&mut activated).unwrap();
        // Simulate the older retry crashing halfway through in-place disposal.
        replace_json(directory.path(), "sandboxes.json", &registry).unwrap();
        fs::remove_file(directory.path().join("records/meta.json")).unwrap();
        for prefix in [".records-", ".records-retired-"] {
            let path = directory
                .path()
                .join(format!("{prefix}{}", uuid::Uuid::new_v4()));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(&path).unwrap();
            write_new(&path.join("meta.json"), b"partial-json").unwrap();
            write_new(&path.join(format!("{ID}.json")), b"partial-secret-record").unwrap();
        }
        drop(store);
        let (store, mut restored) = Store::open(directory.path()).unwrap();
        assert_eq!(restored.version, 1);
        assert_eq!(restored.sandboxes[ID].env, registry.sandboxes[ID].env);
        store.initialize(&mut restored).unwrap();
        assert_eq!(restored.version, 2);
        assert!(!fs::read_dir(directory.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".records-")
        }));
        drop(store);
        let (_, restored) = Store::open(directory.path()).unwrap();
        assert_eq!(
            restored.sandboxes[ID].endpoint_token,
            registry.sandboxes[ID].endpoint_token
        );
    }

    #[test]
    fn old_snapshot_at_the_size_limit_migrates_despite_owner_envelope_overhead() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut registry) = Store::open(directory.path()).unwrap();
        for _ in 0..16 {
            let id = format!("sb-{}", uuid::Uuid::new_v4());
            registry.sandboxes.insert(id.clone(), record(&id));
        }
        let id = registry.sandboxes.keys().next().unwrap().clone();
        registry
            .sandboxes
            .get_mut(&id)
            .unwrap()
            .env
            .insert("PADDING".into(), String::new());
        let remaining = MAX_BYTES - serde_json::to_vec(&registry).unwrap().len();
        registry
            .sandboxes
            .get_mut(&id)
            .unwrap()
            .env
            .insert("PADDING".into(), "x".repeat(remaining));
        assert_eq!(serde_json::to_vec(&registry).unwrap().len(), MAX_BYTES);
        let envelope_bytes: usize = registry
            .sandboxes
            .values()
            .map(|record| {
                serde_json::to_vec(&RecordRef {
                    owner: &registry.owner,
                    record,
                })
                .unwrap()
                .len()
            })
            .sum();
        assert!(envelope_bytes > MAX_BYTES);
        replace_json(directory.path(), "sandboxes.json", &registry).unwrap();
        drop(store);
        let (store, mut loaded) = Store::open(directory.path()).unwrap();
        store.initialize(&mut loaded).unwrap();
        let mut oversized = loaded.sandboxes[&id].clone();
        oversized.env.get_mut("PADDING").unwrap().push('x');
        assert!(store.commit_record(&id, Some(&oversized)).is_err());
        drop(store);
        let (store, loaded) = Store::open(directory.path()).unwrap();
        assert_eq!(loaded.sandboxes.len(), 16);
        assert_eq!(loaded.sandboxes[&id].env, registry.sandboxes[&id].env);
        store.commit_record(&id, None).unwrap();
    }

    #[test]
    fn injected_disk_boundaries_keep_old_accounting_and_reopen_the_visible_winner() {
        for point in [
            CommitPoint::BeforeDisk,
            CommitPoint::AfterRename,
            CommitPoint::AfterUnlink,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let (store, mut registry) = Store::open(directory.path()).unwrap();
            store.initialize(&mut registry).unwrap();
            let original = record(ID);
            store.commit_record(ID, Some(&original)).unwrap();
            let header = fs::read(directory.path().join("sandboxes.json")).unwrap();
            let metadata = fs::read(directory.path().join("records/meta.json")).unwrap();
            let (sizes, total) = {
                let layout = store.layout.lock().unwrap();
                (layout.sizes.clone(), layout.total)
            };
            store.set_commit_hook(move |visited| {
                ensure!(visited != point, "injected fault at {point:?}");
                Ok(())
            });
            let mut updated = original.clone();
            updated
                .env
                .insert("EXTRA".into(), "different serialized size".into());
            updated.sandbox.status.state = "Running".into();
            let replacement = (point != CommitPoint::AfterUnlink).then_some(&updated);
            assert!(store.commit_record(ID, replacement).is_err());
            {
                let layout = store.layout.lock().unwrap();
                assert_eq!(layout.sizes, sizes);
                assert_eq!(layout.total, total);
            }
            assert_eq!(
                fs::read(directory.path().join("sandboxes.json")).unwrap(),
                header
            );
            assert_eq!(
                fs::read(directory.path().join("records/meta.json")).unwrap(),
                metadata
            );
            assert!(
                fs::read_dir(directory.path().join("records"))
                    .unwrap()
                    .all(|entry| {
                        !entry
                            .unwrap()
                            .file_name()
                            .to_str()
                            .unwrap()
                            .starts_with(".record-")
                    })
            );
            drop(store);
            let (store, restored) = Store::open(directory.path()).unwrap();
            let expected = match point {
                CommitPoint::BeforeDisk => Some(&original),
                CommitPoint::AfterRename => Some(&updated),
                CommitPoint::AfterUnlink => None,
            };
            assert_eq!(restored.sandboxes.len(), usize::from(expected.is_some()));
            let layout = store.layout.lock().unwrap();
            if let Some(expected) = expected {
                assert_eq!(
                    serde_json::to_vec(&restored.sandboxes[ID]).unwrap(),
                    serde_json::to_vec(expected).unwrap()
                );
                assert_eq!(layout.sizes[ID], record_weight(ID, expected).unwrap());
                assert_eq!(layout.total, layout.sizes[ID]);
            } else {
                assert!(layout.sizes.is_empty());
                assert_eq!(layout.total, 0);
            }
        }
    }

    #[test]
    fn mismatched_metadata_record_owner_or_record_identity_fails_closed() {
        for corruption in [
            "metadata-owner",
            "metadata-version",
            "record-owner",
            "record-id",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let (store, mut registry) = Store::open(directory.path()).unwrap();
            store.initialize(&mut registry).unwrap();
            store.commit_record(ID, Some(&record(ID))).unwrap();
            drop(store);
            let path = if corruption.starts_with("metadata") {
                directory.path().join("records/meta.json")
            } else {
                directory.path().join(format!("records/{ID}.json"))
            };
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            match corruption {
                "metadata-owner" | "record-owner" => {
                    value["owner"] = uuid::Uuid::new_v4().to_string().into()
                }
                "metadata-version" => value["version"] = 1.into(),
                "record-id" => value["record"]["sandbox"]["id"] = OTHER.into(),
                _ => unreachable!(),
            }
            let corrupted = serde_json::to_vec(&value).unwrap();
            fs::write(&path, &corrupted).unwrap();
            let header = fs::read(directory.path().join("sandboxes.json")).unwrap();
            assert!(Store::open(directory.path()).is_err(), "{corruption}");
            assert_eq!(fs::read(&path).unwrap(), corrupted);
            assert_eq!(
                fs::read(directory.path().join("sandboxes.json")).unwrap(),
                header
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn abandoned_migration_symlinks_are_rejected_without_touching_the_target() {
        let directory = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let (store, mut registry) = Store::open(directory.path()).unwrap();
        let path = directory
            .path()
            .join(format!(".records-{}", uuid::Uuid::new_v4()));
        std::os::unix::fs::symlink(external.path(), &path).unwrap();
        assert!(store.initialize(&mut registry).is_err());
        assert!(external.path().exists());
        assert_eq!(registry.version, 1);
    }

    #[cfg(unix)]
    #[test]
    fn record_paths_cannot_escape_or_follow_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let (store, mut registry) = Store::open(directory.path()).unwrap();
        store.initialize(&mut registry).unwrap();
        assert!(
            store
                .commit_record("../../outside", Some(&record(ID)))
                .is_err()
        );
        store.commit_record(ID, Some(&record(ID))).unwrap();
        drop(store);
        let path = directory.path().join(format!("records/{ID}.json"));
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("../sandboxes.json", &path).unwrap();
        assert!(Store::open(directory.path()).is_err());
    }
}
