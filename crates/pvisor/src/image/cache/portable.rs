//! Daemonless cache format: portable metadata plus shared, packed 1 MiB blobs.
use super::storage::{MAX_OBJECT, Storage};
use super::{ImageTotals, MAX_READ, Request, Response, hash};
use anyhow::{Context, bail, ensure};
use lru::LruCache;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

mod publish;
#[cfg(test)]
mod tests;

const MAX_ENTRIES: usize = 200_000;
const MAX_SPANS: usize = 500_000;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Span {
    blob: String,
    offset: u32,
    length: u32,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: Vec<u8>,
    metadata: Response,
    spans: Vec<Span>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    version: u32,
    digest: String,
    architecture: String,
    env: BTreeMap<String, String>,
    entrypoint: Vec<String>,
    cmd: Vec<String>,
    totals: ImageTotals,
    entries: Vec<Entry>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    version: u32,
    image: String,
    architecture: String,
    checked_at: u64,
    digest: String,
    index: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pointer {
    version: u32,
    index: String,
}
struct LoadedIndex {
    index: Index,
    paths: HashMap<Vec<u8>, usize>,
    directories: HashMap<Vec<u8>, Vec<usize>>,
}

pub(super) struct PortableCache {
    storage: Storage,
    local_store: Option<PathBuf>,
    read_only: bool,
    local_objects: Option<PathBuf>,
    indexes: Mutex<LruCache<String, Arc<LoadedIndex>>>,
    blobs: Mutex<LruCache<String, Arc<Vec<u8>>>>,
}
impl PortableCache {
    pub(super) fn new(storage: Storage, local_store: Option<PathBuf>, read_only: bool) -> Self {
        Self {
            storage,
            local_store,
            read_only,
            local_objects: None,
            indexes: Mutex::new(LruCache::new(NonZeroUsize::new(4).unwrap())),
            blobs: Mutex::new(LruCache::new(NonZeroUsize::new(64).unwrap())),
        }
    }
    pub(super) fn with_local_objects(mut self, root: Option<PathBuf>) -> Self {
        self.local_objects = root;
        self
    }
    fn immutable_object(&self, kind: &str, digest: &str, limit: usize) -> anyhow::Result<Vec<u8>> {
        let hex = crate::image::oci::digest_hex(digest)?;
        let key = format!(
            "v1/{kind}/{hex}{}",
            if kind == "indexes" { ".json" } else { "" }
        );
        let local = self
            .local_objects
            .as_ref()
            .map(|root| root.join(kind).join(hex));
        if let Some(path) = &local {
            use std::io::Read;
            use std::os::unix::fs::OpenOptionsExt;
            if let Ok(file) = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
                && file.metadata().is_ok_and(|meta| meta.is_file())
            {
                let mut bytes = Vec::new();
                if file
                    .take((limit + 1) as u64)
                    .read_to_end(&mut bytes)
                    .is_ok()
                    && bytes.len() <= limit
                    && hash(&bytes) == digest
                {
                    return Ok(bytes);
                }
            }
        }
        let bytes = self
            .storage
            .get(&key)?
            .context("missing immutable cache object")?;
        ensure!(
            bytes.len() <= limit && hash(&bytes) == digest,
            "cache {kind} digest mismatch"
        );
        if let Some(path) = local {
            // Local acceleration is optional; a read-only local cache directory
            // must not prevent reading a valid remote object.
            let result = (|| -> anyhow::Result<()> {
                std::fs::create_dir_all(path.parent().unwrap())?;
                crate::util::atomic_write(&path, &bytes, 0o600)?;
                Ok(())
            })();
            if let Err(error) = result {
                crate::diagnostics::diagnostic(format_args!("local cache object write: {error:#}"));
            }
        }
        Ok(bytes)
    }
    pub(super) fn request(&self, request: Request) -> anyhow::Result<(Response, Vec<u8>)> {
        match request {
            Request::Ping => {
                self.storage.get("v1/format")?;
                Ok((Response::Ready, Vec::new()))
            }
            Request::Prepare {
                image,
                architecture,
                refresh,
            } => self.prepare(&image, &architecture, refresh),
            Request::List {
                digest,
                path,
                offset,
            } => {
                validate_path(&path)?;
                let index = self.for_digest(&digest)?;
                let entry = index.entry(&path)?;
                ensure!(
                    matches!(&entry.metadata, Response::Metadata { kind, .. } if kind == "directory"),
                    "list requires a directory"
                );
                let children = index
                    .directories
                    .get(&path)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                ensure!(offset <= children.len(), "directory offset out of range");
                let mut names = Vec::new();
                let mut metadata = Vec::new();
                let mut frame_size = 128;
                for child in children.iter().skip(offset).take(256) {
                    let entry = &index.index.entries[*child];
                    let name = entry.path.rsplit(|c| *c == b'/').next().unwrap().to_vec();
                    let size = serde_json::to_vec(&name)?.len()
                        + serde_json::to_vec(&entry.metadata)?.len()
                        + 2;
                    if frame_size + size > super::protocol::MAX_FRAME {
                        ensure!(!names.is_empty(), "directory entry exceeds protocol limit");
                        break;
                    }
                    frame_size += size;
                    names.push(name);
                    metadata.push(entry.metadata.clone());
                }
                let end = offset + names.len();
                Ok((
                    Response::Entries {
                        names,
                        metadata: Some(metadata),
                        next_offset: (end < children.len()).then_some(end),
                    },
                    Vec::new(),
                ))
            }
            Request::Stat { digest, path } => {
                validate_path(&path)?;
                Ok((
                    self.for_digest(&digest)?.entry(&path)?.metadata.clone(),
                    Vec::new(),
                ))
            }
            Request::Read {
                digest,
                path,
                offset,
                length,
            } => {
                validate_path(&path)?;
                ensure!(
                    length > 0 && length <= MAX_READ,
                    "read length must be 1..={MAX_READ}"
                );
                let index = self.for_digest(&digest)?;
                let entry = index.entry(&path)?;
                let Response::Metadata { kind, size, .. } = &entry.metadata else {
                    bail!("invalid index metadata")
                };
                ensure!(kind == "file", "only regular files can be read");
                let end = offset.saturating_add(length as u64).min(*size);
                let mut body = Vec::new();
                let mut position = 0u64;
                for span in &entry.spans {
                    let next = position + span.length as u64;
                    if position < end && next > offset {
                        let blob = self.blob(&span.blob)?;
                        let start = span.offset as usize + offset.saturating_sub(position) as usize;
                        let stop = span.offset as usize + (end.min(next) - position) as usize;
                        ensure!(stop <= blob.len(), "truncated cache blob");
                        body.extend_from_slice(&blob[start..stop]);
                    }
                    position = next;
                    if position >= end {
                        break;
                    }
                }
                ensure!(
                    body.len() as u64 == end.saturating_sub(offset),
                    "incomplete cache file"
                );
                Ok((
                    Response::Data {
                        length: body.len() as u32,
                        sha256: hash(&body),
                    },
                    body,
                ))
            }
        }
    }
    fn prepare(
        &self,
        image: &str,
        architecture: &str,
        refresh: bool,
    ) -> anyhow::Result<(Response, Vec<u8>)> {
        ensure!(
            matches!(architecture, "amd64" | "arm64"),
            "unsupported image architecture"
        );
        ensure!(
            !refresh || !self.read_only,
            "read-only cache cannot refresh; refresh with a publisher"
        );
        let (canonical, pinned) = crate::image::oci::cache_reference(image)?;
        let key = reference_key(&canonical, architecture);
        let mut expired = false;
        if !refresh && let Some(bytes) = self.storage.get(&key)? {
            let reference: Reference =
                serde_json::from_slice(&bytes).context("invalid cache reference")?;
            ensure!(
                reference.version == 1
                    && reference.image == canonical
                    && reference.architecture == architecture,
                "incompatible cache reference"
            );
            if self.read_only || pinned || now().saturating_sub(reference.checked_at) < 300 {
                let index = self.load_index(&reference.index)?;
                ensure!(
                    index.index.digest == reference.digest
                        && index.index.architecture == architecture,
                    "cache reference does not match index"
                );
                return Ok((prepared(&index.index, &reference.index), Vec::new()));
            }
            expired = true;
        }
        ensure!(
            !self.read_only,
            "image is absent from read-only cache; publish it with `pvisor cache prepare` first"
        );
        let store = crate::image::oci::ImageStore::new(self.local_store.clone())?;
        let image = store.prepare_with_refresh(image, architecture, refresh || expired)?;
        self.publish(&store, &image, architecture, &canonical)
    }
    fn for_digest(&self, digest: &str) -> anyhow::Result<Arc<LoadedIndex>> {
        let hex = crate::image::oci::digest_hex(digest)?;
        let mut indexes = self.indexes.lock().unwrap();
        let cached = indexes
            .iter()
            .find_map(|(key, loaded)| (loaded.index.digest == digest).then(|| key.clone()));
        if let Some(key) = cached {
            return Ok(indexes.get(&key).unwrap().clone());
        }
        drop(indexes);
        let bytes = self
            .storage
            .get(&format!("v1/images/{hex}.json"))?
            .ok_or_else(not_found)?;
        let pointer: Pointer = serde_json::from_slice(&bytes).context("invalid image pointer")?;
        ensure!(pointer.version == 1, "unsupported cache format");
        let index = self.load_index(&pointer.index)?;
        ensure!(index.index.digest == digest, "cache image digest mismatch");
        Ok(index)
    }
    fn load_index(&self, digest: &str) -> anyhow::Result<Arc<LoadedIndex>> {
        crate::image::oci::digest_hex(digest)?;
        if let Some(index) = self.indexes.lock().unwrap().get(digest).cloned() {
            return Ok(index);
        }
        let bytes = self.immutable_object("indexes", digest, MAX_OBJECT)?;
        let index: Index = serde_json::from_slice(&bytes).context("invalid cache index")?;
        let index = Arc::new(LoadedIndex::validate(index)?);
        self.indexes
            .lock()
            .unwrap()
            .put(digest.into(), index.clone());
        Ok(index)
    }
    fn blob(&self, digest: &str) -> anyhow::Result<Arc<Vec<u8>>> {
        crate::image::oci::digest_hex(digest)?;
        if let Some(bytes) = self.blobs.lock().unwrap().get(digest).cloned() {
            return Ok(bytes);
        }
        let bytes = self.immutable_object("blobs", digest, MAX_READ as usize)?;
        let bytes = Arc::new(bytes);
        self.blobs.lock().unwrap().put(digest.into(), bytes.clone());
        Ok(bytes)
    }
}
impl LoadedIndex {
    fn entry(&self, path: &[u8]) -> anyhow::Result<&Entry> {
        let index = self.paths.get(path).ok_or_else(not_found)?;
        Ok(&self.index.entries[*index])
    }
    fn validate(index: Index) -> anyhow::Result<Self> {
        ensure!(
            index.version == 1 && matches!(index.architecture.as_str(), "amd64" | "arm64"),
            "unsupported cache index"
        );
        crate::image::oci::digest_hex(&index.digest)?;
        ensure!(
            index.entries.len() <= MAX_ENTRIES,
            "image exceeds cache entry limit"
        );
        let mut paths = HashMap::new();
        let mut directories: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
        let mut totals = ImageTotals::default();
        let mut span_count = 0usize;
        for (i, entry) in index.entries.iter().enumerate() {
            validate_path(&entry.path)?;
            ensure!(
                paths.insert(entry.path.clone(), i).is_none(),
                "duplicate cache path"
            );
            let Response::Metadata {
                kind,
                size,
                inode,
                target,
                ..
            } = &entry.metadata
            else {
                bail!("index contains non-metadata response")
            };
            ensure!(
                *inode > 0 && matches!(kind.as_str(), "directory" | "file" | "symlink" | "special"),
                "invalid cache metadata"
            );
            ensure!(
                (kind == "symlink") == target.is_some(),
                "invalid symlink metadata"
            );
            let mut total = 0u64;
            span_count = span_count
                .checked_add(entry.spans.len())
                .context("cache span count overflow")?;
            ensure!(span_count <= MAX_SPANS, "image exceeds cache span limit");
            for span in &entry.spans {
                crate::image::oci::digest_hex(&span.blob)?;
                ensure!(
                    span.length > 0
                        && span
                            .offset
                            .checked_add(span.length)
                            .is_some_and(|end| end <= MAX_READ),
                    "invalid cache span"
                );
                total = total
                    .checked_add(span.length as u64)
                    .context("cache span size overflow")?;
            }
            if kind == "file" {
                ensure!(total == *size, "cache file spans do not match size");
                totals.files += 1;
                totals.bytes = totals
                    .bytes
                    .checked_add(*size)
                    .context("cache totals overflow")?;
            } else {
                ensure!(entry.spans.is_empty(), "non-file has content spans");
            }
            if !entry.path.is_empty() {
                let parent = entry
                    .path
                    .iter()
                    .rposition(|c| *c == b'/')
                    .map_or(&[][..], |i| &entry.path[..i]);
                directories.entry(parent.to_vec()).or_default().push(i);
            }
        }
        let root = paths.get(&Vec::new()).context("cache index has no root")?;
        ensure!(
            matches!(&index.entries[*root].metadata, Response::Metadata { kind, .. } if kind == "directory"),
            "cache root is not a directory"
        );
        for (parent, children) in &mut directories {
            let i = paths.get(parent).context("cache index lacks parent")?;
            ensure!(
                matches!(&index.entries[*i].metadata, Response::Metadata { kind, .. } if kind == "directory"),
                "cache parent is not a directory"
            );
            children.sort_by(|a, b| index.entries[*a].path.cmp(&index.entries[*b].path));
        }
        ensure!(totals == index.totals, "cache image totals mismatch");
        Ok(Self {
            index,
            paths,
            directories,
        })
    }
}
fn prepared(index: &Index, generation: &str) -> Response {
    Response::Prepared {
        metadata_generation: Some(generation.into()),
        totals: Some(index.totals),
        digest: index.digest.clone(),
        architecture: index.architecture.clone(),
        env: index.env.clone(),
        entrypoint: index.entrypoint.clone(),
        cmd: index.cmd.clone(),
    }
}
fn validate_path(path: &[u8]) -> anyhow::Result<()> {
    ensure!(
        !path.contains(&0)
            && (path.is_empty()
                || path
                    .split(|c| *c == b'/')
                    .all(|part| !part.is_empty() && part != b"." && part != b"..")),
        "cache path must be relative without dot or parent components"
    );
    Ok(())
}
fn reference_key(image: &str, architecture: &str) -> String {
    format!(
        "v1/refs/{}.json",
        &hash(format!("{image}\0{architecture}").as_bytes())[7..]
    )
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn not_found() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "cache path or image not found",
    )
}
