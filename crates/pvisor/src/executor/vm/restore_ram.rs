//! One authenticated read-only RAM inode per live snapshot on this supervisor.
//! Guest mappings remain MAP_PRIVATE; only unchanged baseline pages are shared.
use crate::environment_snapshot::{PublishedEnvironment, SnapshotRamMount};
use anyhow::ensure;
use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

pub(super) struct SharedRam {
    pub file: Arc<File>,
    pub path: PathBuf,
    pub extents: Vec<pvisor_vm::api::RamRestoreExtent>,
    pub _base: Option<Arc<crate::environment_snapshot::PinnedRamBlocks>>,
    // Drop the file before unmounting. PreparedRestore and its native mappings
    // must keep this entire owner alive until the runner is reaped.
    _mount: Option<SnapshotRamMount>,
    _node: Option<crate::node::Pin>,
    #[cfg(target_os = "linux")]
    preparation_cache: Option<Arc<PreparationCache>>,
}

pub(super) fn acquire(
    store: &Path,
    snapshot_id: &str,
    published: &PublishedEnvironment,
    node_socket: Option<&Path>,
    filesystem_pool: Option<&Path>,
) -> anyhow::Result<Arc<SharedRam>> {
    if let Some(socket) = node_socket {
        // The caller already authenticated this publication; the service also
        // checks it before looking up its cross-supervisor owner.
        let pin = crate::node::Pin::ram(
            socket,
            store,
            snapshot_id,
            published.manifest().compatibility.clone(),
            filesystem_pool.map(Path::to_path_buf),
        )?;
        let file = std::fs::OpenOptions::new().read(true).open(pin.path())?;
        return Ok(Arc::new(SharedRam {
            path: pin.path().into(),
            extents: Vec::new(),
            file: Arc::new(file),
            _base: published.ram_reader()?.compressed_base(),
            _mount: None,
            _node: Some(pin),
            #[cfg(target_os = "linux")]
            preparation_cache: None,
        }));
    }
    static MOUNTS: OnceLock<Mounts<(PathBuf, String), SharedRam>> = OnceLock::new();
    // The caller opens/validates the published reference on every restore,
    // before cache lookup. A live mount cannot authorize a deleted snapshot.
    let store = store.canonicalize()?;
    MOUNTS
        .get_or_init(|| Mounts::new(4096))
        .get((store.clone(), snapshot_id.to_owned()), || {
            // Independent of any one Execution's directory/lifetime, and never
            // nested in the durable store (which may be an OverlayFS lower).
            let mut reader = published.ram_reader()?;
            let extents = reader.compact_zero_restore()?;
            let base = reader.compressed_base();
            #[cfg(target_os = "linux")]
            let preparation_cache = match reader.raw_preparation_cache() {
                Ok(file) => file.map(|file| Arc::new(PreparationCache::new(file))),
                Err(error) => {
                    tracing::warn!(%error, "skip raw RAM preparation cache advice");
                    None
                }
            };
            let (mut mount, file) = SnapshotRamMount::runtime(reader, &[&store])?;
            mount.watch_native_owner_exit(&std::env::current_exe()?)?;
            Ok(SharedRam {
                file: Arc::new(file),
                path: mount.ram_path(),
                extents,
                _base: base,
                _mount: Some(mount),
                _node: None,
                #[cfg(target_os = "linux")]
                preparation_cache,
            })
        })
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct ReadyState {
    last: Option<std::time::Instant>,
    worker: bool,
    done: bool,
}

#[cfg(target_os = "linux")]
impl ReadyState {
    fn notify(&mut self, now: std::time::Instant) -> bool {
        if self.done {
            return false;
        }
        self.last = Some(now);
        if self.worker {
            return false;
        }
        self.worker = true;
        true
    }
    fn claim(&mut self, now: std::time::Instant) -> bool {
        if self.done
            || self
                .last
                .is_none_or(|last| now.duration_since(last) < std::time::Duration::from_secs(30))
        {
            return false;
        }
        self.done = true;
        true
    }
}

#[cfg(target_os = "linux")]
struct PreparationCache {
    file: File,
    state: Mutex<ReadyState>,
}

#[cfg(target_os = "linux")]
impl PreparationCache {
    fn new(file: File) -> Self {
        Self {
            file,
            state: Mutex::new(ReadyState::default()),
        }
    }

    fn ready(self: &Arc<Self>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.notify(std::time::Instant::now()) {
            return;
        }
        let weak = Arc::downgrade(self);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            state.done = true;
            return;
        };
        runtime.spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                let Some(cache) = weak.upgrade() else {
                    return;
                };
                let claimed = {
                    let mut state = cache.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.claim(std::time::Instant::now())
                };
                if !claimed {
                    continue;
                }
                // Timer retains only a weak owner; blocking advice pins only
                // this exact source FD, not the VM or its FUSE mount.
                let _ = tokio::task::spawn_blocking(move || {
                    use std::os::fd::AsRawFd;
                    let error = unsafe {
                        libc::posix_fadvise(cache.file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED)
                    };
                    if error != 0 {
                        tracing::warn!(error, "raw RAM ready cache advice failed");
                    } else {
                        tracing::debug!(
                            "raw RAM ready cache advice requested (not measured eviction)"
                        );
                    }
                })
                .await;
                return;
            }
        });
    }
}

#[cfg(target_os = "linux")]
impl SharedRam {
    pub(super) fn has_preparation_cache(&self) -> bool {
        self.preparation_cache.is_some()
    }
    pub(super) fn restored_ready(&self) {
        if let Some(cache) = &self.preparation_cache {
            cache.ready();
        }
    }
}

type Slot<T> = Arc<Mutex<Weak<T>>>;

/// The map holds weak ownership only. Per-key serialization deduplicates
/// concurrent preparations without blocking creation of unrelated snapshots.
struct Mounts<K, T> {
    slots: Mutex<BTreeMap<K, Slot<T>>>,
    limit: usize,
}
impl<K: Ord, T> Mounts<K, T> {
    fn new(limit: usize) -> Self {
        Self {
            slots: Mutex::new(BTreeMap::new()),
            limit,
        }
    }

    fn get(&self, key: K, load: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<Arc<T>> {
        let slot = {
            let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            slots.retain(|_, slot| {
                Arc::strong_count(slot) > 1
                    || match slot.try_lock() {
                        Ok(value) => value.strong_count() > 0,
                        Err(std::sync::TryLockError::Poisoned(e)) => {
                            e.into_inner().strong_count() > 0
                        }
                        Err(std::sync::TryLockError::WouldBlock) => true,
                    }
            });
            if let Some(slot) = slots.get(&key) {
                slot.clone()
            } else {
                ensure!(
                    slots.len() < self.limit,
                    "live snapshot RAM mount limit reached"
                );
                let slot = Arc::new(Mutex::new(Weak::new()));
                slots.insert(key, slot.clone());
                slot
            }
        };
        // A panicking preparation never publishes a partial owner. Recovering
        // this cache lock allows a later preparation to retry from scratch.
        let mut cached = slot.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(value) = cached.upgrade() {
            return Ok(value);
        }
        let value = Arc::new(load()?);
        *cached = Arc::downgrade(&value);
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Barrier,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    #[cfg(target_os = "linux")]
    #[test]
    fn ready_advice_waits_for_quiet_and_is_once_per_owner() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let mut s = ReadyState::default();
        assert!(!s.claim(now));
        assert!(s.notify(now));
        assert!(!s.notify(now + Duration::from_secs(20)));
        assert!(!s.claim(now + Duration::from_secs(30)));
        assert!(s.claim(now + Duration::from_secs(50)));
        assert!(!s.claim(now + Duration::from_secs(100)));
        assert!(!s.notify(now + Duration::from_secs(100)));
        assert!(ReadyState::default().notify(now));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn source_advice_preserves_private_cow_and_pins_original_inode() {
        use std::os::{fd::AsRawFd, unix::fs::FileExt};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ram");
        std::fs::write(&path, vec![7u8; 4096]).unwrap();
        let cache = PreparationCache::new(File::open(&path).unwrap());
        let fd = cache.file.as_raw_fd();
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                4096,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE,
                fd,
                0,
            )
        };
        assert_ne!(ptr, libc::MAP_FAILED);
        unsafe {
            *(ptr as *mut u8) = 42;
        }
        std::fs::rename(&path, dir.path().join("old")).unwrap();
        std::fs::write(&path, vec![9u8; 4096]).unwrap();
        assert_eq!(
            unsafe { libc::posix_fadvise(fd, 0, 0, libc::POSIX_FADV_DONTNEED) },
            0
        );
        let mut bytes = [0u8; 1];
        cache.file.read_exact_at(&mut bytes, 0).unwrap();
        assert_eq!(bytes, [7]);
        assert_eq!(std::fs::read(&path).unwrap()[0], 9);
        assert_eq!(unsafe { *(ptr as *const u8) }, 42);
        unsafe {
            libc::munmap(ptr, 4096);
        }
    }

    struct Owner(Arc<AtomicUsize>);
    impl Drop for Owner {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn concurrent_same_snapshot_shares_one_owner_and_last_execution_releases_it() {
        let pool = Mounts::new(1);
        let loads = AtomicUsize::new(0);
        let drops = Arc::new(AtomicUsize::new(0));
        let start = Barrier::new(8);
        let owners = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        pool.get(("store", "snapshot"), || {
                            loads.fetch_add(1, Ordering::SeqCst);
                            Ok(Owner(drops.clone()))
                        })
                        .unwrap()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        assert!(owners.iter().all(|owner| Arc::ptr_eq(owner, &owners[0])));
        assert!(
            pool.get(("store", "other"), || Ok(Owner(drops.clone())))
                .is_err()
        );
        let last = owners[0].clone();
        drop(owners);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(last);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        drop(
            pool.get(("store", "other"), || Ok(Owner(drops.clone())))
                .unwrap(),
        );
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failed_and_panicking_preparation_can_retry_without_pinning_capacity() {
        let pool = Mounts::new(1);
        assert!(
            pool.get("snapshot", || anyhow::bail!("bad backing"))
                .is_err()
        );
        assert_eq!(*pool.get("snapshot", || Ok(7)).unwrap(), 7);
        assert!(
            std::panic::catch_unwind(|| pool.get("snapshot", || -> anyhow::Result<u32> {
                panic!("aborted preparation")
            }))
            .is_err()
        );
        assert_eq!(*pool.get("snapshot", || Ok(9)).unwrap(), 9);
        assert_eq!(*pool.get("other", || Ok(8)).unwrap(), 8);
    }

    #[test]
    fn stores_and_snapshots_are_isolated_and_unrelated_preparation_does_not_block() {
        let pool = Mounts::new(3);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let preparing_pool = &pool;
            let pending = scope.spawn(move || {
                preparing_pool
                    .get(("a", "snapshot"), || {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(1)
                    })
                    .unwrap()
            });
            entered_rx.recv().unwrap();
            let second = pool.get(("b", "snapshot"), || Ok(2)).unwrap();
            let third = pool.get(("a", "other"), || Ok(3)).unwrap();
            assert!(pool.get(("c", "snapshot"), || Ok(4)).is_err());
            release_tx.send(()).unwrap();
            let first = pending.join().unwrap();
            assert!(!Arc::ptr_eq(&first, &second));
            assert!(!Arc::ptr_eq(&first, &third));
        });
    }
}
