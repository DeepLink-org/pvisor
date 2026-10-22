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
use crate::devices::virtio::bindings;

/// An empty filesystem with just a root directory and nothing in it.
pub struct NullFs;

type Inode = u64;
type Handle = u64;

impl FileSystem for NullFs {
    fn supports_directory_image(&self) -> bool {
        true
    }

    fn capture_state(&self) -> io::Result<super::snapshot::FsSnapshot> {
        Ok(super::snapshot::FsSnapshot::Null)
    }
    fn restore_state(&self, state: &super::snapshot::FsSnapshot) -> io::Result<()> {
        if !matches!(state, super::snapshot::FsSnapshot::Null) {
            return Err(super::snapshot::invalid("null filesystem type mismatch"));
        }
        Ok(())
    }

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
    use super::super::filesystem::root_context;
    use super::*;
    use crate::devices::virtio::fs::augment_fs::AugmentFs;
    use crate::devices::virtio::fs::inode_alloc::InodeAllocator;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::Arc;

    #[test]
    fn unsupported_ioctl_is_enotty_without_breaking_guest_exit_reporting() {
        let fs = AugmentFs::new(NullFs, &InodeAllocator::new(), vec![]);
        let ctx = root_context();
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

    fn injected_init() -> AugmentFs<NullFs> {
        use super::super::virtual_entry::{VirtualDirEntry, VirtualEntry, VirtualEntryContent};
        use std::ffi::CString;

        AugmentFs::new(
            NullFs,
            &InodeAllocator::new(),
            vec![VirtualDirEntry {
                name: CString::new("init.krun").unwrap(),
                entry: VirtualEntry {
                    mode: 0o755,
                    one_shot: true,
                    content: VirtualEntryContent::File {
                        data: Arc::<[u8]>::from(&b"init"[..]),
                    },
                },
            }],
        )
    }

    #[test]
    fn listing_injected_init_does_not_consume_the_one_shot_name() {
        let fs = injected_init();
        let listed = fs.synthetic_entries(fuse::ROOT_ID);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, b"init.krun");
        assert_eq!(listed[0].1.attr.st_size, 4);
        let ctx = root_context();
        let found = fs.lookup(ctx, fuse::ROOT_ID, c"init.krun").unwrap();
        assert_eq!(found.inode, listed[0].1.inode);
        match fs.lookup(ctx, fuse::ROOT_ID, c"init.krun") {
            Err(error) => assert_eq!(error.raw_os_error(), Some(libc::ENOENT)),
            Ok(_) => panic!("one-shot name still resolves"),
        }
        assert!(fs.synthetic_entries(fuse::ROOT_ID).is_empty());
    }

    #[test]
    fn releasing_an_imported_one_shot_inode_removes_its_name() {
        let fs = injected_init();
        let listed = fs.synthetic_entries(fuse::ROOT_ID);
        let inode = listed[0].1.inode;
        let ctx = root_context();
        // Directory import may instantiate an inode without a host LOOKUP.
        let (handle, _) = fs.open(ctx, inode, false, libc::O_RDONLY as u32).unwrap();
        fs.release(ctx, inode, 0, handle.unwrap(), false, false, None)
            .unwrap();
        let super::super::snapshot::FsSnapshot::Augment { names, inodes, .. } =
            fs.capture_state().unwrap()
        else {
            panic!("wrong snapshot type")
        };
        assert!(names.is_empty());
        assert!(inodes.is_empty());
        assert!(fs.lookup(ctx, fuse::ROOT_ID, c"init.krun").is_err());
    }

    #[test]
    fn concurrent_lookups_consume_a_one_shot_name_once() {
        let fs = injected_init();
        let barrier = std::sync::Barrier::new(8);
        let successes = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        fs.lookup(
                            root_context(),
                            fuse::ROOT_ID,
                            c"init.krun",
                        )
                        .is_ok()
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|thread| usize::from(thread.join().unwrap()))
                .sum::<usize>()
        });
        assert_eq!(successes, 1);
    }
}
