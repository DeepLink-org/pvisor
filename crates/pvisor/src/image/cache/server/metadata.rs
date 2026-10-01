//! Metadata for immutable, fully prepared image roots.
use super::*;
use lru::LruCache;
use std::num::NonZeroUsize;
use std::os::unix::fs::MetadataExt;
use std::sync::OnceLock;

static STATS: OnceLock<Mutex<LruCache<String, Response>>> = OnceLock::new();
type Names = Arc<Vec<Vec<u8>>>;
static DIRECTORIES: OnceLock<Mutex<LruCache<String, Names>>> = OnceLock::new();

pub(super) fn generation(store: &ImageStore, digest: &str) -> anyhow::Result<String> {
    let root = store
        .root
        .join("rootfs-v3/sha256")
        .join(crate::image::oci::digest_hex(digest)?);
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
    cache: &OnceLock<Mutex<LruCache<String, T>>>,
    key: String,
    capacity: usize,
    load: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let cache =
        cache.get_or_init(|| Mutex::new(LruCache::new(NonZeroUsize::new(capacity).unwrap())));
    if let Some(value) = cache.lock().unwrap().get(&key).cloned() {
        return Ok(value);
    }
    // Duplicate concurrent misses are harmless for immutable metadata. Keep
    // filesystem I/O outside the lock so a slow scan cannot block cache hits.
    let value = load()?;
    cache.lock().unwrap().put(key, value.clone());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_evicts_only_old_entries_and_loads_without_holding_lock() {
        let cache = OnceLock::new();
        cached(&cache, "a".into(), 2, || Ok(1)).unwrap();
        cached(&cache, "b".into(), 2, || Ok(2)).unwrap();
        assert_eq!(
            cached(&cache, "a".into(), 2, || panic!("cache miss")).unwrap(),
            1
        );
        cached(&cache, "c".into(), 2, || Ok(3)).unwrap();
        assert!(cache.get().unwrap().lock().unwrap().contains("a"));
        assert!(!cache.get().unwrap().lock().unwrap().contains("b"));
        cached(&cache, "d".into(), 2, || {
            assert!(
                cache.get().unwrap().try_lock().is_ok(),
                "miss held the cache lock"
            );
            Ok(4)
        })
        .unwrap();
    }
}
