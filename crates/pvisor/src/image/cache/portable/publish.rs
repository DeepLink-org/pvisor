//! Build one image, upload immutable dependencies, then conditionally commit HEAD.
use super::binary::{SourceContent, SourceEntry};
use super::*;
use crate::image::oci::{ImageStore, PreparedImage};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
// Private tuning control for bounded publication experiments; default unchanged.
fn publication_workers(value: Option<&std::ffi::OsStr>) -> anyhow::Result<usize> {
    let Some(value) = value else {
        return Ok(8);
    };
    let value = value
        .to_str()
        .context("PVISOR_IMAGE_PUBLISH_WORKERS must be UTF-8")?;
    ensure!(
        !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit()),
        "PVISOR_IMAGE_PUBLISH_WORKERS must be an integer in 1..=8"
    );
    let workers: usize = value.parse()?;
    ensure!(
        (1..=8).contains(&workers),
        "PVISOR_IMAGE_PUBLISH_WORKERS must be in 1..=8"
    );
    Ok(workers)
}

fn visit_source(
    store: &ImageStore,
    digest: &str,
    mut visit: impl FnMut(Vec<u8>, Response) -> anyhow::Result<bool>,
) -> anyhow::Result<bool> {
    let mut seen = 0;
    let mut pending = vec![Vec::new()];
    while let Some(path) = pending.pop() {
        let metadata = super::super::source::stat(store, digest, &path)?;
        if matches!(&metadata, Response::Metadata {kind, ..} if kind == "directory") {
            for name in super::super::source::directory(store, digest, &path)?.iter() {
                let mut child = path.clone();
                if !child.is_empty() {
                    child.push(b'/');
                }
                child.extend_from_slice(name);
                pending.push(child);
            }
        }
        seen += 1;
        ensure!(
            seen + pending.len() <= MAX_ENTRIES,
            "image exceeds cache entry limit"
        );
        if !visit(path, metadata)? {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) fn supports_extracted_run(
    store: &ImageStore,
    digest: &str,
    prefix: Option<&crate::image::oci::PrefixView>,
) -> anyhow::Result<bool> {
    let Some(prefix) = prefix else {
        return supports_merged_run(store, digest);
    };
    let start = std::time::Instant::now();
    let qualified = prefix.backing_directories().is_ok_and(|(base, upper)| {
        super::super::source::supports_private_view(&base, &upper, MAX_ENTRIES, MAX_SPANS)
    });
    let supported = qualified || supports_merged_run(store, digest)?;
    crate::util::write_private_json(
        &store.root.join("prefix-qualification.json"),
        &serde_json::json!({
            "physical_bounds_sufficient": qualified,
            "elapsed_ms": start.elapsed().as_millis(),
        }),
    )?;
    Ok(supported)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn supports_merged_run(store: &ImageStore, digest: &str) -> anyhow::Result<bool> {
    let mut spans = 0u64;
    visit_source(store, digest, |path, metadata| {
        let Response::Metadata {
            kind,
            size,
            inode,
            nlink,
            mtime_nsec,
            ..
        } = metadata
        else {
            bail!("invalid source metadata")
        };
        ensure!(
            inode > 0 && nlink > 0 && (0..1_000_000_000).contains(&mtime_nsec),
            "invalid source attributes"
        );
        if !matches!(kind.as_str(), "directory" | "file" | "symlink") {
            return Ok(false);
        }
        super::super::source::validate_pool_attributes(store, digest, &path, &kind)?;
        if kind == "file" {
            // A conservative bound before publication deduplicates file contents.
            spans = spans.saturating_add(size.div_ceil(MAX_READ as u64));
        }
        Ok(spans <= MAX_SPANS as u64)
    })
}

impl PortableCache {
    #[cfg(test)]
    pub(in crate::image::cache) fn publish(
        &self,
        store: &ImageStore,
        image: &PreparedImage,
        architecture: &str,
        canonical: &str,
    ) -> anyhow::Result<(Response, Vec<u8>)> {
        self.publish_observed(
            store,
            image,
            architecture,
            canonical,
            self.observe(canonical, platform(architecture)?)?,
        )
    }
    fn put_verified(&self, key: &str, bytes: Vec<u8>) -> anyhow::Result<()> {
        let digest = hash(&bytes);
        let len = bytes.len();
        // Local creation also drains directory durability barriers when the
        // object already exists, including after a concurrent publisher wins.
        let existing = if self.storage.is_local() {
            None
        } else {
            self.storage.get(key)?
        };
        let stored = match existing {
            Some(stored) => stored,
            None => {
                self.storage.put(key, bytes, true)?;
                self.storage
                    .get(key)?
                    .context("published object is missing")?
            }
        };
        ensure!(
            stored.len() == len && hash(&stored) == digest,
            "immutable cache object digest mismatch; repair requires administrative action"
        );
        Ok(())
    }
    pub(super) fn publish_observed(
        &self,
        store: &ImageStore,
        image: &PreparedImage,
        architecture: &str,
        canonical: &str,
        observed: Option<StoredObject>,
    ) -> anyhow::Result<(Response, Vec<u8>)> {
        ensure!(!self.read_only, "cannot publish to a read-only cache");
        let packing = Format::for_write(&self.storage)?;
        let version = Version::ImagePackV1;
        let worker_limit =
            publication_workers(std::env::var_os("PVISOR_IMAGE_PUBLISH_WORKERS").as_deref())?;
        let platform = platform(architecture)?;
        crate::image::oci::digest_hex(&image.digest)?;
        let key = image_key(canonical);
        let generation = observed
            .as_ref()
            .map(|o| decode_head(&o.bytes, &key, platform))
            .transpose()?
            .map_or(Ok(1), |h| {
                ensure!(
                    h.format_version == version.record(),
                    "cannot write a prerelease HEAD"
                );
                h.generation
                    .checked_add(1)
                    .context("HEAD generation overflow")
            })?;
        // Stage file-independent chunks on disk so the source is read once and
        // publication never races re-reading a changed source file.
        let staging = tempfile::tempdir_in(&store.root)?;
        let mut entries = Vec::new();
        visit_source(store, &image.digest, |path, metadata| {
            entries.push(SourceEntry {
                path,
                metadata,
                content: None,
            });
            Ok(true)
        })?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let mut contents: BTreeMap<String, SourceContent> = BTreeMap::new();
        let mut objects = BTreeMap::new();
        let mut inodes = HashMap::new();
        let mut links = HashMap::new();
        let mut totals = ImageTotals::default();
        let mut span_count = 0usize;
        for entry in &mut entries {
            let Response::Metadata {
                kind,
                size,
                inode,
                mode,
                uid,
                gid,
                nlink,
                mtime,
                mtime_nsec,
                ..
            } = &entry.metadata
            else {
                bail!("invalid source metadata")
            };
            ensure!(
                *inode > 0 && *nlink > 0 && (0..1_000_000_000).contains(mtime_nsec),
                "invalid source attributes"
            );
            let source_inode = *inode;
            if matches!(&self.storage, Storage::Pool(_)) {
                super::super::source::validate_pool_attributes(
                    store,
                    &image.digest,
                    &entry.path,
                    kind,
                )?;
            }
            let portable_inode = inodes.len() as u64 + 1;
            let portable_inode = *inodes.entry(source_inode).or_insert(portable_inode);
            let mut content_digest = None;
            if kind == "file" {
                totals.files += 1;
                totals.bytes = totals
                    .bytes
                    .checked_add(*size)
                    .context("image size overflow")?;
                let (parent, name) =
                    super::super::source::parent(store, &image.digest, &entry.path)?;
                let mut file =
                    super::super::source::open_child(&parent, OsStr::from_bytes(&name), false)?;
                let before = file.metadata()?;
                ensure!(
                    before.is_file()
                        && before.ino() == source_inode
                        && before.size() == *size
                        && serde_json::to_vec(&super::super::source::current_stat(
                            store,
                            &image.digest,
                            &entry.path
                        )?)? == serde_json::to_vec(&entry.metadata)?,
                    "source changed during publication"
                );
                let mut hash_file = Sha256::new();
                let mut remaining = *size;
                let mut chunks = Vec::new();
                while remaining > 0 {
                    let len = remaining.min(u64::from(CHUNK_BYTES)) as usize;
                    let mut bytes = vec![0; len];
                    file.read_exact(&mut bytes)
                        .context("source changed during publication")?;
                    hash_file.update(&bytes);
                    let digest = hash(&bytes);
                    if objects.insert(digest.clone(), len as u32).is_none() {
                        std::fs::write(staging.path().join(&digest[7..]), &bytes)?;
                    }
                    chunks.push((digest, len as u32));
                    remaining -= len as u64;
                }
                let mut extra = [0u8; 1];
                let after = file.metadata()?;
                ensure!(
                    file.read(&mut extra)? == 0
                        && after.is_file()
                        && after.size() == *size
                        && after.dev() == before.dev()
                        && after.ino() == source_inode
                        && after.mode() == before.mode()
                        && after.uid() == before.uid()
                        && after.gid() == before.gid()
                        && after.nlink() == *nlink
                        && after.mtime() == *mtime
                        && after.mtime_nsec() == *mtime_nsec
                        && serde_json::to_vec(&super::super::source::current_stat(
                            store,
                            &image.digest,
                            &entry.path
                        )?)? == serde_json::to_vec(&entry.metadata)?,
                    "source changed during publication"
                );
                let digest = format!("sha256:{}", crate::util::encode_hex(&hash_file.finalize()));
                if let Some(previous) = links.insert(
                    source_inode,
                    (
                        digest.clone(),
                        *mode,
                        *uid,
                        *gid,
                        *nlink,
                        *mtime,
                        *mtime_nsec,
                    ),
                ) {
                    ensure!(
                        previous
                            == (
                                digest.clone(),
                                *mode,
                                *uid,
                                *gid,
                                *nlink,
                                *mtime,
                                *mtime_nsec
                            ),
                        "inconsistent hard-link attributes"
                    );
                }
                if !contents.contains_key(&digest) {
                    span_count = span_count
                        .checked_add(chunks.len())
                        .context("chunk count overflow")?;
                    ensure!(span_count <= MAX_SPANS, "image exceeds cache span limit");
                    contents.insert(
                        digest.clone(),
                        SourceContent {
                            size: *size,
                            chunks,
                        },
                    );
                }
                content_digest = Some(digest);
            }
            if let Response::Metadata { inode, .. } = &mut entry.metadata {
                *inode = portable_inode;
            }
            entry.content = content_digest;
        }
        let plan = aggregate::plan(&entries, &contents, staging.path(), packing)?;
        let mut metadata = binary::encode(&entries, &contents, &plan)?;
        let largest = plan.objects.values().copied().max().unwrap_or(1) as usize;
        let worker_limit = worker_limit.min(MAX_OBJECT / largest);
        metadata.insert(
            "objects.bin".into(),
            frames::encode(&plan.objects.into_iter().collect::<Vec<_>>())?,
        );
        let (config_digest, layer_digests) = provenance(store, &image.digest)?;
        metadata.insert(
            "manifest.json".into(),
            serde_json::to_vec(&Manifest {
                format_version: version.record(),
                reference: canonical.into(),
                platform: platform.into(),
                manifest_digest: image.digest.clone(),
                config_digest,
                layer_digests,
            })?,
        );
        let configuration = Configuration {
            format_version: version.record(),
            architecture: architecture.into(),
            env: image.env.clone(),
            entrypoint: image.entrypoint.clone(),
            cmd: image.cmd.clone(),
            totals,
        };
        metadata.insert("config.json".into(), serde_json::to_vec(&configuration)?);
        for (name, bytes) in &metadata {
            ensure!(
                !bytes.is_empty() && bytes.len() <= metadata_limit(name),
                "image metadata exceeds size limit: {name}"
            );
        }
        let descriptors = metadata
            .iter()
            .map(|(name, bytes)| {
                (
                    name.clone(),
                    Descriptor {
                        sha256: hash(bytes),
                        bytes: bytes.len() as u64,
                    },
                )
            })
            .collect();
        let commit = Commit {
            format: Some(format::family()),
            format_version: version.record(),
            image_key: key.clone(),
            platform: platform.into(),
            manifest_digest: image.digest.clone(),
            packing: Some(packing),
            metadata: descriptors,
        };
        let commit_bytes = serde_json::to_vec(&commit)?;
        ensure!(
            commit_bytes.len() <= MAX_CONTROL,
            "image COMMIT exceeds size limit"
        );
        let revision = hash(&commit_bytes);
        let handle = Handle {
            version,
            image_key: key.clone(),
            platform: platform.into(),
            revision: revision[7..].into(),
        };
        let upload = uuid::Uuid::new_v4().to_string();
        let upload_prefix = format!("meta/{key}/platforms/{platform}/uploads/{upload}");
        self.put_verified("format.json", Format::V1(packing).encode()?)?;
        self.put_verified(
            &format!("meta/{key}/identity.json"),
            serde_json::to_vec(&Identity {
                format_version: version.record(),
                image_key: key,
                reference: canonical.into(),
            })?,
        )?;
        self.put_verified(&format!("{upload_prefix}/plan.json"),serde_json::to_vec(&serde_json::json!({
            "format_version":version.record(),"platform":platform,"revision":revision,"manifest_digest":image.digest,
            "expected_head":observed.as_ref().map(|o|&o.version.e_tag)
        }))?)?;
        // Bound concurrent uploads of verified immutable objects.
        let batch_directories = matches!(&self.storage, Storage::Pool(_))
            && std::env::var_os("PVISOR_IMAGE_BATCH_DIRECTORY_SYNC").as_deref()
                == Some(OsStr::new("1"));
        let directories = std::sync::Mutex::new(std::collections::BTreeSet::new());
        let uploads = plan.uploads.into_iter().collect::<Vec<_>>();
        let publication_started = batch_directories.then(std::time::Instant::now);
        std::thread::scope(|scope| {
            let workers = (0..uploads.len().min(worker_limit))
                .map(|worker| {
                    let uploads = &uploads;
                    let directories = &directories;
                    scope.spawn(move || -> anyhow::Result<()> {
                        for (key, path) in uploads.iter().skip(worker).step_by(worker_limit) {
                            let bytes = std::fs::read(path)?;
                            if batch_directories {
                                let Storage::Pool(pool) = &self.storage else { unreachable!() };
                                let expected_len = bytes.len();
                                let expected_digest = hash(&bytes);
                                pool.put_deferred(key, &bytes, directories)?;
                                drop(bytes);
                                let stored = self.storage.get(key)?.context("published object is missing")?;
                                ensure!(stored.len() == expected_len && hash(&stored) == expected_digest,
                                    "immutable cache object digest mismatch; repair requires administrative action");
                            } else { self.put_verified(key, bytes)?; }
                        }
                        Ok(())
                    })
                })
                .collect::<Vec<_>>();
            for worker in workers {
                worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("content publisher panicked"))??;
            }
            Ok::<_, anyhow::Error>(())
        })?;
        if batch_directories {
            let upload_seconds = publication_started.unwrap().elapsed().as_secs_f64();
            let sync_started = std::time::Instant::now();
            let count = super::super::storage::flush_object_directories(&directories)?;
            if std::env::var_os("PVISOR_IMAGE_PUBLICATION_PROFILE").as_deref()
                == Some(OsStr::new("1"))
            {
                crate::diagnostics::diagnostic(format_args!(
                    "pvisor-image-publication {}",
                    serde_json::json!({
                        "content_objects": uploads.len(), "directory_syncs": count,
                        "upload_seconds": upload_seconds,
                        "directory_sync_seconds": sync_started.elapsed().as_secs_f64(),
                        "workers": worker_limit,
                        "manifest_digest": image.digest,
                    })
                ));
            }
        }
        for (name, bytes) in metadata {
            self.put_verified(&format!("{}/{name}", handle.prefix()), bytes)?;
        }
        self.put_verified(&format!("{}/COMMIT.json", handle.prefix()), commit_bytes)?;
        let loaded = self.load(&handle)?;
        let head = Head {
            format_version: version.record(),
            image_key: handle.image_key.clone(),
            platform: platform.into(),
            revision,
            manifest_digest: image.digest.clone(),
            generation,
            published_at: now(),
            publication_id: upload,
        };
        let bytes = serde_json::to_vec(&head)?;
        let key = head_key(canonical, platform);
        if let Err(error) =
            self.storage
                .compare_and_swap(&key, bytes.clone(), observed.map(|o| o.version))
        {
            // SDK retries can turn a lost successful PUT response into a
            // precondition failure. The unique publication ID proves whether
            // this execution committed, even when its revision matches a rival.
            if self.storage.is_local() || !self.storage.matches_bytes(&key, &bytes)? {
                return Err(error.context(
                    "HEAD commit outcome unresolved; re-observe source and HEAD before retrying",
                ));
            }
        }
        // A completion receipt needs PutObject only; offline maintenance can
        // remove stopped uploads without granting routine publishers deletion.
        if let Err(error) = self.storage.put(
            &format!("{upload_prefix}/progress.json"),
            serde_json::to_vec(&serde_json::json!({"state":"committed","revision":head.revision}))?,
            false,
        ) {
            crate::diagnostics::diagnostic(format_args!(
                "cache upload completion receipt: {error:#}"
            ));
        }
        Ok((loaded.prepared(), Vec::new()))
    }
}
fn provenance(
    store: &ImageStore,
    digest: &str,
) -> anyhow::Result<(Option<String>, Option<Vec<String>>)> {
    let path = store
        .root
        .join("metadata/manifests-v1")
        .join(format!("{}.json", &digest[7..]));
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((None, None)),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        bytes.len() <= MAX_CONTROL && hash(&bytes) == digest,
        "cached OCI manifest digest mismatch"
    );
    let manifest: serde_json::Value = serde_json::from_slice(&bytes)?;
    let config = manifest["config"]["digest"]
        .as_str()
        .context("manifest config digest missing")?
        .to_owned();
    crate::image::oci::digest_hex(&config)?;
    let layers = manifest["layers"]
        .as_array()
        .context("manifest layers missing")?
        .iter()
        .map(|layer| {
            let digest = layer["digest"].as_str().context("layer digest missing")?;
            crate::image::oci::digest_hex(digest)?;
            Ok(digest.to_owned())
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok((Some(config), Some(layers)))
}

#[cfg(test)]
mod worker_tests {
    use super::*;

    #[test]
    fn reusing_local_object_rechecks_its_durability_barrier() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::filesystem(dir.path().into(), false).unwrap();
        let key = "data/blobs/object";
        let bytes = b"verified".to_vec();
        storage.put_verified(key, bytes.clone()).unwrap();
        let cache = PortableCache::new(storage.clone(), None, false);
        super::super::super::storage::FAIL_OBJECT_DIRECTORY_SYNC.with(|flag| flag.set(true));
        assert!(cache.put_verified(key, bytes.clone()).is_err());
        assert!(storage.matches_bytes(key, &bytes).unwrap());
        cache.put_verified(key, bytes).unwrap();
    }

    #[test]
    fn publication_worker_limit_is_bounded_and_defaults_to_eight() {
        assert_eq!(publication_workers(None).unwrap(), 8);
        for count in 1..=8 {
            assert_eq!(
                publication_workers(Some(OsStr::new(&count.to_string()))).unwrap(),
                count
            );
        }
        for invalid in [
            "",
            "0",
            "9",
            "-1",
            "+2",
            " 4",
            "1.0",
            "999999999999999999999999",
        ] {
            assert!(publication_workers(Some(OsStr::new(invalid))).is_err());
        }
    }
}
