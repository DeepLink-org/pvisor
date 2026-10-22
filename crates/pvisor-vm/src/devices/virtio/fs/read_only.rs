// Read-only wrapper for PassthroughFs.
//
// Delegates all read-only FUSE operations to the inner PassthroughFs and
// rejects all mutating operations with EROFS (read-only filesystem).
//
// IMPORTANT: When adding new methods to the FileSystem trait, review this
// wrapper to ensure mutating operations are explicitly blocked with EROFS.
// Unoverridden methods fall back to the trait defaults (which return ENOSYS),
// so the wrapper fails closed -- but new methods should still be explicitly
// handled here for correct error semantics.

#[cfg(target_os = "macos")]
use crossbeam_channel::Sender;
use std::ffi::CStr;
use std::io;
use std::sync::atomic::AtomicI32;
use std::sync::Arc;
use std::time::Duration;

#[cfg(target_os = "macos")]
use crate::utils::worker_message::WorkerMessage;

use super::filesystem::{
    Context, DirEntry, Entry, Extensions, FileSystem, FsOptions, GetxattrReply, ListxattrReply,
    OpenOptions, SetattrValid, ZeroCopyReader, ZeroCopyWriter,
};
use super::fuse;
use super::inode_alloc::InodeAllocator;
use super::passthrough::{self, PassthroughFs};
use crate::devices::virtio::bindings;

type Inode = u64;
type Handle = u64;

fn erofs() -> io::Error {
    io::Error::from_raw_os_error(libc::EROFS)
}

fn read_only_open_flags(flags: u32) -> io::Result<u32> {
    let f = flags as i32;
    if f & libc::O_ACCMODE != libc::O_RDONLY {
        return Err(erofs());
    }
    if f & bindings::LINUX_O_TRUNC != 0 {
        return Err(erofs());
    }
    // OPEN flags are Linux wire values, including on macOS.
    const LINUX_O_TMPFILE: i32 = 0x410000;
    if f & LINUX_O_TMPFILE == LINUX_O_TMPFILE {
        return Err(erofs());
    }

    Ok((flags & !(libc::O_ACCMODE as u32)) | (libc::O_RDONLY as u32))
}

pub struct PassthroughFsRo {
    inner: PassthroughFs,
    #[cfg(target_os = "linux")]
    dax: Option<super::dax::ReadOnlyDax>,
}

impl PassthroughFsRo {
    pub fn new(cfg: passthrough::Config, inode_alloc: Arc<InodeAllocator>) -> io::Result<Self> {
        Ok(Self {
            inner: PassthroughFs::new(cfg, inode_alloc)?,
            #[cfg(target_os = "linux")]
            dax: None,
        })
    }
    #[cfg(target_os = "linux")]
    pub(super) fn enable_dax(
        &mut self,
        region: crate::devices::virtio::VirtioShmRegion,
    ) -> io::Result<()> {
        self.dax = Some(super::dax::ReadOnlyDax::new(region)?);
        Ok(())
    }
    fn dax_attr_flags(&self, attr: &bindings::stat64) -> u32 {
        #[cfg(target_os = "linux")]
        if self.dax.is_some() && attr.st_mode & libc::S_IFMT == libc::S_IFREG {
            return 2;
        } // FUSE_ATTR_DAX
        let _ = attr;
        0
    }
}

impl FileSystem for PassthroughFsRo {
    fn supports_directory_image(&self) -> bool {
        #[cfg(target_os = "linux")]
        if self.dax.is_some() {
            return true;
        }
        false
    }

    fn capture_state(&self) -> io::Result<super::snapshot::FsSnapshot> {
        #[cfg(target_os = "linux")]
        if self.dax.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "DAX mappings cannot be captured",
            ));
        }
        Ok(super::snapshot::FsSnapshot::ReadOnly(Box::new(
            self.inner.capture_state()?,
        )))
    }
    fn restore_state(&self, state: &super::snapshot::FsSnapshot) -> io::Result<()> {
        let super::snapshot::FsSnapshot::ReadOnly(inner) = state else {
            return Err(super::snapshot::invalid(
                "read-only filesystem type mismatch",
            ));
        };
        self.inner.restore_state(inner)
    }

    type Inode = Inode;
    type Handle = Handle;

    // --- Delegated read-only operations ---

    fn init(&self, capable: FsOptions) -> io::Result<FsOptions> {
        let opts = self.inner.init(capable)?;
        #[cfg(target_os = "linux")]
        let opts = if self.dax.is_some() {
            opts | (capable & (FsOptions::HAS_INODE_DAX | FsOptions::MAP_ALIGNMENT))
        } else {
            opts
        };
        // Strip WRITEBACK_CACHE to prevent the guest kernel from buffering writes.
        Ok(opts & !FsOptions::WRITEBACK_CACHE)
    }

    fn destroy(&self) {
        #[cfg(target_os = "linux")]
        if let Some(dax) = &self.dax {
            if let Err(error) = dax.clear() {
                log::error!("clear DAX window: {error}");
            }
        }
        self.inner.destroy()
    }

    fn lookup(&self, ctx: Context, parent: Inode, name: &CStr) -> io::Result<Entry> {
        let mut entry = self.inner.lookup(ctx, parent, name)?;
        entry.attr_flags |= self.dax_attr_flags(&entry.attr);
        Ok(entry)
    }

    fn attribute_flags(&self, _inode: Inode, attr: &bindings::stat64) -> u32 {
        self.dax_attr_flags(attr)
    }

    fn forget(&self, ctx: Context, inode: Inode, count: u64) {
        self.inner.forget(ctx, inode, count)
    }

    fn batch_forget(&self, ctx: Context, requests: Vec<(Inode, u64)>) {
        self.inner.batch_forget(ctx, requests)
    }

    fn getattr(
        &self,
        ctx: Context,
        inode: Inode,
        handle: Option<Handle>,
    ) -> io::Result<(bindings::stat64, Duration)> {
        self.inner.getattr(ctx, inode, handle)
    }

    fn readlink(&self, ctx: Context, inode: Inode) -> io::Result<Vec<u8>> {
        self.inner.readlink(ctx, inode)
    }

    fn open(
        &self,
        ctx: Context,
        inode: Inode,
        kill_priv: bool,
        flags: u32,
    ) -> io::Result<(Option<Handle>, OpenOptions)> {
        let ro_flags = read_only_open_flags(flags)?;
        self.inner.open(ctx, inode, kill_priv, ro_flags)
    }

    fn read<W: io::Write + ZeroCopyWriter>(
        &self,
        ctx: Context,
        inode: Inode,
        handle: Handle,
        w: W,
        size: u32,
        offset: u64,
        lock_owner: Option<u64>,
        flags: u32,
    ) -> io::Result<usize> {
        self.inner
            .read(ctx, inode, handle, w, size, offset, lock_owner, flags)
    }

    fn flush(&self, ctx: Context, inode: Inode, handle: Handle, lock_owner: u64) -> io::Result<()> {
        self.inner.flush(ctx, inode, handle, lock_owner)
    }

    fn fsync(&self, ctx: Context, inode: Inode, datasync: bool, handle: Handle) -> io::Result<()> {
        self.inner.fsync(ctx, inode, datasync, handle)
    }

    fn release(
        &self,
        ctx: Context,
        inode: Inode,
        flags: u32,
        handle: Handle,
        flush: bool,
        flock_release: bool,
        lock_owner: Option<u64>,
    ) -> io::Result<()> {
        self.inner
            .release(ctx, inode, flags, handle, flush, flock_release, lock_owner)
    }

    fn statfs(&self, ctx: Context, inode: Inode) -> io::Result<bindings::statvfs64> {
        let mut st = self.inner.statfs(ctx, inode)?;
        st.f_flag |= libc::ST_RDONLY;
        Ok(st)
    }

    fn getxattr(
        &self,
        ctx: Context,
        inode: Inode,
        name: &CStr,
        size: u32,
    ) -> io::Result<GetxattrReply> {
        self.inner.getxattr(ctx, inode, name, size)
    }

    fn listxattr(&self, ctx: Context, inode: Inode, size: u32) -> io::Result<ListxattrReply> {
        self.inner.listxattr(ctx, inode, size)
    }

    fn opendir(
        &self,
        ctx: Context,
        inode: Inode,
        flags: u32,
    ) -> io::Result<(Option<Handle>, OpenOptions)> {
        let f = flags as i32;
        let accmode = f & libc::O_ACCMODE;
        if accmode != libc::O_RDONLY {
            return Err(erofs());
        }
        // Force O_RDONLY on the underlying call.
        let ro_flags = (flags & !(libc::O_ACCMODE as u32)) | (libc::O_RDONLY as u32);
        self.inner.opendir(ctx, inode, ro_flags)
    }

    fn readdir<F>(
        &self,
        ctx: Context,
        inode: Inode,
        handle: Handle,
        size: u32,
        offset: u64,
        add_entry: F,
    ) -> io::Result<()>
    where
        F: FnMut(DirEntry) -> io::Result<usize>,
    {
        self.inner
            .readdir(ctx, inode, handle, size, offset, add_entry)
    }

    fn readdirplus<F>(
        &self,
        ctx: Context,
        inode: Inode,
        handle: Handle,
        size: u32,
        offset: u64,
        mut add_entry: F,
    ) -> io::Result<()>
    where
        F: FnMut(DirEntry, Entry) -> io::Result<usize>,
    {
        self.inner
            .readdirplus(ctx, inode, handle, size, offset, |dir, mut entry| {
                entry.attr_flags |= self.dax_attr_flags(&entry.attr);
                add_entry(dir, entry)
            })
    }

    fn fsyncdir(
        &self,
        ctx: Context,
        inode: Inode,
        datasync: bool,
        handle: Handle,
    ) -> io::Result<()> {
        self.inner.fsyncdir(ctx, inode, datasync, handle)
    }

    fn releasedir(&self, ctx: Context, inode: Inode, flags: u32, handle: Handle) -> io::Result<()> {
        self.inner.releasedir(ctx, inode, flags, handle)
    }

    fn access(&self, ctx: Context, inode: Inode, mask: u32) -> io::Result<()> {
        if mask & (libc::W_OK as u32) != 0 {
            return Err(erofs());
        }
        self.inner.access(ctx, inode, mask)
    }

    fn lseek(
        &self,
        ctx: Context,
        inode: Inode,
        handle: Handle,
        offset: u64,
        whence: u32,
    ) -> io::Result<u64> {
        self.inner.lseek(ctx, inode, handle, offset, whence)
    }

    fn setupmapping(
        &self,
        ctx: Context,
        inode: Inode,
        handle: Handle,
        foffset: u64,
        len: u64,
        flags: u64,
        moffset: u64,
        host_shm_base: u64,
        shm_size: u64,
        #[cfg(target_os = "macos")] map_sender: &Option<Sender<WorkerMessage>>,
    ) -> io::Result<()> {
        // Reject writable mappings.
        if (flags & fuse::SetupmappingFlags::WRITE.bits()) != 0 {
            return Err(erofs());
        }
        #[cfg(target_os = "linux")]
        {
            let _ = ctx;
            let dax = self
                .dax
                .as_ref()
                .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOSYS))?;
            self.inner.with_dax_file(inode, handle, |file| {
                dax.setup(file, foffset, len, flags, moffset, host_shm_base, shm_size)
            })
        }
        #[cfg(target_os = "macos")]
        {
            let _ = (
                ctx,
                inode,
                handle,
                foffset,
                len,
                flags,
                moffset,
                host_shm_base,
                shm_size,
                map_sender,
            );
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "read-only DAX is supported only on Linux",
            ))
        }
    }

    fn removemapping(
        &self,
        ctx: Context,
        requests: Vec<fuse::RemovemappingOne>,
        host_shm_base: u64,
        shm_size: u64,
        #[cfg(target_os = "macos")] map_sender: &Option<Sender<WorkerMessage>>,
    ) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            let _ = ctx;
            self.dax
                .as_ref()
                .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOSYS))?
                .remove(&requests, host_shm_base, shm_size)
        }
        #[cfg(target_os = "macos")]
        {
            let _ = (ctx, requests, host_shm_base, shm_size, map_sender);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "read-only DAX is supported only on Linux",
            ))
        }
    }

    fn ioctl(
        &self,
        ctx: Context,
        inode: Inode,
        handle: Handle,
        flags: u32,
        cmd: u32,
        arg: u64,
        in_size: u32,
        out_size: u32,
        exit_code: &Arc<AtomicI32>,
    ) -> io::Result<Vec<u8>> {
        self.inner.ioctl(
            ctx, inode, handle, flags, cmd, arg, in_size, out_size, exit_code,
        )
    }

    // --- Write operations rejected with EROFS ---

    fn setattr(
        &self,
        _ctx: Context,
        _inode: Inode,
        _attr: bindings::stat64,
        _handle: Option<Handle>,
        _valid: SetattrValid,
    ) -> io::Result<(bindings::stat64, Duration)> {
        Err(erofs())
    }

    fn symlink(
        &self,
        _ctx: Context,
        _linkname: &CStr,
        _parent: Inode,
        _name: &CStr,
        _extensions: Extensions,
    ) -> io::Result<Entry> {
        Err(erofs())
    }

    fn mknod(
        &self,
        _ctx: Context,
        _inode: Inode,
        _name: &CStr,
        _mode: u32,
        _rdev: u32,
        _umask: u32,
        _extensions: Extensions,
    ) -> io::Result<Entry> {
        Err(erofs())
    }

    fn mkdir(
        &self,
        _ctx: Context,
        _parent: Inode,
        _name: &CStr,
        _mode: u32,
        _umask: u32,
        _extensions: Extensions,
    ) -> io::Result<Entry> {
        Err(erofs())
    }

    fn unlink(&self, _ctx: Context, _parent: Inode, _name: &CStr) -> io::Result<()> {
        Err(erofs())
    }

    fn rmdir(&self, _ctx: Context, _parent: Inode, _name: &CStr) -> io::Result<()> {
        Err(erofs())
    }

    fn rename(
        &self,
        _ctx: Context,
        _olddir: Inode,
        _oldname: &CStr,
        _newdir: Inode,
        _newname: &CStr,
        _flags: u32,
    ) -> io::Result<()> {
        Err(erofs())
    }

    fn link(
        &self,
        _ctx: Context,
        _inode: Inode,
        _newparent: Inode,
        _newname: &CStr,
    ) -> io::Result<Entry> {
        Err(erofs())
    }

    fn create(
        &self,
        _ctx: Context,
        _parent: Inode,
        _name: &CStr,
        _mode: u32,
        _kill_priv: bool,
        _flags: u32,
        _umask: u32,
        _extensions: Extensions,
    ) -> io::Result<(Entry, Option<Handle>, OpenOptions)> {
        Err(erofs())
    }

    fn write<R: io::Read + ZeroCopyReader>(
        &self,
        _ctx: Context,
        _inode: Inode,
        _handle: Handle,
        _r: R,
        _size: u32,
        _offset: u64,
        _lock_owner: Option<u64>,
        _delayed_write: bool,
        _kill_priv: bool,
        _flags: u32,
    ) -> io::Result<usize> {
        Err(erofs())
    }

    fn fallocate(
        &self,
        _ctx: Context,
        _inode: Inode,
        _handle: Handle,
        _mode: u32,
        _offset: u64,
        _length: u64,
    ) -> io::Result<()> {
        Err(erofs())
    }

    fn setxattr(
        &self,
        _ctx: Context,
        _inode: Inode,
        _name: &CStr,
        _value: &[u8],
        _flags: u32,
    ) -> io::Result<()> {
        Err(erofs())
    }

    fn removexattr(&self, _ctx: Context, _inode: Inode, _name: &CStr) -> io::Result<()> {
        Err(erofs())
    }

    fn copyfilerange(
        &self,
        _ctx: Context,
        _inode_in: Inode,
        _handle_in: Handle,
        _offset_in: u64,
        _inode_out: Inode,
        _handle_out: Handle,
        _offset_out: u64,
        _len: u64,
        _flags: u64,
    ) -> io::Result<usize> {
        Err(erofs())
    }
}

#[cfg(test)]
mod tests {
    use super::read_only_open_flags;

    #[test]
    fn read_only_open_flags_allow_append() {
        let flags = (libc::O_RDONLY | crate::devices::virtio::bindings::LINUX_O_APPEND) as u32;
        let ro_flags = read_only_open_flags(flags).unwrap();

        assert_eq!((ro_flags as i32) & libc::O_ACCMODE, libc::O_RDONLY);
        assert_ne!((ro_flags as i32) & crate::devices::virtio::bindings::LINUX_O_APPEND, 0);
    }

    #[test]
    fn read_only_open_flags_reject_write_access() {
        let err = read_only_open_flags(libc::O_WRONLY as u32).unwrap_err();

        assert_eq!(err.raw_os_error(), Some(libc::EROFS));
    }

    #[test]
    fn read_only_open_flags_reject_truncate() {
        let err = read_only_open_flags((libc::O_RDONLY | crate::devices::virtio::bindings::LINUX_O_TRUNC) as u32).unwrap_err();

        assert_eq!(err.raw_os_error(), Some(libc::EROFS));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dax_requires_opened_regular_file_and_preserves_readonly_lifecycle() {
        use super::*;
        use vm_memory::{Bytes, GuestAddress, GuestMemory, GuestMemoryMmap};
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("payload"), vec![0x5a; 4096]).unwrap();
        std::fs::create_dir(directory.path().join("dir")).unwrap();
        let memory = GuestMemoryMmap::<()>::from_ranges(&[(GuestAddress(0), 4096)]).unwrap();
        let region = crate::devices::virtio::VirtioShmRegion {
            host_addr: memory.get_host_address(GuestAddress(0)).unwrap() as u64,
            guest_addr: 0,
            size: 4096,
        };
        let mut fs = PassthroughFsRo::new(
            passthrough::Config {
                root_dir: directory.path().to_string_lossy().into_owned(),
                ..Default::default()
            },
            Arc::new(InodeAllocator::new()),
        )
        .unwrap();
        fs.enable_dax(region.clone()).unwrap();
        let options = fs
            .init(FsOptions::HAS_INODE_DAX | FsOptions::MAP_ALIGNMENT | FsOptions::WRITEBACK_CACHE)
            .unwrap();
        assert!(options.contains(FsOptions::HAS_INODE_DAX | FsOptions::MAP_ALIGNMENT));
        assert!(!options.contains(FsOptions::WRITEBACK_CACHE));
        let ctx = Context {
            uid: unsafe { libc::geteuid() },
            gid: unsafe { libc::getegid() },
            pid: 0,
        };
        let entry = fs.lookup(ctx, fuse::ROOT_ID, c"payload").unwrap();
        assert_eq!(entry.attr_flags, 2);
        assert_eq!(fs.attribute_flags(entry.inode, &entry.attr), 2);
        assert_eq!(fs.lookup(ctx, fuse::ROOT_ID, c"dir").unwrap().attr_flags, 0);
        assert_eq!(
            fs.setupmapping(ctx, entry.inode, 999, 0, 4096, 2, 0, region.host_addr, 4096)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EBADF)
        );
        assert_eq!(
            fs.setupmapping(
                ctx,
                entry.inode,
                u64::MAX,
                0,
                4096,
                2,
                0,
                region.host_addr,
                4096
            )
            .unwrap_err()
            .raw_os_error(),
            Some(libc::EBADF)
        );
        let handle = fs
            .open(ctx, entry.inode, false, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        assert_eq!(
            fs.setupmapping(
                ctx,
                entry.inode + 100,
                handle,
                0,
                4096,
                2,
                0,
                region.host_addr,
                4096
            )
            .unwrap_err()
            .raw_os_error(),
            Some(libc::EBADF)
        );
        assert_eq!(
            fs.setupmapping(
                ctx,
                entry.inode,
                handle,
                0,
                4096,
                3,
                0,
                region.host_addr,
                4096
            )
            .unwrap_err()
            .raw_os_error(),
            Some(libc::EROFS)
        );
        fs.setupmapping(
            ctx,
            entry.inode,
            u64::MAX,
            0,
            4096,
            2,
            0,
            region.host_addr,
            4096,
        )
        .unwrap();
        fs.release(ctx, entry.inode, 0, handle, false, false, None)
            .unwrap();
        assert_eq!(memory.read_obj::<u8>(GuestAddress(0)).unwrap(), 0x5a);
        assert_eq!(
            fs.setupmapping(
                ctx,
                entry.inode,
                handle,
                0,
                4096,
                2,
                0,
                region.host_addr,
                4096
            )
            .unwrap_err()
            .raw_os_error(),
            Some(libc::EBADF)
        );
        assert_eq!(
            fs.setupmapping(
                ctx,
                entry.inode,
                u64::MAX,
                0,
                4096,
                2,
                0,
                region.host_addr,
                4096
            )
            .unwrap_err()
            .raw_os_error(),
            Some(libc::EBADF)
        );
        assert_eq!(
            fs.capture_state().unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        fs.destroy();
        assert_eq!(memory.read_obj::<u8>(GuestAddress(0)).unwrap(), 0);
    }
}
