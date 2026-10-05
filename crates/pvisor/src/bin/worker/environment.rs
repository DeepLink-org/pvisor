//! Shared read-only revision mounts; each native attempt has its own upper.
use anyhow::{Context, ensure};
use pvisor::cache::{CacheBackend, CacheConfig, LazyImage, open_image_handle_for_vm};
use pvisor_cluster::EnvironmentRecord;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Weak},
};
use tokio::sync::Mutex;

type Slot<T> = Arc<Mutex<Weak<T>>>;

/// The final FUSE owner can block while unmounting and joining its request
/// thread. Keep this work off the lease/poll executor, including error and
/// cancelled-preparation paths.
pub struct MountOwners<T: Send + Sync + 'static> {
    mounts: Vec<Arc<T>>,
}
impl<T: Send + Sync + 'static> MountOwners<T> {
    pub fn new() -> Self {
        Self { mounts: Vec::new() }
    }
    pub fn mounts(&self) -> &[Arc<T>] {
        &self.mounts
    }
    pub async fn release(mut self) -> anyhow::Result<()> {
        let mounts = std::mem::take(&mut self.mounts);
        if !mounts.is_empty() {
            tokio::task::spawn_blocking(move || drop(mounts))
                .await
                .context("native environment release task failed")?;
        }
        Ok(())
    }
}
impl<T: Send + Sync + 'static> Drop for MountOwners<T> {
    fn drop(&mut self) {
        let mounts = std::mem::take(&mut self.mounts);
        if mounts.is_empty() {
            return;
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(move || drop(mounts));
        } else {
            drop(mounts);
        }
    }
}

pub struct EnvironmentMounts {
    config: Option<CacheConfig>,
    node_socket: Option<std::path::PathBuf>,
    mounts: SharedMounts<LazyImage>,
}
pub enum ImageOwner {
    Local(Arc<LazyImage>),
    Node(pvisor::node::Pin),
}
impl ImageOwner {
    pub fn rootfs(&self) -> &Path {
        match self {
            Self::Local(image) => image.rootfs(),
            Self::Node(pin) => pin.rootfs(),
        }
    }
    pub fn manifest_digest(&self) -> &str {
        match self {
            Self::Local(image) => image.manifest_digest(),
            Self::Node(pin) => pin.manifest_digest(),
        }
    }
}
impl EnvironmentMounts {
    pub fn new(storage: &Path, limit: usize, node_socket: Option<&Path>) -> anyhow::Result<Self> {
        ensure!(
            (1..=4096).contains(&limit),
            "environment mount limit must be 1..4096"
        );
        let config = if node_socket.is_some() {
            None
        } else {
            let mut config = CacheConfig::from_env()?;
            ensure!(
                matches!(config.backend, CacheBackend::Filesystem | CacheBackend::S3),
                "immutable environments require the filesystem or S3 native cache backend"
            );
            config.read_only = true;
            config.image_store = Some(storage.join("environment-mounts"));
            Some(config)
        };
        Ok(Self {
            config,
            node_socket: node_socket.map(Path::to_path_buf),
            mounts: SharedMounts::new(limit),
        })
    }
    async fn layer(&self, handle: &str) -> anyhow::Result<Arc<LazyImage>> {
        self.mounts
            .get(handle, || async {
                let config = self
                    .config
                    .clone()
                    .context("local environment cache is disabled")?;
                let handle = handle.to_owned();
                tokio::task::spawn_blocking(move || open_image_handle_for_vm(config, &handle))
                    .await
                    .context("native cache mount task failed")?
            })
            .await
    }
    pub async fn prepare(
        &self,
        record: &EnvironmentRecord,
    ) -> anyhow::Result<MountOwners<ImageOwner>> {
        pvisor_cluster::environment::validate(record)?;
        let mut mounts = MountOwners::new();
        for layer in record.template.layers() {
            let owner = if let Some(socket) = &self.node_socket {
                let (socket, handle, digest) = (
                    socket.clone(),
                    layer.handle.clone(),
                    layer.manifest_digest.clone(),
                );
                ImageOwner::Node(
                    tokio::task::spawn_blocking(move || {
                        pvisor::node::Pin::image(&socket, &handle, &digest)
                    })
                    .await??,
                )
            } else {
                ImageOwner::Local(self.layer(&layer.handle).await?)
            };
            mounts.mounts.push(Arc::new(owner));
            ensure!(
                mounts.mounts.last().unwrap().manifest_digest() == layer.manifest_digest,
                "native cache revision manifest does not match environment template"
            );
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

    struct BlockingMount {
        async_thread: std::thread::ThreadId,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl Drop for BlockingMount {
        fn drop(&mut self) {
            assert_ne!(std::thread::current().id(), self.async_thread);
            self.entered.send(()).unwrap();
            self.release
                .get_mut()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        }
    }
    struct ReleaseOnDrop(std::sync::mpsc::Sender<()>);
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn last_mount_release_keeps_the_poll_thread_live_and_waits_for_unmount() {
        let (entered, observe) = std::sync::mpsc::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let release = ReleaseOnDrop(release);
        let mut mounts = MountOwners::new();
        mounts.mounts.push(Arc::new(BlockingMount {
            async_thread: std::thread::current().id(),
            entered,
            release: std::sync::Mutex::new(gate),
        }));
        let completion = tokio::spawn(mounts.release());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if observe.try_recv().is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // This timer represents the single-thread worker's lease watchdog.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!completion.is_finished());
        drop(release);
        completion.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_preparation_releases_mounts_without_blocking_the_poll_thread() {
        let (entered, observe) = std::sync::mpsc::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let release = ReleaseOnDrop(release);
        let mut mounts = MountOwners::new();
        mounts.mounts.push(Arc::new(BlockingMount {
            async_thread: std::thread::current().id(),
            entered,
            release: std::sync::Mutex::new(gate),
        }));
        let (prepared, waiting) = tokio::sync::oneshot::channel();
        let preparation = tokio::spawn(async move {
            let _owners = mounts;
            prepared.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        waiting.await.unwrap();
        preparation.abort();
        assert!(preparation.await.unwrap_err().is_cancelled());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while observe.try_recv().is_err() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(release);
    }

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
