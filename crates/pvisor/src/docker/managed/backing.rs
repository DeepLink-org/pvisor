//! Execution-local backing; no OCI preparation, scheduler policy or checkpoint.
use anyhow::{Context, Result};
use pvisor_vm::api::{RamFileMapping, RamFileMount};
use std::{
    fs::File,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub(in crate::docker) struct Backing {
    // File consumers close before mount teardown, then staging storage drops.
    pub(in crate::docker) file: File,
    _mount: Option<RamFileMount>,
    store: Option<Arc<Mutex<crate::ram_backing::CompressedRam>>>,
    _directory: Option<tempfile::TempDir>,
    remote: Option<Mutex<std::os::unix::net::UnixStream>>,
}
impl Backing {
    pub(in crate::docker) fn create(directory: &Path, compressed: bool) -> Result<Self> {
        if !compressed {
            return Ok(Self {
                file: tempfile::tempfile_in(directory)?,
                _mount: None,
                store: None,
                _directory: None,
                remote: None,
            });
        }
        let private = tempfile::Builder::new()
            .prefix("managed-ram-")
            .tempdir_in(directory)?;
        let store = Arc::new(Mutex::new(crate::ram_backing::CompressedRam::create(
            tempfile::tempfile_in(private.path())?,
            private.path(),
        )?));
        // FUSE readiness failure rejects this launch; never silently use raw RAM.
        let (mount, file) = RamFileMount::mount(store.clone(), private.path())?;
        Ok(Self {
            file,
            _mount: Some(mount),
            store: Some(store),
            _directory: Some(private),
            remote: None,
        })
    }
    pub(in crate::docker) fn path(&self) -> PathBuf {
        PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            self.file.as_raw_fd()
        ))
    }
    pub(in crate::docker) fn finish_offload(&self) -> Result<()> {
        if let Some(remote) = &self.remote {
            // Keep the VMM parked while its separate owner publishes compression.
            self.file
                .sync_all()
                .context("managed RAM writeback drain")?;
            let mut stream = remote
                .lock()
                .map_err(|_| anyhow::anyhow!("commit channel poisoned"))?;
            crate::host_transport::write_host_frame_sync(&mut stream, &"commit")?;
            let reply: std::result::Result<(), String> =
                crate::host_transport::read_host_frame_sync(&mut stream)?;
            return reply.map_err(anyhow::Error::msg);
        }
        if let Some(store) = &self.store {
            // Native acknowledgement has drained CPU/device writers. Fsync the
            // logical FUSE inode BEFORE locking its store (fsync needs that lock).
            self.file
                .sync_all()
                .context("managed RAM writeback drain")?;
            store
                .lock()
                .map_err(|_| anyhow::anyhow!("managed RAM store poisoned"))?
                .sync_all()
                .context("managed compressed RAM publication")?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn staged() -> (Arc<Backing>, Arc<Mutex<crate::ram_backing::CompressedRam>>) {
    let directory = tempfile::tempdir().unwrap();
    let mut store = crate::ram_backing::CompressedRam::create(
        tempfile::tempfile_in(directory.path()).unwrap(),
        directory.path(),
    )
    .unwrap();
    store.set_len(4096).unwrap();
    store.write_at(0, b"first generation").unwrap();
    let store = Arc::new(Mutex::new(store));
    let file = tempfile::tempfile_in(directory.path()).unwrap();
    (
        Arc::new(Backing {
            file,
            _mount: None,
            store: Some(store.clone()),
            _directory: Some(directory),
            remote: None,
        }),
        store,
    )
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod tests {
    use super::*;
    use std::os::unix::fs::FileExt;

    #[test]
    #[ignore = "requires Linux FUSE mount permission; no VM is started"]
    fn mounted_backing_commits_and_refaults_changed_generations() {
        let directory = tempfile::tempdir().unwrap();
        let backing = Backing::create(directory.path(), true).unwrap();
        backing.file.set_len(4096).unwrap();
        // Mmap writes exercise the kernel/FUSE staging path, not store.write_at.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                4096,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                backing.file.as_raw_fd(),
                0,
            )
        };
        assert_ne!(address, libc::MAP_FAILED);
        struct Mapping(*mut libc::c_void);
        impl Drop for Mapping {
            fn drop(&mut self) {
                unsafe {
                    libc::munmap(self.0, 4096);
                }
            }
        }
        let mapping = Mapping(address);
        for byte in [17u8, 93u8] {
            unsafe {
                std::ptr::write_bytes(address.cast::<u8>(), byte, 4096);
            }
            backing.finish_offload().unwrap();
            assert!(
                backing
                    .store
                    .as_ref()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .head_id()
                    .is_some()
            );
            assert_eq!(
                unsafe { libc::madvise(address, 4096, libc::MADV_DONTNEED) },
                0
            );
            assert!(
                unsafe { std::slice::from_raw_parts(address.cast::<u8>(), 4096) }
                    .iter()
                    .all(|value| *value == byte)
            );
            let mut data = [0; 4096];
            backing.file.read_exact_at(&mut data, 0).unwrap();
            assert!(data.iter().all(|value| *value == byte));
        }
        drop(mapping);
        drop(backing);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}

// The VMM exits via _exit; its Rust destructors cannot own a live FUSE mount.
// The ordinary container owner retains the mount and joins publication only
// after ChildGuard has reaped the VMM. This does not introduce another launcher.
pub(in crate::docker) const OWNER_FD: &str = "PVISOR_INTERNAL_MANAGED_RAM_FD";

#[derive(serde::Serialize, serde::Deserialize)]
struct Handoff {
    target: super::Target,
    path: PathBuf,
    device: u64,
    inode: u64,
}

pub(in crate::docker) struct ParentBacking {
    // A strong backing reference is held by the worker until it has joined.
    cancel: std::os::unix::net::UnixStream,
    worker: Option<std::thread::JoinHandle<()>>,
    _backing: Arc<Backing>,
}
impl ParentBacking {
    pub(in crate::docker) fn prepare(
        directory: &Path,
        target: super::Target,
    ) -> Result<(Self, std::os::unix::net::UnixStream)> {
        let backing = Arc::new(Backing::create(directory, true)?);
        Self::serve(backing, target)
    }
    fn serve(
        backing: Arc<Backing>,
        target: super::Target,
    ) -> Result<(Self, std::os::unix::net::UnixStream)> {
        use std::os::unix::fs::MetadataExt;
        let (mut parent, child) = std::os::unix::net::UnixStream::pair()?;
        let cancel = parent.try_clone()?;
        let meta = backing.file.metadata()?;
        let handoff = Handoff {
            target,
            path: backing.path(),
            device: meta.dev(),
            inode: meta.ino(),
        };
        let retained = backing.clone();
        let worker = std::thread::Builder::new()
            .name("docker-ram-store".into())
            .spawn(move || {
                let mut serve = || -> Result<()> {
                    crate::host_transport::write_host_frame_sync(&mut parent, &handoff)?;
                    loop {
                        let request: String =
                            crate::host_transport::read_host_frame_sync(&mut parent)?;
                        anyhow::ensure!(request == "commit", "invalid RAM owner request");
                        let result = backing.finish_offload().map_err(|e| format!("{e:#}"));
                        let failed = result.is_err();
                        crate::host_transport::write_host_frame_sync(&mut parent, &result)?;
                        anyhow::ensure!(!failed, "RAM publication failed; channel fenced");
                    }
                };
                let _ = serve(); // EOF/disconnect never manufactures a successful commit.
                let _ = parent.shutdown(std::net::Shutdown::Both);
            })?;
        Ok((
            Self {
                cancel,
                worker: Some(worker),
                _backing: retained,
            },
            child,
        ))
    }
}
impl Drop for ParentBacking {
    fn drop(&mut self) {
        let _ = self.cancel.shutdown(std::net::Shutdown::Both);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Backing {
    pub(in crate::docker) fn from_parent(target: &super::Target) -> Result<Self> {
        use std::os::fd::FromRawFd;
        let fd: i32 = std::env::var(OWNER_FD)
            .context("missing RAM owner handoff")?
            .parse()?;
        anyhow::ensure!(fd > 2, "invalid RAM owner descriptor");
        let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
        Self::receive_parent(stream, target)
    }
    fn receive_parent(
        mut stream: std::os::unix::net::UnixStream,
        target: &super::Target,
    ) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let fd = stream.as_raw_fd();
        crate::host_transport::authorize_host_peer_sync(&stream)?;
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        stream.set_read_timeout(Some(std::time::Duration::from_secs(315)))?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
        let handoff: Handoff = crate::host_transport::read_host_frame_sync(&mut stream)?;
        anyhow::ensure!(&handoff.target == target, "stale RAM owner generation");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&handoff.path)?;
        let meta = file.metadata()?;
        anyhow::ensure!(
            (meta.dev(), meta.ino()) == (handoff.device, handoff.inode),
            "RAM backing changed"
        );
        Ok(Self {
            file,
            _mount: None,
            store: None,
            _directory: None,
            remote: Some(Mutex::new(stream)),
        })
    }
}

#[cfg(test)]
mod owner_tests {
    use super::*;
    use crate::host_transport::{read_host_frame_sync, write_host_frame_sync};
    fn target() -> super::super::Target {
        super::super::Target {
            container_id: "a".repeat(64),
            generation: "b".repeat(32),
        }
    }
    #[test]
    fn parent_retains_store_after_child_disconnect_until_reap_scope_ends() {
        let (backing, store) = staged();
        let weak = Arc::downgrade(&backing);
        let (owner, mut child) = ParentBacking::serve(backing, target()).unwrap();
        child
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let _: Handoff = read_host_frame_sync(&mut child).unwrap();
        write_host_frame_sync(&mut child, &"commit").unwrap();
        let reply: std::result::Result<(), String> = read_host_frame_sync(&mut child).unwrap();
        reply.unwrap();
        assert!(store.lock().unwrap().head_id().is_some());
        drop(child);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !owner.worker.as_ref().unwrap().is_finished() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(weak.upgrade().is_some());
        drop(owner); // the owner calls this only after reaping its ChildGuard
        assert!(weak.upgrade().is_none());
    }
    #[test]
    fn parent_commit_error_closes_channel_without_discarding_retained_store() {
        let (backing, store) = staged();
        let weak = Arc::downgrade(&backing);
        let _ = std::panic::catch_unwind(|| {
            let _lock = store.lock().unwrap();
            panic!("injected");
        });
        let (owner, mut child) = ParentBacking::serve(backing, target()).unwrap();
        child
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let _: Handoff = read_host_frame_sync(&mut child).unwrap();
        write_host_frame_sync(&mut child, &"commit").unwrap();
        let reply: std::result::Result<(), String> = read_host_frame_sync(&mut child).unwrap();
        assert!(reply.is_err());
        // EOF is not a successful commit and doesn't authorize store destruction.
        assert!(read_host_frame_sync::<std::result::Result<(), String>>(&mut child).is_err());
        assert!(weak.upgrade().is_some());
        drop(child);
        drop(owner);
        assert!(weak.upgrade().is_none());
    }
    #[test]
    fn mismatched_handoff_generation_is_rejected_before_opening_backing() {
        let (backing, _) = staged();
        let (owner, child) = ParentBacking::serve(backing, target()).unwrap();
        let mut wrong = target();
        wrong.generation = "c".repeat(32);
        assert!(
            Backing::receive_parent(child, &wrong)
                .err()
                .unwrap()
                .to_string()
                .contains("stale")
        );
        drop(owner);
    }
}
