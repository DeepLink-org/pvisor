//! Shared read-only revision mounts; each native attempt has its own upper.
use anyhow::{Context, ensure};
use pvisor::cache::{CacheBackend, CacheConfig, MountedImage, mount_image_handle};
use pvisor_cluster::EnvironmentRecord;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Weak},
};
use tokio::sync::Mutex;

type Slot<T> = Arc<Mutex<Weak<T>>>;
pub struct EnvironmentMounts {
    config: CacheConfig,
    mounts: SharedMounts<MountedImage>,
}
impl EnvironmentMounts {
    pub fn new(storage: &Path, limit: usize) -> anyhow::Result<Self> {
        ensure!(
            (1..=4096).contains(&limit),
            "environment mount limit must be 1..4096"
        );
        let mut config = CacheConfig::from_env()?;
        ensure!(
            matches!(config.backend, CacheBackend::Filesystem | CacheBackend::S3),
            "immutable environments require the filesystem or S3 native cache backend"
        );
        config.read_only = true;
        config.image_store = Some(storage.join("environment-mounts"));
        Ok(Self {
            config,
            mounts: SharedMounts::new(limit),
        })
    }
    async fn layer(&self, handle: &str) -> anyhow::Result<Arc<MountedImage>> {
        self.mounts
            .get(handle, || async {
                let config = self.config.clone();
                let handle = handle.to_owned();
                tokio::task::spawn_blocking(move || mount_image_handle(config, &handle))
                    .await
                    .context("native cache mount task failed")?
            })
            .await
    }
    pub async fn prepare(
        &self,
        record: &EnvironmentRecord,
    ) -> anyhow::Result<Vec<Arc<MountedImage>>> {
        pvisor_cluster::environment::validate(record)?;
        let mut mounts = Vec::new();
        for layer in record.template.layers() {
            let mounted = self.layer(&layer.handle).await?;
            ensure!(
                mounted.manifest_digest() == layer.manifest_digest,
                "native cache revision manifest does not match environment template"
            );
            mounts.push(mounted);
        }
        Ok(mounts)
    }
}

/// Bound live mounts/preparations, share per key, and release idle mounts rather
/// than retaining their FUSE threads and hot caches after the final attempt.
struct SharedMounts<T> {
    slots: Mutex<BTreeMap<String, Slot<T>>>,
    limit: usize,
}
impl<T> SharedMounts<T> {
    fn new(limit: usize) -> Self {
        Self {
            slots: Mutex::new(BTreeMap::new()),
            limit,
        }
    }
    async fn get<F: std::future::Future<Output = anyhow::Result<T>>>(
        &self,
        key: &str,
        load: impl FnOnce() -> F,
    ) -> anyhow::Result<Arc<T>> {
        let slot = {
            let mut slots = self.slots.lock().await;
            // Evict only idle bookkeeping. Active preparation or a live mount
            // keeps its slot, so concurrent attempts share one revision mount.
            slots.retain(|_, slot| {
                Arc::strong_count(slot) > 1
                    || match slot.try_lock() {
                        Ok(mount) => mount.strong_count() > 0,
                        Err(_) => true,
                    }
            });
            if let Some(slot) = slots.get(key) {
                slot.clone()
            } else {
                ensure!(
                    slots.len() < self.limit,
                    "node immutable environment mount limit reached"
                );
                let slot = Arc::new(Mutex::new(Weak::new()));
                slots.insert(key.to_owned(), slot.clone());
                slot
            }
        };
        let mut cached = slot.lock().await;
        if let Some(mount) = cached.upgrade() {
            return Ok(mount);
        }
        let mounted = Arc::new(load().await?);
        *cached = Arc::downgrade(&mounted);
        Ok(mounted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn concurrent_attempts_share_one_mount_and_last_owner_releases_it() {
        struct Mount(Arc<AtomicUsize>);
        impl Drop for Mount {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let pool = SharedMounts::new(1);
        let loads = AtomicUsize::new(0);
        let drops = Arc::new(AtomicUsize::new(0));
        let (a, b) = tokio::join!(
            pool.get("revision", || async {
                loads.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                Ok(Mount(drops.clone()))
            }),
            pool.get("revision", || async { panic!("duplicate mount creation") }),
        );
        let a = a.unwrap();
        let b = b.unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        assert!(
            pool.get("other", || async { Ok(Mount(drops.clone())) })
                .await
                .is_err()
        );
        drop(a);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(b);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        let next = pool
            .get("other", || async { Ok(Mount(drops.clone())) })
            .await
            .unwrap();
        drop(next);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn failed_or_cancelled_preparation_does_not_poison_or_pin_a_slot() {
        let pool = Arc::new(SharedMounts::<u32>::new(1));
        assert!(
            pool.get("failed", || async { anyhow::bail!("cache unavailable") })
                .await
                .is_err()
        );
        let retried = pool.get("failed", || async { Ok(1) }).await.unwrap();
        drop(retried);
        let entered = Arc::new(tokio::sync::Notify::new());
        let pending = tokio::spawn({
            let pool = pool.clone();
            let entered = entered.clone();
            async move {
                pool.get("pending", || async {
                    entered.notify_one();
                    std::future::pending::<anyhow::Result<u32>>().await
                })
                .await
            }
        });
        entered.notified().await;
        pending.abort();
        assert!(pending.await.unwrap_err().is_cancelled());
        assert_eq!(*pool.get("next", || async { Ok(2) }).await.unwrap(), 2);
    }
}
