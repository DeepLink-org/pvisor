//! Linux statx wire layout shared by GNU and musl implementations.
//!
//! Use SYS_statx rather than C-library statx bindings: musl libc versions do
//! not consistently expose the struct, function or constants.

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct LinuxStatxTimestamp {
    pub(super) tv_sec: i64,
    pub(super) tv_nsec: u32,
    _reserved: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct LinuxStatx {
    pub(super) stx_mask: u32,
    pub(super) stx_blksize: u32,
    pub(super) stx_attributes: u64,
    pub(super) stx_nlink: u32,
    pub(super) stx_uid: u32,
    pub(super) stx_gid: u32,
    pub(super) stx_mode: u16,
    _spare0: u16,
    pub(super) stx_ino: u64,
    pub(super) stx_size: u64,
    pub(super) stx_blocks: u64,
    pub(super) stx_attributes_mask: u64,
    pub(super) stx_atime: LinuxStatxTimestamp,
    pub(super) stx_btime: LinuxStatxTimestamp,
    pub(super) stx_ctime: LinuxStatxTimestamp,
    pub(super) stx_mtime: LinuxStatxTimestamp,
    pub(super) stx_rdev_major: u32,
    pub(super) stx_rdev_minor: u32,
    pub(super) stx_dev_major: u32,
    pub(super) stx_dev_minor: u32,
    pub(super) stx_mnt_id: u64,
    pub(super) stx_dio_mem_align: u32,
    pub(super) stx_dio_offset_align: u32,
    _spare3: [u64; 12],
}

pub(super) const STATX_BASIC_STATS: u32 = 0x0000_07ff;
pub(super) const STATX_BTIME: u32 = 0x0000_0800;
pub(super) const STATX_MNT_ID: u32 = 0x0000_1000;
pub(super) const STATX_ATTR_DAX: u64 = 0x0020_0000;

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        mem::{align_of, offset_of, size_of},
        os::fd::AsRawFd,
        os::unix::fs::MetadataExt,
    };

    #[test]
    fn statx_layout_matches_linux_uapi() {
        assert_eq!(size_of::<LinuxStatxTimestamp>(), 16);
        assert_eq!(size_of::<LinuxStatx>(), 256);
        assert_eq!(align_of::<LinuxStatx>(), 8);
        assert_eq!(offset_of!(LinuxStatx, stx_ino), 32);
        assert_eq!(offset_of!(LinuxStatx, stx_btime), 80);
        assert_eq!(offset_of!(LinuxStatx, stx_mnt_id), 144);
    }

    #[test]
    fn statx_syscall_reads_a_pinned_inode_with_the_shared_layout() {
        let file = tempfile::tempfile().unwrap();
        let native = file.metadata().unwrap();
        let mut stat = LinuxStatx::default();
        // SAFETY: the descriptor, empty name and complete output remain live.
        let result = unsafe {
            libc::syscall(
                libc::SYS_statx,
                file.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
                STATX_BASIC_STATS | STATX_BTIME,
                &mut stat,
            )
        };
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
        assert_eq!(stat.stx_ino, native.ino());
        assert_eq!(stat.stx_uid, native.uid());
        assert_eq!(stat.stx_gid, native.gid());
        assert_eq!(u32::from(stat.stx_mode), native.mode());
        if stat.stx_mask & STATX_BTIME != 0 {
            assert!(stat.stx_btime.tv_sec > 0);
            assert!(stat.stx_btime.tv_nsec < 1_000_000_000);
        }
    }
}
