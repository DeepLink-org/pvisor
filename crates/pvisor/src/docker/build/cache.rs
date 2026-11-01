use super::*;
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};

pub(super) struct Cache {
    root: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Record {
    image: BuildImage,
    config: Value,
    args: BTreeMap<String, String>,
}

impl Cache {
    pub(super) fn open(store: &Store) -> anyhow::Result<Self> {
        let root = store.root.join("build-cache-v1");
        private_directory(&root)?;
        for name in ["entries", "locks"] {
            private_directory(&root.join(name))?;
        }
        Ok(Self { root })
    }

    fn lock_file(&self, path: &Path) -> anyhow::Result<File> {
        Ok(OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?)
    }

    pub(super) fn lease(&self, exclusive: bool) -> anyhow::Result<File> {
        let file = self.lock_file(&self.root.join("lease"))?;
        if exclusive {
            file.lock_exclusive()?;
        } else {
            FileExt::lock_shared(&file)?;
        }
        Ok(file)
    }

    pub(super) fn key_lock(&self, key: &str) -> anyhow::Result<File> {
        let file = self.lock_file(&self.root.join("locks").join(&key[7..]))?;
        file.lock_exclusive()?;
        Ok(file)
    }

    pub(super) fn load(&self, key: &str, work: &Path) -> anyhow::Result<Option<Built>> {
        let entry = self.root.join("entries").join(&key[7..]);
        if !entry.exists() {
            return Ok(None);
        }
        // Invalid/incomplete cache records are expendable, never authoritative.
        // Verify all blobs before interpreting the tar or configuration.
        let read = || -> anyhow::Result<(Record, String)> {
            let record: Record = serde_json::from_reader(File::open(entry.join("record.json"))?)?;
            let blobs = entry.join("blobs/sha256");
            for blob in fs::read_dir(&blobs)? {
                let blob = blob?;
                let name = blob.file_name();
                let name = name.to_str().context("invalid cache blob name")?;
                ensure!(
                    name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit()),
                    "invalid cache blob"
                );
                ensure!(
                    fs::symlink_metadata(blob.path())?.is_file(),
                    "invalid cache blob type"
                );
                ensure!(
                    file_digest(&blob.path())? == format!("sha256:{name}"),
                    "cache blob mismatch"
                );
            }
            for id in [&record.image.id, &record.image.digest] {
                ensure!(
                    id.starts_with("sha256:")
                        && id.len() == 71
                        && id[7..].bytes().all(|b| b.is_ascii_hexdigit()),
                    "invalid cached digest"
                );
            }
            let manifest: Value =
                serde_json::from_reader(File::open(blobs.join(&record.image.digest[7..]))?)?;
            ensure!(
                manifest["config"]["digest"] == record.image.id,
                "cached config mismatch"
            );
            let config: Value =
                serde_json::from_reader(File::open(blobs.join(&record.image.id[7..]))?)?;
            ensure!(
                config["config"] == record.config,
                "cached metadata mismatch"
            );
            let layer = manifest["layers"][0]["digest"]
                .as_str()
                .context("missing cache layer")?;
            ensure!(
                layer.starts_with("sha256:")
                    && layer.len() == 71
                    && layer[7..].bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid cached layer"
            );
            ensure!(
                config["rootfs"]["diff_ids"][0] == layer,
                "cached layer mismatch"
            );
            Ok((record, layer.to_owned()))
        };
        let (mut record, layer) = match read() {
            Ok(record) => record,
            Err(_) => {
                fs::remove_dir_all(entry)?;
                return Ok(None);
            }
        };
        // Materialization failures (e.g. ENOSPC) are build errors, not evidence
        // of cache corruption. Preserve the verified checkpoint for a retry.
        let layout = work.join(format!("cached-{}", uuid()));
        fs::create_dir(&layout)?;
        fs::create_dir_all(layout.join("blobs/sha256"))?;
        for blob in fs::read_dir(entry.join("blobs/sha256"))? {
            let blob = blob?;
            fs::hard_link(
                blob.path(),
                layout.join("blobs/sha256").join(blob.file_name()),
            )?;
        }
        fs::write(
            layout.join("oci-layout"),
            b"{\"imageLayoutVersion\":\"1.0.0\"}",
        )?;
        let size = fs::metadata(layout.join("blobs/sha256").join(&record.image.digest[7..]))?.len();
        fs::write(
            layout.join("index.json"),
            serde_json::to_vec(
                &json!({"schemaVersion":2,"manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":record.image.digest,"size":size}]}),
            )?,
        )?;
        let root = layout.join("rootfs");
        fs::create_dir(&root)?;
        import_layer(&layout.join("blobs/sha256").join(&layer[7..]), &root)?;
        record.image.cache = BuildSource::Temporary { root };
        fs::write(entry.join("last-used"), crate::unix_now_ms().to_string())?;
        Ok(Some(Built {
            image: record.image,
            config: record.config,
            layout,
            args: record.args,
        }))
    }

    pub(super) fn save(&self, key: &str, built: &Built) -> anyhow::Result<()> {
        let entries = self.root.join("entries");
        let temporary = tempfile::Builder::new()
            .prefix(".pending-")
            .tempdir_in(&entries)?;
        let root = temporary.path();
        fs::create_dir_all(root.join("blobs/sha256"))?;
        for blob in fs::read_dir(built.layout.join("blobs/sha256"))? {
            let blob = blob?;
            let dest = root.join("blobs/sha256").join(blob.file_name());
            fs::copy(blob.path(), &dest)?;
            File::open(dest)?.sync_all()?;
        }
        let record = Record {
            image: built.image.clone(),
            config: built.config.clone(),
            args: built.args.clone(),
        };
        for (name, bytes) in [
            ("record.json", serde_json::to_vec(&record)?),
            ("last-used", crate::unix_now_ms().to_string().into_bytes()),
        ] {
            fs::write(root.join(name), bytes)?;
            File::open(root.join(name))?.sync_all()?;
        }
        Persistence::sync_directory(&root.join("blobs/sha256"))?;
        Persistence::sync_directory(&root.join("blobs"))?;
        Persistence::sync_directory(root)?;
        let destination = entries.join(&key[7..]);
        if destination.exists() {
            fs::remove_dir_all(&destination)?;
        }
        fs::rename(root, destination)?;
        Persistence::sync_directory(&entries)?;
        Ok(())
    }
}

// Hash the private, immutable snapshot, never the live context. Framed records
// distinguish path/type/content boundaries; mtime is deliberately not identity.
pub(super) fn fingerprint(root: &Path, sources: &[PathBuf]) -> anyhow::Result<String> {
    fn walk(
        root: &Path,
        path: &Path,
        hash: &mut Sha256,
        links: &mut BTreeMap<(u64, u64), PathBuf>,
    ) -> anyhow::Result<()> {
        let full = root.join(path);
        let metadata = fs::symlink_metadata(&full)?;
        let kind = if metadata.is_dir() {
            "dir"
        } else if metadata.is_file() {
            "file"
        } else {
            "link"
        };
        let identity = filesystem::identity(&full, &metadata)?;
        let previous = if metadata.is_file() {
            let key = (metadata.dev(), metadata.ino());
            let previous = links.get(&key).cloned();
            links.entry(key).or_insert_with(|| path.to_owned());
            previous
        } else {
            None
        };
        let content = if metadata.is_file() {
            file_digest(&full)?.into_bytes()
        } else if metadata.file_type().is_symlink() {
            fs::read_link(&full)?.as_os_str().as_bytes().to_vec()
        } else {
            vec![]
        };
        let bytes = serde_json::to_vec(&json!([
            path.as_os_str().as_bytes(),
            kind,
            identity,
            previous.map(|p| p.as_os_str().as_bytes().to_vec()),
            content
        ]))?;
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
        if metadata.is_dir() {
            let mut names = fs::read_dir(&full)?
                .map(|e| e.map(|e| e.file_name()))
                .collect::<io::Result<Vec<_>>>()?;
            names.sort();
            for name in names {
                walk(root, &path.join(name), hash, links)?;
            }
        }
        Ok(())
    }
    let mut hash = Sha256::new();
    let mut links = BTreeMap::new();
    for source in sources {
        walk(root, source, &mut hash, &mut links)?;
    }
    Ok(format!("sha256:{}", hex(&hash.finalize())))
}

/// Selection rules for local build-cache reclamation.
#[derive(Clone, Debug, Default)]
pub struct PruneOptions {
    /// Also remove checkpoints matching a currently published image. Never
    /// removes the published image itself or its pooled data.
    pub all: bool,
    /// Only remove entries unused for at least this duration.
    pub until: Option<Duration>,
    /// Stop once total cache size is at most this many bytes, evicting least
    /// recently used eligible entries first. Protected/age-filtered records may
    /// exceed this target; one eviction can bring usage below it.
    /// None removes every eligible entry.
    pub keep_storage: Option<u64>,
}

/// Logical bytes and number of deleted cache records, not filesystem free space.
#[derive(Clone, Debug, Default)]
pub struct PruneReport {
    /// Removed RUN checkpoint records.
    pub records: usize,
    /// Sum of removed regular-file lengths; hardlinks may delay physical freeing.
    pub bytes: u64,
}

/// Reclaim local RUN checkpoints, waiting for active builds to finish first.
///
/// Takes an exclusive build-cache lease and never touches images, containers,
/// volumes or temporary build workspaces. Default protects checkpoints whose
/// configuration IDs are still published. I/O failure returns an error; records
/// already removed stay removed. Abandoned atomic-write staging is also reclaimed.
pub fn prune(store: &Store, options: &PruneOptions) -> anyhow::Result<PruneReport> {
    let cache = Cache::open(store)?;
    let _lease = cache.lease(true)?;
    let _catalog = store.lock()?;
    let published: BTreeSet<_> = store.catalog()?.values().map(|i| i.id.clone()).collect();
    fn size(path: &Path) -> anyhow::Result<u64> {
        let meta = fs::symlink_metadata(path)?;
        if meta.is_dir() {
            let mut bytes = 0;
            for child in fs::read_dir(path)? {
                bytes += size(&child?.path())?;
            }
            Ok(bytes)
        } else {
            Ok(meta.len())
        }
    }
    let mut total = 0;
    let mut candidates = Vec::new();
    let now = crate::unix_now_ms();
    for entry in fs::read_dir(cache.root.join("entries"))? {
        let path = entry?.path();
        let bytes = size(&path)?;
        total += bytes;
        let used = fs::read_to_string(path.join("last-used"))
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let record = File::open(path.join("record.json"))
            .ok()
            .and_then(|f| serde_json::from_reader::<_, Record>(f).ok());
        let protected = record.is_some_and(|r| published.contains(&r.image.id));
        if (!protected || options.all)
            && options
                .until
                .is_none_or(|age| u128::from(now.saturating_sub(used)) >= age.as_millis())
        {
            candidates.push((used, path, bytes));
        }
    }
    candidates.sort();
    let mut report = PruneReport::default();
    for (_, path, bytes) in candidates {
        if options.keep_storage.is_some_and(|keep| total <= keep) {
            break;
        }
        fs::remove_dir_all(path)?;
        total = total.saturating_sub(bytes);
        report.records += 1;
        report.bytes += bytes;
    }
    // The exclusive lease guarantees that no builder holds, or is waiting on,
    // a per-key lock. Retire orphaned lock inodes without split-lock races.
    for lock in fs::read_dir(cache.root.join("locks"))? {
        let lock = lock?;
        if !cache.root.join("entries").join(lock.file_name()).exists() {
            fs::remove_file(lock.path())?;
        }
    }
    Persistence::sync_directory(&cache.root.join("entries"))?;
    Ok(report)
}
