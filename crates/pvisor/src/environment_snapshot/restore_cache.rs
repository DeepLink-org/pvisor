//! Best-effort cleanup of source and freshly published pool preparation caches.
//! Never changes data, snapshot authority, or the running VM's mappings.
use super::{PublishedEnvironment, SharedFilesystemLayer};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CString,
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::{Duration, Instant},
};

const QUIET: Duration = Duration::from_secs(30);
const MAX_KEYS: usize = 8;
pub(super) const MAX_FILES: usize = 131_072;
pub(super) const MAX_PATH_BYTES: usize = 16 * 1024 * 1024;
type Key = (PathBuf, String);

struct Slot<T> {
    active: usize,
    deadline: Instant,
    job: Option<T>,
    releasing: bool,
}
fn ready_key<T>(slots: &BTreeMap<Key, Slot<T>>, now: Instant) -> Option<Key> {
    slots
        .iter()
        .find(|(_, s)| s.active == 0 && !s.releasing && s.job.is_some() && s.deadline <= now)
        .map(|(k, _)| k.clone())
}
struct Coordinator {
    slots: Mutex<BTreeMap<Key, Slot<Job>>>,
    wake: Condvar,
}

/// A bounded, process-local preparation guard. Missing capacity disables advice.
/// Acquisition precedes payload validation; Drop also handles failed prepares.
pub(crate) struct RestoreCacheWindow {
    coordinator: Arc<Coordinator>,
    key: Key,
}
impl RestoreCacheWindow {
    pub(crate) fn begin(store: &Path, id: &str) -> Option<Self> {
        static COORDINATOR: OnceLock<Option<Arc<Coordinator>>> = OnceLock::new();
        let key = (store.canonicalize().ok()?, id.to_owned());
        let coordinator = COORDINATOR
            .get_or_init(|| {
                let c = Arc::new(Coordinator {
                    slots: Mutex::new(BTreeMap::new()),
                    wake: Condvar::new(),
                });
                let worker = c.clone();
                match std::thread::Builder::new()
                    .name("snapshot-cache".into())
                    .spawn(move || worker.run())
                {
                    Ok(_) => Some(c),
                    Err(error) => {
                        tracing::warn!(%error, "snapshot cache worker unavailable");
                        None
                    }
                }
            })
            .as_ref()?
            .clone();
        let mut slots = coordinator.slots.lock().unwrap_or_else(|e| e.into_inner());
        while slots.get(&key).is_some_and(|s| s.releasing) {
            slots = coordinator
                .wake
                .wait(slots)
                .unwrap_or_else(|e| e.into_inner());
        }
        if !slots.contains_key(&key) && slots.len() >= MAX_KEYS {
            return None;
        }
        slots
            .entry(key.clone())
            .or_insert_with(|| Slot {
                active: 0,
                deadline: Instant::now() + QUIET,
                job: None,
                releasing: false,
            })
            .active += 1;
        drop(slots);
        Some(Self { coordinator, key })
    }
    /// Only submit after all native restore validation and binding succeeded.
    pub(crate) fn complete(
        &self,
        published: &PublishedEnvironment,
        layers: &[(PathBuf, Arc<SharedFilesystemLayer>)],
    ) {
        match published.source_cache_job(layers) {
            Ok(Some(mut job)) => {
                let mut slots = self
                    .coordinator
                    .slots
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(slot) = slots.get_mut(&self.key) {
                    if let Some(previous) = slot.job.take() {
                        job.absorb_new_pools(previous);
                    }
                    slot.job = Some(job);
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "source cache cleanup skipped"),
        }
    }
}
impl Drop for RestoreCacheWindow {
    fn drop(&mut self) {
        let mut slots = self
            .coordinator
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = slots.get_mut(&self.key) {
            slot.active -= 1;
            slot.deadline = Instant::now() + QUIET;
            if slot.active == 0 && slot.job.is_none() {
                slots.remove(&self.key);
            }
        }
        self.coordinator.wake.notify_all();
    }
}
impl Coordinator {
    fn run(self: Arc<Self>) {
        loop {
            let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            let key = loop {
                if let Some(key) = ready_key(&slots, Instant::now()) {
                    break key;
                }
                let wait = slots
                    .values()
                    .filter(|s| s.active == 0 && s.job.is_some())
                    .map(|s| s.deadline.saturating_duration_since(Instant::now()))
                    .min()
                    .unwrap_or(Duration::from_secs(3600));
                slots = self
                    .wake
                    .wait_timeout(slots, wait)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            };
            let slot = slots.get_mut(&key).unwrap();
            slot.releasing = true;
            let job = slot.job.take().unwrap();
            drop(slots);
            job.release();
            let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            slots.remove(&key);
            self.wake.notify_all();
        }
    }
}

pub(super) struct SourceLayer {
    pub source: File,
    pub shared: File,
    pub paths: Vec<PathBuf>,
    pub _owner: Arc<SharedFilesystemLayer>,
}
pub(super) struct Job {
    pub layers: Vec<SourceLayer>,
    pub new_pools: Vec<PoolLayer>,
    pub _reference: File,
}
// Only a successful fresh publication can issue this in-memory capability.
// Retained/pre-existing pools never get it, and each capability is consumed once.
pub(super) struct PoolLayer {
    directory: File,
    paths: Vec<PathBuf>,
    _owner: Arc<SharedFilesystemLayer>,
}
impl SharedFilesystemLayer {
    pub(super) fn take_preparation_cache(self: &Arc<Self>) -> anyhow::Result<Option<PoolLayer>> {
        use std::sync::atomic::Ordering;
        if !self.preparation_cache.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let (_, inventory) = super::filesystems::seal(self)?;
        let paths: Vec<PathBuf> = inventory
            .entries
            .into_iter()
            .filter(|e| matches!(e.object, super::TreeObject::File { .. }))
            .map(|e| PathBuf::from(std::ffi::OsStr::from_bytes(&e.path)))
            .collect();
        if paths.len() > MAX_FILES
            || paths.iter().map(|p| p.as_os_str().len()).sum::<usize>() > MAX_PATH_BYTES
        {
            return Ok(None);
        }
        use std::os::unix::fs::OpenOptionsExt;
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(self.root())?;
        if !self.preparation_cache.swap(false, Ordering::Relaxed) {
            return Ok(None);
        }
        Ok(Some(PoolLayer {
            directory,
            paths,
            _owner: self.clone(),
        }))
    }
}
impl PoolLayer {
    fn release(&self) -> std::io::Result<()> {
        let mut seen = BTreeSet::new();
        for path in &self.paths {
            let file = open_beneath(&self.directory, path)?;
            let m = file.metadata()?;
            // Multi-link data is conservatively excluded, as with source cleanup.
            if !m.is_file() || m.nlink() != 1 || !seen.insert((m.dev(), m.ino())) {
                continue;
            }
            let rc =
                unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
            if rc != 0 {
                return Err(std::io::Error::from_raw_os_error(rc));
            }
        }
        tracing::info!(
            files = seen.len(),
            "new filesystem pool cache advice completed (not measured eviction)"
        );
        Ok(())
    }
}

// Resolve every component beneath the pinned directory without following any
// symlink. Unsupported kernels skip this optional optimization; no unsafe fallback.
fn open_beneath(root: &File, relative: &Path) -> std::io::Result<File> {
    #[repr(C)]
    struct How {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    let name = CString::new(relative.as_os_str().as_bytes())?;
    let how = How {
        flags: (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK) as u64,
        mode: 0,
        resolve: 0x08 | 0x04,
    }; // RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS
    // SAFETY: valid borrowed descriptors, C string and correctly sized open_how.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            name.as_ptr(),
            &how,
            std::mem::size_of::<How>(),
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful openat2 returns a fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd as i32) })
}
impl Job {
    pub(super) fn absorb_new_pools(&mut self, previous: Self) {
        self.new_pools.extend(previous.new_pools);
        let files: usize = self.new_pools.iter().map(|p| p.paths.len()).sum();
        let bytes: usize = self
            .new_pools
            .iter()
            .flat_map(|p| &p.paths)
            .map(|p| p.as_os_str().len())
            .sum();
        if files > MAX_FILES || bytes > MAX_PATH_BYTES {
            self.new_pools.clear();
        }
    }
    pub(super) fn release(self) {
        for pool in &self.new_pools {
            if let Err(error) = pool.release() {
                tracing::warn!(%error, "new filesystem pool cache advice stopped");
            }
        }
        let mut seen = BTreeSet::new();
        let mut advised = 0usize;
        let mut skipped = 0usize;
        for layer in &self.layers {
            for path in &layer.paths {
                let result = (|| -> std::io::Result<bool> {
                    let source = open_beneath(&layer.source, path)?;
                    let shared = open_beneath(&layer.shared, path)?;
                    let a = source.metadata()?;
                    let b = shared.metadata()?;
                    if !a.is_file()
                        || !b.is_file()
                        || a.len() != b.len()
                        || (a.dev(), a.ino()) == (b.dev(), b.ino())
                        || a.nlink() != 1
                        || !seen.insert((a.dev(), a.ino()))
                    {
                        return Ok(false);
                    }
                    // SAFETY: source FD is live; advice cannot modify file bytes.
                    let rc = unsafe {
                        libc::posix_fadvise(source.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED)
                    };
                    if rc != 0 {
                        return Err(std::io::Error::from_raw_os_error(rc));
                    }
                    Ok(true)
                })();
                match result {
                    Ok(true) => advised += 1,
                    Ok(false) => skipped += 1,
                    Err(error) => {
                        // Fail closed on path/identity/capability trouble. Never
                        // retry with weaker path rules or affect restore success.
                        tracing::warn!(%error, advised, skipped, "source cache cleanup stopped");
                        return;
                    }
                }
            }
        }
        tracing::info!(
            advised,
            skipped,
            "source-only restore cache advice completed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn active_and_quiet_preparations_are_not_released() {
        let key = (PathBuf::from("snapshot"), "id".into());
        let now = Instant::now();
        let mut slots = BTreeMap::from([(
            key.clone(),
            Slot {
                active: 2,
                deadline: now,
                job: Some(1),
                releasing: false,
            },
        )]);
        assert!(ready_key(&slots, now + QUIET).is_none());
        slots.get_mut(&key).unwrap().active = 0;
        slots.get_mut(&key).unwrap().deadline = now + QUIET;
        assert!(ready_key(&slots, now).is_none());
        assert_eq!(ready_key(&slots, now + QUIET), Some(key.clone()));
        slots.get_mut(&key).unwrap().releasing = true;
        assert!(ready_key(&slots, now + QUIET).is_none());
    }
    #[test]
    fn failed_preparations_release_tracking_without_enqueuing_cleanup() {
        let key = (PathBuf::from("snapshot"), "id".into());
        let coordinator = Arc::new(Coordinator {
            slots: Mutex::new(BTreeMap::from([(
                key.clone(),
                Slot {
                    active: 2,
                    deadline: Instant::now(),
                    job: None,
                    releasing: false,
                },
            )])),
            wake: Condvar::new(),
        });
        let first = RestoreCacheWindow {
            coordinator: coordinator.clone(),
            key: key.clone(),
        };
        let second = RestoreCacheWindow {
            coordinator: coordinator.clone(),
            key: key.clone(),
        };
        drop(first);
        assert_eq!(
            coordinator.slots.lock().unwrap().get(&key).unwrap().active,
            1
        );
        drop(second);
        assert!(coordinator.slots.lock().unwrap().is_empty());
    }
    #[test]
    fn safe_open_rejects_symlinks_and_parent_escape() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("data"), b"untouched").unwrap();
        std::os::unix::fs::symlink("data", root.path().join("link")).unwrap();
        let dir = File::open(root.path()).unwrap();
        assert!(open_beneath(&dir, Path::new("data")).is_ok());
        assert!(open_beneath(&dir, Path::new("link")).is_err());
        assert!(open_beneath(&dir, Path::new("../data")).is_err());
        assert_eq!(
            std::fs::read(root.path().join("data")).unwrap(),
            b"untouched"
        );
    }
}
