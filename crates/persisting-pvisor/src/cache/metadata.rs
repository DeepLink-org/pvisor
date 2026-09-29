//! Metadata for immutable, fully prepared image roots.
use super::*;
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::sync::OnceLock;

static STATS: OnceLock<Mutex<HashMap<String, Response>>> = OnceLock::new();
type Names = Arc<Vec<Vec<u8>>>;
static DIRECTORIES: OnceLock<Mutex<HashMap<String, Names>>> = OnceLock::new();

pub(super) fn generation(store: &ImageStore, digest: &str) -> anyhow::Result<String> {
    let root = store
        .root
        .join("rootfs-v3/sha256")
        .join(crate::oci::digest_hex(digest)?);
    let m = fs::symlink_metadata(&root)?;
    ensure!(m.is_dir(), "image root is not a directory");
    // Include store identity and host inode generation: extracted inode numbers
    // can change when the same OCI digest is removed and prepared again.
    Ok(hash(&serde_json::to_vec(&(
        root.as_os_str().as_bytes(),
        m.dev(),
        m.ino(),
        m.ctime(),
        m.ctime_nsec(),
    ))?))
}

fn key(store: &ImageStore, digest: &str, path: &[u8]) -> anyhow::Result<String> {
    Ok(hash(&serde_json::to_vec(&(
        generation(store, digest)?,
        digest,
        path,
    ))?))
}

fn cached<T: Clone>(
    cache: &OnceLock<Mutex<HashMap<String, T>>>,
    key: String,
    capacity: usize,
    load: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let mut cache = cache.get_or_init(Default::default).lock().unwrap();
    if let Some(value) = cache.get(&key) {
        return Ok(value.clone());
    }
    // ponytail: serialize metadata misses and clear at the entry ceiling;
    // use per-key loading and an LRU only if cache contention/churn matters.
    let value = load()?;
    if cache.len() >= capacity {
        cache.clear();
    }
    cache.insert(key, value.clone());
    Ok(value)
}

pub(super) fn stat(store: &ImageStore, digest: &str, path: &[u8]) -> anyhow::Result<Response> {
    cached(&STATS, key(store, digest, path)?, 4096, || {
        let (directory, name) = parent(store, digest, path)?;
        metadata_at(&directory, &name)
    })
}

pub(super) fn directory(store: &ImageStore, digest: &str, path: &[u8]) -> anyhow::Result<Names> {
    cached(&DIRECTORIES, key(store, digest, path)?, 128, || {
        let (directory, name) = parent(store, digest, path)?;
        let directory = open_child(&directory, OsStr::from_bytes(&name), true)?;
        let mut names = directory_names(directory)?;
        names.sort();
        Ok(Arc::new(names))
    })
}
