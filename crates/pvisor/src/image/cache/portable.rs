//! V2 compressed directory radix indexes and content-only seekable blobs.
use super::storage::{MAX_OBJECT, Storage, StoredObject};
use super::{ImageTotals, MAX_READ, Request, Response, hash};
use anyhow::{Context, bail, ensure};
use lru::LruCache;
use pvisor_journal::api::{DurableFiles, Persistence};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
mod binary;
mod content;
mod frames;
#[cfg(test)]
mod measurement;
mod publish;
#[cfg(test)]
mod tests;
use binary::LoadedImage;
pub(super) use binary::name_order;
#[cfg(test)]
pub(super) fn root_offset_for_test(bytes: &[u8]) -> usize {
    frames::decode::<binary::Record>(bytes)
        .unwrap()
        .directory
        .unwrap()
        .offset as usize
}
const MAX_ENTRIES: usize = 1_000_000;
const MAX_SPANS: usize = 2_000_000;
pub(super) const MAX_CONTROL: usize = 1024 * 1024;
const BINARY_NAMES: [&str; 2] = ["index.bin", "objects.bin"];
pub(super) const FORMAT: &[u8] = br#"{"format_version":2,"hash_algorithm":"sha256","encoding":"zstd-seekable","chunk_bytes":1048576,"metadata_encoding":"pvisor-radix-v2","metadata_frame_bytes":65536,"route_hash":"xxh3-64","route_seed":"707669736f727632","route_radix_bits":4}"#;
/// Exact immutable metadata basenames accepted by services and pinned proxies.
pub(super) fn metadata_object_name(name: &str) -> bool {
    matches!(
        name,
        "COMMIT.json" | "manifest.json" | "config.json" | "root.bin" | "index.bin" | "objects.bin"
    )
}

pub(super) fn metadata_prefix(handle: &str) -> anyhow::Result<String> {
    Ok(Handle::parse(handle)?.prefix())
}

/// A lazy socket metadata reader pinned to one immutable revision. Its client
/// owns only the transport, never this reader, so retaining the client is acyclic.
pub(super) struct MetadataReader {
    handle: Handle,
    cache: PortableCache,
}

impl MetadataReader {
    /// Validate the handle without connecting or touching the optional cache.
    pub(super) fn new(
        client: Arc<super::CacheClient>,
        handle: String,
        cache: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let storage = Storage::metadata(client, handle.clone())?;
        Ok(Self {
            handle: Handle::parse(&handle)?,
            cache: PortableCache::new(storage, None, true).with_local_objects(cache),
        })
    }

    /// Only exact-handle Stat/List are accepted. Cached objects and pages are
    /// authenticated on use; loading errors propagate without RPC fallback.
    pub(super) fn request(&self, request: Request) -> anyhow::Result<Response> {
        match request {
            Request::Stat { digest, path } => {
                ensure!(
                    digest == self.handle.encode(),
                    "metadata reader handle mismatch"
                );
                validate_path(&path)?;
                Ok(self
                    .cache
                    .load_revision(&self.handle)?
                    .entry(&path)?
                    .metadata)
            }
            Request::List {
                digest,
                path,
                offset,
            } => {
                ensure!(
                    digest == self.handle.encode(),
                    "metadata reader handle mismatch"
                );
                validate_path(&path)?;
                self.cache.load_revision(&self.handle)?.list(&path, offset)
            }
            _ => bail!("metadata reader accepts only Stat/List"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    sha256: String,
    bytes: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    format_version: u32,
    image_key: String,
    reference: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Head {
    format_version: u32,
    image_key: String,
    platform: String,
    revision: String,
    manifest_digest: String,
    generation: u64,
    published_at: u64,
    publication_id: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Commit {
    format_version: u32,
    image_key: String,
    platform: String,
    manifest_digest: String,
    metadata: BTreeMap<String, Descriptor>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format_version: u32,
    reference: String,
    platform: String,
    manifest_digest: String,
    config_digest: Option<String>,
    layer_digests: Option<Vec<String>>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    format_version: u32,
    architecture: String,
    env: BTreeMap<String, String>,
    entrypoint: Vec<String>,
    cmd: Vec<String>,
    totals: ImageTotals,
}
#[derive(Clone, Debug)]
struct Handle {
    image_key: String,
    platform: String,
    revision: String,
}
impl Handle {
    fn parse(value: &str) -> anyhow::Result<Self> {
        let parts: Vec<_> = value.split(':').collect();
        ensure!(
            parts.len() == 4 && parts[0] == "pvisor-v2",
            "expected immutable pvisor-v2 image handle; V1 is retired, republish into a new V2 prefix"
        );
        check_hex(parts[1])?;
        check_hex(parts[3])?;
        platform_architecture(parts[2])?;
        Ok(Self {
            image_key: parts[1].into(),
            platform: parts[2].into(),
            revision: parts[3].into(),
        })
    }
    fn encode(&self) -> String {
        format!(
            "pvisor-v2:{}:{}:{}",
            self.image_key, self.platform, self.revision
        )
    }
    fn prefix(&self) -> String {
        format!(
            "meta/{}/platforms/{}/revisions/{}",
            self.image_key, self.platform, self.revision
        )
    }
}
pub(super) struct PortableCache {
    storage: Storage,
    local_store: Option<PathBuf>,
    read_only: bool,
    local_objects: Option<PathBuf>,
    images: Mutex<LruCache<String, Arc<LoadedImage>>>,
    blobs: Mutex<content::DataCache>,
    flights: frames::Flights,
}
impl PortableCache {
    pub(super) fn publish_prepared(
        &self,
        store: &crate::image::oci::ImageStore,
        image: &crate::image::oci::PreparedImage,
        architecture: &str,
        canonical: &str,
    ) -> anyhow::Result<Response> {
        let observed = self.observe(canonical, platform(architecture)?)?;
        Ok(self
            .publish_observed(store, image, architecture, canonical, observed)?
            .0)
    }
    pub(super) fn new(storage: Storage, local_store: Option<PathBuf>, read_only: bool) -> Self {
        Self {
            storage,
            local_store,
            read_only,
            local_objects: None,
            images: Mutex::new(LruCache::new(NonZeroUsize::new(4).unwrap())),
            blobs: Mutex::new(content::DataCache::new()),
            flights: frames::Flights::default(),
        }
    }
    pub(super) fn with_local_objects(mut self, root: Option<PathBuf>) -> Self {
        self.local_objects = root;
        self
    }
    pub(super) fn request(&self, request: Request) -> anyhow::Result<(Response, Vec<u8>)> {
        match request {
            Request::Open {
                handle,
                architecture,
            } => {
                let handle = Handle::parse(&handle)?;
                ensure!(
                    handle.platform == platform(&architecture)?,
                    "immutable cache handle architecture mismatch"
                );
                Ok((self.load(&handle)?.prepared(), Vec::new()))
            }
            Request::Ping => {
                if let Some(bytes) = self.storage.get("format.json")? {
                    ensure!(bytes == FORMAT, "unsupported cache format");
                }
                Ok((Response::Ready, Vec::new()))
            }
            Request::Prepare {
                image,
                architecture,
                refresh,
            } => self.prepare(&image, &architecture, refresh),
            Request::Metadata {
                handle,
                object_name,
                offset,
                length,
            } => self.metadata(&handle, &object_name, offset, length),
            Request::Stat { digest, path } => {
                validate_path(&path)?;
                Ok((
                    self.load(&Handle::parse(&digest)?)?.entry(&path)?.metadata,
                    Vec::new(),
                ))
            }
            Request::List {
                digest,
                path,
                offset,
            } => {
                validate_path(&path)?;
                Ok((
                    self.load(&Handle::parse(&digest)?)?.list(&path, offset)?,
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
                let image = self.load(&Handle::parse(&digest)?)?;
                let entry = image.entry(&path)?;
                let Response::Metadata { kind, size, .. } = &entry.metadata else {
                    bail!("invalid file record")
                };
                ensure!(kind == "file", "only regular files can be read");
                let end = offset.saturating_add(length as u64).min(*size);
                let mut body = Vec::new();
                if offset < end {
                    match entry.content.context("file lacks content")? {
                        binary::Content::Empty => bail!("nonempty file has empty content"),
                        binary::Content::Packed(packed) => {
                            body = self.read_packed(
                                &packed,
                                &entry.digest.context("file lacks digest")?,
                                *size,
                                offset,
                                end,
                            )?;
                        }
                        binary::Content::Chunks(root) => {
                            ensure!(
                                root.count == size.div_ceil(MAX_READ as u64),
                                "file chunk count mismatch"
                            );
                            for ordinal in offset / MAX_READ as u64..=(end - 1) / MAX_READ as u64 {
                                let (digest, len) = image.chunk(&root, ordinal)?;
                                let position = ordinal * MAX_READ as u64;
                                ensure!(
                                    len as u64 == (*size - position).min(MAX_READ as u64),
                                    "invalid file chunk length"
                                );
                                body.extend(self.read_chunk(
                                    &digest,
                                    len,
                                    offset.saturating_sub(position) as usize,
                                    (end.min(position + len as u64) - position) as usize,
                                )?);
                            }
                        }
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
    fn metadata(
        &self,
        handle: &str,
        object_name: &str,
        offset: u64,
        length: u32,
    ) -> anyhow::Result<(Response, Vec<u8>)> {
        ensure!(
            metadata_object_name(object_name),
            "invalid metadata object name"
        );
        ensure!(
            length > 0 && length <= MAX_READ,
            "invalid metadata read length"
        );
        let end = offset
            .checked_add(length as u64)
            .context("metadata range overflow")?;
        let handle = Handle::parse(handle)?;
        let key = format!("{}/{object_name}", handle.prefix());
        let body = if object_name == "COMMIT.json" {
            let bytes = self.immutable(
                &key,
                &format!("sha256:{}", handle.revision),
                None,
                MAX_CONTROL,
            )?;
            ensure!(offset <= bytes.len() as u64, "metadata offset beyond EOF");
            bytes[offset as usize..end.min(bytes.len() as u64) as usize].to_vec()
        } else {
            let image = self.load_revision(&handle)?;
            let descriptor = &image.commit.metadata[object_name];
            ensure!(offset <= descriptor.bytes, "metadata offset beyond EOF");
            let end = end.min(descriptor.bytes);
            if !BINARY_NAMES.contains(&object_name) {
                let bytes = self.immutable(
                    &key,
                    &descriptor.sha256,
                    Some(descriptor.bytes),
                    MAX_CONTROL,
                )?;
                bytes[offset as usize..end as usize].to_vec()
            } else if offset == end {
                Vec::new()
            } else {
                self.storage.range(&key, offset..end)?
            }
        };
        Ok((
            Response::Data {
                length: body.len() as u32,
                sha256: hash(&body),
            },
            body,
        ))
    }

    fn prepare(
        &self,
        image: &str,
        architecture: &str,
        refresh: bool,
    ) -> anyhow::Result<(Response, Vec<u8>)> {
        let platform = platform(architecture)?;
        ensure!(
            !refresh || !self.read_only,
            "read-only cache cannot refresh; refresh with a publisher"
        );
        let (canonical, pinned) = crate::image::oci::cache_reference(image)?;
        let observed = self.observe(&canonical, platform)?;
        if !refresh && let Some(object) = &observed {
            let head = decode_head(&object.bytes, &image_key(&canonical), platform)?;
            if self.read_only || pinned || now().saturating_sub(head.published_at) < 300 {
                let handle = Handle {
                    image_key: head.image_key,
                    platform: head.platform,
                    revision: head.revision[7..].into(),
                };
                let loaded = self.load(&handle)?;
                ensure!(
                    loaded.commit.manifest_digest == head.manifest_digest,
                    "HEAD manifest mismatch"
                );
                return Ok((loaded.prepared(), Vec::new()));
            }
        }
        ensure!(
            !self.read_only,
            "image absent from read-only cache; use a publisher"
        );
        let store = crate::image::oci::ImageStore::new(self.local_store.clone())?;
        let prepared =
            store.prepare_with_refresh(image, architecture, refresh || observed.is_some())?;
        self.publish_observed(&store, &prepared, architecture, &canonical, observed)
    }
    fn observe(&self, canonical: &str, platform: &str) -> anyhow::Result<Option<StoredObject>> {
        let object = self.storage.get_versioned(&head_key(canonical, platform))?;
        if let Some(object) = &object {
            decode_head(&object.bytes, &image_key(canonical), platform)?;
            ensure!(
                object.version.e_tag.is_some(),
                "HEAD backend lacks a CAS token"
            );
        }
        Ok(object)
    }
    fn load(&self, handle: &Handle) -> anyhow::Result<Arc<LoadedImage>> {
        let key = handle.encode();
        if let Some(image) = self.images.lock().unwrap().get(&key).cloned() {
            return Ok(image);
        }
        self.immutable(
            "format.json",
            &hash(FORMAT),
            Some(FORMAT.len() as u64),
            MAX_CONTROL,
        )?;
        self.load_revision(handle)
    }

    // Socket readers have no access to global format.json. The pinned COMMIT,
    // root descriptor and frame references authenticate the supported schema.
    fn load_revision(&self, handle: &Handle) -> anyhow::Result<Arc<LoadedImage>> {
        let key = handle.encode();
        if let Some(image) = self.images.lock().unwrap().get(&key).cloned() {
            return Ok(image);
        }
        let prefix = handle.prefix();
        let bytes = self.immutable(
            &format!("{prefix}/COMMIT.json"),
            &format!("sha256:{}", handle.revision),
            None,
            MAX_CONTROL,
        )?;
        let commit: Commit = serde_json::from_slice(&bytes)?;
        ensure!(
            commit.format_version == 2
                && commit.image_key == handle.image_key
                && commit.platform == handle.platform,
            "COMMIT identity mismatch"
        );
        crate::image::oci::digest_hex(&commit.manifest_digest)?;
        let expected = [
            "manifest.json",
            "config.json",
            "root.bin",
            "index.bin",
            "objects.bin",
        ];
        ensure!(
            commit.metadata.len() == expected.len()
                && expected
                    .iter()
                    .all(|name| commit.metadata.contains_key(*name)),
            "incompatible metadata inventory"
        );
        for (name, desc) in &commit.metadata {
            crate::image::oci::digest_hex(&desc.sha256)?;
            let limit = if name.ends_with(".json") || name == "root.bin" {
                MAX_CONTROL
            } else {
                MAX_OBJECT
            };
            ensure!(
                desc.bytes > 0 && desc.bytes <= limit as u64,
                "metadata exceeds size limit"
            );
        }
        let get = |name: &str| -> anyhow::Result<Vec<u8>> {
            let d = &commit.metadata[name];
            self.immutable(
                &format!("{prefix}/{name}"),
                &d.sha256,
                Some(d.bytes),
                MAX_CONTROL,
            )
        };
        // Runner lower attachment loads metadata before CLONE_NEWUSER, which
        // requires kernel-level single-threadedness even after userspace joins.
        let manifest_bytes = get("manifest.json")?;
        let config_bytes = get("config.json")?;
        let root = get("root.bin")?;
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
        ensure!(
            manifest.format_version == 2
                && image_key(&manifest.reference) == handle.image_key
                && manifest.platform == handle.platform
                && manifest.manifest_digest == commit.manifest_digest,
            "manifest provenance mismatch"
        );
        let config: Configuration = serde_json::from_slice(&config_bytes)?;
        ensure!(
            config.format_version == 2
                && config.architecture == platform_architecture(&handle.platform)?,
            "configuration platform mismatch"
        );
        let image = Arc::new(LoadedImage::new(
            self.storage.clone(),
            self.local_objects.clone(),
            handle.clone(),
            commit,
            config,
            &root,
        )?);
        self.images.lock().unwrap().put(key, image.clone());
        Ok(image)
    }
    fn immutable(
        &self,
        key: &str,
        digest: &str,
        length: Option<u64>,
        limit: usize,
    ) -> anyhow::Result<Vec<u8>> {
        let hex = crate::image::oci::digest_hex(digest)?;
        let local = self
            .local_objects
            .as_ref()
            .map(|root| root.join("immutable").join(hex));
        let valid = |bytes: &[u8]| {
            bytes.len() <= limit
                && length.is_none_or(|n| n == bytes.len() as u64)
                && hash(bytes) == digest
        };
        if let Some(path) = &local
            && let Some(bytes) = read_local(path, limit)?
            && valid(&bytes)
        {
            return Ok(bytes);
        }
        let bytes = self
            .storage
            .get(key)?
            .context("missing immutable cache object")?;
        ensure!(
            valid(&bytes),
            "cache object digest mismatch or invalid length"
        );
        if let Some(path) = local {
            cache_local(&path, &bytes);
        }
        Ok(bytes)
    }
}
fn read_local(path: &std::path::Path, limit: usize) -> anyhow::Result<Option<Vec<u8>>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    else {
        return Ok(None);
    };
    if !file
        .metadata()
        .is_ok_and(|m| m.is_file() && m.len() <= limit as u64)
    {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    if file
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > limit
    {
        return Ok(None);
    }
    Ok(Some(bytes))
}
fn cache_local(path: &std::path::Path, bytes: &[u8]) {
    let result = (|| -> anyhow::Result<()> {
        std::fs::create_dir_all(path.parent().context("cache object parent")?)?;
        Persistence::atomic_write(path, bytes, 0o600)?;
        Ok(())
    })();
    if let Err(e) = result {
        crate::diagnostics::diagnostic(format_args!("local cache object write: {e:#}"));
    }
}
fn platform(architecture: &str) -> anyhow::Result<&'static str> {
    match architecture {
        "amd64" => Ok("linux-amd64"),
        "arm64" => Ok("linux-arm64-v8"),
        _ => bail!("unsupported image architecture"),
    }
}
fn platform_architecture(platform: &str) -> anyhow::Result<&'static str> {
    match platform {
        "linux-amd64" => Ok("amd64"),
        "linux-arm64-v8" => Ok("arm64"),
        _ => bail!("unsupported image platform"),
    }
}
fn image_key(canonical: &str) -> String {
    hash(canonical.as_bytes())[7..].into()
}
fn head_key(canonical: &str, platform: &str) -> String {
    format!(
        "meta/{}/platforms/{platform}/HEAD.json",
        image_key(canonical)
    )
}
fn data_key(digest: &str) -> anyhow::Result<String> {
    let h = crate::image::oci::digest_hex(digest)?;
    Ok(format!("data/sha256/{}/{}/{h}", &h[..2], &h[2..4]))
}
fn check_hex(hex: &str) -> anyhow::Result<()> {
    ensure!(
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid SHA-256 hex"
    );
    Ok(())
}
fn decode_head(bytes: &[u8], key: &str, platform: &str) -> anyhow::Result<Head> {
    ensure!(bytes.len() <= MAX_CONTROL, "HEAD exceeds size limit");
    let head: Head = serde_json::from_slice(bytes)?;
    ensure!(
        head.format_version == 2
            && head.image_key == key
            && head.platform == platform
            && head.generation > 0,
        "HEAD identity mismatch"
    );
    uuid::Uuid::parse_str(&head.publication_id).context("invalid HEAD publication ID")?;
    crate::image::oci::digest_hex(&head.revision)?;
    crate::image::oci::digest_hex(&head.manifest_digest)?;
    Ok(head)
}
fn validate_path(path: &[u8]) -> anyhow::Result<()> {
    ensure!(
        path.len() <= 16384
            && !path.contains(&0)
            && (path.is_empty()
                || path
                    .split(|c| *c == b'/')
                    .all(|p| !p.is_empty() && p != b"." && p != b".." && p.len() <= 255)),
        "cache path must be relative without dot or parent components"
    );
    Ok(())
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
