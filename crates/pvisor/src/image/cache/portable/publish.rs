//! Build one image, upload immutable dependencies, then conditionally commit HEAD.
use super::binary::{SourceContent, SourceEntry};
use super::*;
use crate::image::oci::{ImageStore, PreparedImage};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
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
        let store = ImageStore::new(self.local_store.clone())?;
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
        let mut entries = Vec::new();
        let mut pending = vec![Vec::new()];
        while let Some(path) = pending.pop() {
            let (metadata, _) = super::super::source::handle(
                store,
                Request::Stat {
                    digest: image.digest.clone(),
                    path: path.clone(),
                },
            )?;
            if matches!(&metadata,Response::Metadata{kind,..} if kind=="directory") {
                let mut offset = 0;
                loop {
                    let (response, _) = super::super::source::handle(
                        store,
                        Request::List {
                            digest: image.digest.clone(),
                            path: path.clone(),
                            offset,
                        },
                    )?;
                    let Response::Entries {
                        names, next_offset, ..
                    } = response
                    else {
                        bail!("invalid source directory")
                    };
                    for name in names {
                        let mut child = path.clone();
                        if !child.is_empty() {
                            child.push(b'/');
                        }
                        child.extend(name);
                        pending.push(child);
                    }
                    let Some(next) = next_offset else { break };
                    offset = next;
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
                        && after.ino() == source_inode
                        && after.mode() == *mode
                        && after.uid() == *uid
                        && after.gid() == *gid
                        && after.nlink() == *nlink
                        && after.mtime() == *mtime
                        && after.mtime_nsec() == *mtime_nsec,
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
        let mut metadata = binary::encode(&entries, &contents)?;
        let (config_digest, layer_digests) = provenance(store, &image.digest)?;
        metadata.insert(
            "manifest.json".into(),
            serde_json::to_vec(&Manifest {
                format_version: 1,
                reference: canonical.into(),
                platform: platform.into(),
                manifest_digest: image.digest.clone(),
                config_digest,
                layer_digests,
            })?,
        );
        let configuration = Configuration {
            format_version: 1,
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
            format_version: 1,
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
                format_version: 1,
                image_key: key,
                reference: canonical.into(),
            })?,
        )?;
        self.put_verified(&format!("{upload_prefix}/plan.json"),serde_json::to_vec(&serde_json::json!({
            "format_version":1,"platform":platform,"revision":revision,"manifest_digest":image.digest,
            "expected_head":observed.as_ref().map(|o|&o.version.e_tag)
        }))?)?;
        std::thread::scope(|scope| {
            let workers = (0..objects.len().min(8))
                .map(|worker| {
                    let objects = &objects;
                    let staging = &staging;
                    scope.spawn(move || -> anyhow::Result<()> {
                        for digest in objects.keys().skip(worker).step_by(8) {
                            self.put_verified(
                                &data_key(digest)?,
                                std::fs::read(staging.path().join(&digest[7..]))?,
                            )?;
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
        for (name, bytes) in metadata {
            self.put_verified(&format!("{}/{name}", handle.prefix()), bytes)?;
        }
        self.put_verified(&format!("{}/COMMIT.json", handle.prefix()), commit_bytes)?;
        let loaded = self.load(&handle)?;
        let head = Head {
            format_version: 1,
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
            // this attempt committed, even when its revision matches a rival.
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
