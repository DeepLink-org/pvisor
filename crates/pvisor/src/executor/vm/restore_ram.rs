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
    pub base: Option<Arc<crate::environment_snapshot::PinnedRamBlocks>>,
    // Drop the file before unmounting. PreparedRestore and its native mappings
    // must keep this entire owner alive until the runner is reaped.
    _mount: SnapshotRamMount,
}

pub(super) fn acquire(
    store: &Path,
    snapshot_id: &str,
    published: &PublishedEnvironment,
) -> anyhow::Result<Arc<SharedRam>> {
    static MOUNTS: OnceLock<Mounts<(PathBuf, String), SharedRam>> = OnceLock::new();
    // The caller opens/validates the published reference on every restore,
    // before cache lookup. A live mount cannot authorize a deleted snapshot.
    let store = store.canonicalize()?;
    MOUNTS
        .get_or_init(|| Mounts::new(4096))
        .get((store.clone(), snapshot_id.to_owned()), || {
            // Independent of any one Attempt's directory/lifetime.
            let directory = store.join("ram-mounts");
            crate::util::create_dir_all_durable(&directory)?;
            let reader = published.ram_reader()?;
            let base = reader.compressed_base();
            let (mut mount, file) = SnapshotRamMount::new(reader, &directory)?;
            mount.watch_native_owner_exit(&std::env::current_exe()?)?;
            Ok(SharedRam {
                file: Arc::new(file),
                path: mount.ram_path(),
                base,
                _mount: mount,
            })
        })
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

    struct Owner(Arc<AtomicUsize>);
    impl Drop for Owner {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn concurrent_same_snapshot_shares_one_owner_and_last_attempt_releases_it() {
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
