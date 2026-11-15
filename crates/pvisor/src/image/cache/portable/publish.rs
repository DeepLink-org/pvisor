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

impl PortableCache {
    pub(in crate::image::cache) fn publish_image(
        &self,
        image: &str,
        architecture: &str,
        refresh: bool,
    ) -> anyhow::Result<Response> {
        ensure!(!self.read_only, "read-only cache cannot publish images");
        let platform = platform(architecture)?;
        let (canonical, _) = crate::image::oci::cache_reference(image)?;
        // Observe before resolving the source, never after uploading it.
        let observed = self.observe(&canonical, platform)?;
        let local = crate::image::store::LocalStore::discover(self.local_store.clone())?;
        let staging = local.staging()?;
        let store = ImageStore::new(Some(staging.path().to_owned()))?;
        let prepared = store.prepare_with_refresh(image, architecture, refresh)?;
        Ok(self
            .publish_observed(&store, &prepared, architecture, &canonical, observed)?
            .0)
    }
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
        self.storage.put(key, bytes, true)?;
        let stored = self
            .storage
            .get(key)?
            .context("published object is missing")?;
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
                h.generation
                    .checked_add(1)
                    .context("HEAD generation overflow")
            })?;
        // Stage file-independent chunks on disk so the source is read once and
        // publication never races re-reading a changed source file.
        let staging = tempfile::tempdir_in(&store.root)?;
        let previous = if let Some(object) = &observed {
            let head = decode_head(&object.bytes, &key, platform)?;
            self.load(&Handle {
                image_key: key.clone(),
                platform: platform.into(),
                revision: head.revision[7..].into(),
            })?
            .packed_contents()?
        } else {
            BTreeMap::new()
        };
        let mut entries = Vec::new();
        let mut pending = vec![Vec::new()];
        while let Some(path) = pending.pop() {
            let metadata = super::super::source::stat(store, &image.digest, &path)?;
            if matches!(&metadata,Response::Metadata{kind,..} if kind=="directory") {
                for name in super::super::source::directory(store, &image.digest, &path)?.iter() {
                    let mut child = path.clone();
                    if !child.is_empty() {
                        child.push(b'/');
                    }
                    child.extend_from_slice(name);
                    pending.push(child);
                }
            }
            entries.push(SourceEntry {
                path,
                metadata,
                content: None,
            });
            ensure!(
                entries.len() + pending.len() <= MAX_ENTRIES,
                "image exceeds cache entry limit"
            );
        }
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
                    let len = remaining.min(MAX_READ as u64) as usize;
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
                            packed: None,
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
        let mut uploads = self.plan_packs(&entries, &mut contents, staging.path(), previous)?;
        let mut inventory = BTreeMap::<String, u64>::new();
        for content in contents.values() {
            if let Some(packed) = &content.packed {
                let (key, length) = packed.object();
                inventory.insert(key, length);
            } else {
                for (digest, _) in &content.chunks {
                    let key = data_key(digest)?;
                    if !inventory.contains_key(&key) {
                        let encoded = content::encode_chunk(std::fs::read(
                            staging.path().join(&digest[7..]),
                        )?)?;
                        inventory.insert(key.clone(), encoded.len() as u64);
                        let directory = staging.path().join("encoded-chunks");
                        std::fs::create_dir_all(&directory)?;
                        let path = directory.join(&digest[7..]);
                        std::fs::write(&path, encoded)?;
                        uploads.insert(key, path);
                    }
                }
            }
        }
        let mut metadata = binary::encode(&entries, &contents)?;
        metadata.insert(
            "objects.bin".into(),
            frames::encode(&inventory.into_iter().collect::<Vec<_>>())?,
        );
        let (config_digest, layer_digests) = provenance(store, &image.digest)?;
        metadata.insert(
            "manifest.json".into(),
            serde_json::to_vec(&Manifest {
                format_version: 2,
                reference: canonical.into(),
                platform: platform.into(),
                manifest_digest: image.digest.clone(),
                config_digest,
                layer_digests,
            })?,
        );
        let configuration = Configuration {
            format_version: 2,
            architecture: architecture.into(),
            env: image.env.clone(),
            entrypoint: image.entrypoint.clone(),
            cmd: image.cmd.clone(),
            totals,
        };
        metadata.insert("config.json".into(), serde_json::to_vec(&configuration)?);
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
            format_version: 2,
            image_key: key.clone(),
            platform: platform.into(),
            manifest_digest: image.digest.clone(),
            metadata: descriptors,
        };
        let commit_bytes = serde_json::to_vec(&commit)?;
        ensure!(
            commit_bytes.len() <= MAX_CONTROL
                && metadata
                    .iter()
                    .filter(|(name, _)| name.ends_with(".json"))
                    .all(|(_, b)| b.len() <= MAX_CONTROL),
            "control metadata exceeds limit"
        );
        let revision = hash(&commit_bytes);
        let handle = Handle {
            image_key: key.clone(),
            platform: platform.into(),
            revision: revision[7..].into(),
        };
        let upload = uuid::Uuid::new_v4().to_string();
        let upload_prefix = format!("meta/{key}/platforms/{platform}/uploads/{upload}");
        self.put_verified("format.json", FORMAT.to_vec())?;
        self.put_verified(
            &format!("meta/{key}/identity.json"),
            serde_json::to_vec(&Identity {
                format_version: 2,
                image_key: key,
                reference: canonical.into(),
            })?,
        )?;
        self.put_verified(&format!("{upload_prefix}/plan.json"),serde_json::to_vec(&serde_json::json!({
            "format_version":2,"platform":platform,"revision":revision,"manifest_digest":image.digest,
            "expected_head":observed.as_ref().map(|o|&o.version.e_tag)
        }))?)?;
        // Bound upload workers; all staged data is immutable and source bytes
        // have already been verified. Reused packs are referenced, never rewritten.
        let batch_directories = matches!(&self.storage, Storage::Pool(_))
            && std::env::var_os("PVISOR_IMAGE_BATCH_DIRECTORY_SYNC").as_deref()
                == Some(OsStr::new("1"));
        let directories = std::sync::Mutex::new(std::collections::BTreeSet::new());
        let uploads = uploads.into_iter().collect::<Vec<_>>();
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
            format_version: 2,
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
            if self.storage.get(&key)?.as_ref() != Some(&bytes) {
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
