//! Compatibility adapter: mmap writes stage pages; offload commits generations.
//! The kernel owns fault handling; no signal handler performs allocation or I/O.
use crate::ram_backing::CompressedRam;
use fuser::{
    BackgroundSession, FileAttr, FileType, Filesystem, KernelConfig, MountOption, ReplyAttr,
    ReplyData, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, Request, TimeOrNow,
};
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ROOT: u64 = 1;
const RAM: u64 = 2;
const MAX_IO: usize = 1024 * 1024;

pub(super) struct CompressedMount {
    // Drop file descriptors before unmount, and unmount before deleting the directory.
    session: Option<BackgroundSession>,
    store: Arc<Mutex<CompressedRam>>,
    _directory: tempfile::TempDir,
}

impl CompressedMount {
    pub(super) fn commit_store(&self) -> Arc<Mutex<CompressedRam>> {
        self.store.clone()
    }

    pub(super) fn new(storage: &File, directory: &Path, layers: &Path) -> io::Result<(Self, File)> {
        let store = Arc::new(Mutex::new(CompressedRam::create(
            storage.try_clone()?,
            layers,
        )?));
        let temporary = tempfile::Builder::new()
            .prefix("mount-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(directory)?;
        let options = vec![
            MountOption::FSName("pvisor-pvzram".into()),
            MountOption::DefaultPermissions,
            MountOption::NoExec,
            MountOption::NoSuid,
            MountOption::NoDev,
            #[cfg(target_os = "macos")]
            MountOption::CUSTOM("backend=kernel".into()),
        ];
        // ponytail: one mount/thread per VM; share a mount only if its overhead
        // becomes material in density measurements. No decompressed userspace cache.
        let session = fuser::spawn_mount2(
            RamFs {
                store: store.clone(),
            },
            temporary.path(),
            &options,
        )?;
        let mount = Self {
            session: Some(session),
            store,
            _directory: temporary,
        };
        // macFUSE can return a channel before its asynchronous mount appears.
        // Do not mistake the still-empty host directory for a missing RAM inode.
        let deadline = Instant::now() + Duration::from_secs(5);
        let file = loop {
            match OpenOptions::new()
                .read(true)
                .write(true)
                .open(mount._directory.path().join("ram"))
            {
                Ok(file) => break file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    if mount.session.as_ref().unwrap().guard.is_finished() {
                        return Err(io::Error::other(
                            "compressed RAM FUSE session exited before mount became ready; macOS requires the kernel backend",
                        ));
                    }
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "compressed RAM FUSE mount did not become ready",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error),
            }
        };
        Ok((mount, file))
    }
}

impl Drop for CompressedMount {
    fn drop(&mut self) {
        if let Some(session) = self.session.take()
            && let Err(error) = session.unmount()
        {
            tracing::warn!(%error, "cannot unmount compressed RAM");
        }
    }
}

struct RamFs {
    store: Arc<Mutex<CompressedRam>>,
}
impl RamFs {
    fn attr(&self, ino: u64) -> FileAttr {
        let logical_bytes = self.store.lock().unwrap().logical_bytes();
        FileAttr {
            ino,
            size: if ino == RAM { logical_bytes } else { 0 },
            blocks: if ino == RAM {
                logical_bytes.div_ceil(512)
            } else {
                0
            },
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: if ino == RAM {
                FileType::RegularFile
            } else {
                FileType::Directory
            },
            perm: if ino == RAM { 0o600 } else { 0o700 },
            nlink: if ino == RAM { 1 } else { 2 },
            uid: unsafe { libc::geteuid() },
            gid: unsafe { libc::getegid() },
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }
}

impl Filesystem for RamFs {
    fn init(&mut self, _req: &Request<'_>, config: &mut KernelConfig) -> Result<(), i32> {
        let _ = config.set_max_write(crate::ram_backing::BLOCK_BYTES as u32);
        let _ = config.set_max_readahead(crate::ram_backing::BLOCK_BYTES as u32);
        Ok(())
    }

    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        if parent == ROOT && name == OsStr::new("ram") {
            reply.entry(&Duration::ZERO, &self.attr(RAM), 0);
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        if [ROOT, RAM].contains(&ino) {
            reply.attr(&Duration::ZERO, &self.attr(ino));
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn setattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        if ino != RAM || mode.is_some() || uid.is_some() || gid.is_some() || flags.is_some() {
            reply.error(libc::EPERM);
            return;
        }
        if let Some(size) = size
            && let Err(error) = self.store.lock().unwrap().set_len(size)
        {
            tracing::error!(%error, "compressed RAM resize failed");
            reply.error(libc::EIO);
            return;
        }
        reply.attr(&Duration::ZERO, &self.attr(RAM));
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        if ino != RAM {
            reply.error(libc::ENOENT);
        } else if flags & libc::O_TRUNC != 0 {
            reply.error(libc::EPERM);
        } else {
            reply.opened(0, 0);
        } // Cached I/O: direct_io would disable mmap.
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        if ino != RAM || offset < 0 || size as usize > MAX_IO {
            reply.error(libc::EINVAL);
            return;
        }
        let mut data = vec![0; size as usize];
        match self.store.lock().unwrap().read_at(offset as u64, &mut data) {
            Ok(length) => reply.data(&data[..length]),
            Err(error) => {
                tracing::error!(%error, "compressed RAM read failed");
                reply.error(libc::EIO);
            }
        }
    }

    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        if ino != RAM || offset < 0 || data.len() > MAX_IO {
            reply.error(libc::EINVAL);
            return;
        }
        match self.store.lock().unwrap().write_at(offset as u64, data) {
            Ok(()) => reply.written(data.len() as u32),
            Err(error) => {
                tracing::error!(%error, "compressed RAM write failed");
                reply.error(libc::EIO);
            }
        }
    }

    fn flush(&mut self, req: &Request<'_>, ino: u64, fh: u64, _lock_owner: u64, reply: ReplyEmpty) {
        self.fsync(req, ino, fh, false, reply);
    }
    fn fsync(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        if ino != RAM {
            reply.error(libc::EINVAL);
            return;
        }
        match self.store.lock().unwrap().flush_writes() {
            Ok(()) => reply.ok(),
            Err(error) => {
                tracing::error!(%error, "compressed RAM sync failed");
                reply.error(libc::EIO);
            }
        }
    }
}
