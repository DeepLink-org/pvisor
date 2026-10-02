// A minimal filesystem that serves an empty root directory.
//
// Used with AugmentFs to provide a virtual-only filesystem (e.g. for
// booting from a block device where the virtiofs root only needs init.krun).

use std::ffi::CStr;
use std::io;
use std::mem;
use std::time::Duration;

use super::filesystem::{Context, Entry, FileSystem, FsOptions};
use super::fuse;
use super::virtual_entry::VIRTUAL_BLKSIZE;
use crate::virtio::bindings;

/// An empty filesystem with just a root directory and nothing in it.
pub struct NullFs;

type Inode = u64;
type Handle = u64;

impl FileSystem for NullFs {
    type Inode = Inode;
    type Handle = Handle;

    fn init(&self, _capable: FsOptions) -> io::Result<FsOptions> {
        Ok(FsOptions::empty())
    }

    fn lookup(&self, _ctx: Context, _parent: Inode, _name: &CStr) -> io::Result<Entry> {
        Err(io::Error::from_raw_os_error(libc::ENOENT))
    }

    fn getattr(
        &self,
        _ctx: Context,
        inode: Inode,
        _handle: Option<Handle>,
    ) -> io::Result<(bindings::stat64, Duration)> {
        if inode == fuse::ROOT_ID {
            let mut st: bindings::stat64 = unsafe { mem::zeroed() };
            st.st_ino = fuse::ROOT_ID;
            st.st_mode = libc::S_IFDIR | 0o755;
            st.st_nlink = 2;
            st.st_blksize = VIRTUAL_BLKSIZE as _;
            return Ok((st, Duration::MAX));
        }
        Err(io::Error::from_raw_os_error(libc::ENOENT))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::virtio::fs::augment_fs::AugmentFs;
    use crate::virtio::fs::inode_alloc::InodeAllocator;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::Arc;

    #[test]
    fn unsupported_ioctl_is_enotty_without_breaking_guest_exit_reporting() {
        let fs = AugmentFs::new(NullFs, &InodeAllocator::new(), vec![]);
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let exit = Arc::new(AtomicI32::new(-1));
        // FS_IOC_GETFLAGS: Linux OverlayFS can probe lower file attributes.
        let err = fs
            .ioctl(ctx, fuse::ROOT_ID, 0, 0, 0x80086601, 0, 0, 8, &exit)
            .unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ENOTTY));
        assert_eq!(exit.load(Ordering::SeqCst), -1);
        assert!(fs
            .ioctl(ctx, fuse::ROOT_ID, 0, 0, 0x7602, 17, 0, 0, &exit)
            .unwrap()
            .is_empty());
        assert_eq!(exit.load(Ordering::SeqCst), 17);
    }
}
