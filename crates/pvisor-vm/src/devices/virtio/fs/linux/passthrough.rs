// Copyright 2019 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::btree_map;
use std::collections::BTreeMap;
use std::convert::TryInto;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::mem::{self, size_of, MaybeUninit};
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use caps::{has_cap, CapSet, Capability};
use nix::request_code_read;

use vm_memory::ByteValued;

use super::super::filesystem::{
    Context, DirEntry, Entry, ExportTable, Extensions, FileSystem, FsOptions, GetxattrReply,
    ListxattrReply, OpenOptions, SetattrValid, ZeroCopyReader, ZeroCopyWriter,
};
use super::super::fuse;
use super::super::inode_alloc::InodeAllocator;
use super::super::multikey::MultikeyBTreeMap;

const CURRENT_DIR_CSTR: &[u8] = b".\0";
const PARENT_DIR_CSTR: &[u8] = b"..\0";
const EMPTY_CSTR: &[u8] = b"\0";
const PROC_CSTR: &[u8] = b"/proc/self/fd\0";

fn local_readonly_flush_filesystem(magic: u64) -> bool {
    // EXT2/3/4 share EXT_SUPER_MAGIC. Never infer local semantics for FUSE,
    // overlayfs, NFS, CIFS or another unknown backing from its pathname.
    matches!(magic, 0xef53 | 0x5846_5342 | 0x9123_683e | 0x0102_1994)
}

type Inode = u64;
type Handle = u64;

#[derive(Clone, Copy, PartialOrd, Ord, PartialEq, Eq)]
struct InodeAltKey {
    ino: libc::ino64_t,
    dev: libc::dev_t,
    mnt_id: u64,
}

/// Actual pinned immutable-lower descriptor identity, not a pathname cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::devices::virtio::fs) struct LowerDataIdentity {
    pub dev: u64,
    pub ino: u64,
    pub mount_id: u64,
    pub size: u64,
}

/// Evidence for one actual native OPEN, never authority for another OPEN.
#[derive(Debug, Default)]
pub(in crate::devices::virtio::fs) struct ReadonlyOpenQualification {
    pub data_identity: Option<LowerDataIdentity>,
    pub local_flush: bool,
}

pub(in crate::devices::virtio::fs) const OPEN_QUALIFICATION_COUNTERS: &[&str] = &[
    "open_qualification_fgetfl_attempts",
    "open_qualification_fstat_attempts",
    "open_qualification_statx_attempts",
    "open_qualification_fstatfs_attempts",
    "open_qualification_shared_attempts",
    "open_qualification_legacy_flush_attempts",
    "open_qualification_legacy_data_attempts",
    "open_qualification_receipt_hits",
    "open_qualification_receipt_misses",
];

struct InodeData {
    inode: Inode,
    // Most of these aren't actually files but ¯\_(ツ)_/¯.
    file: File,
    dev: u64,
    mnt_id: u64,
    refcount: AtomicU64,
}

struct HandleData {
    directory_snapshot: Option<Vec<super::super::snapshot::DirectoryEntrySnapshot>>,
    inode: Inode,
    // Non-owning alias of file's descriptor, captured before the handle is
    // shared. The File is never replaced or closed before this HandleData's
    // final Arc drops. Its lock orders I/O effects, not descriptor lifetime.
    qualification_fd: RawFd,
    file: RwLock<File>,
    exported: AtomicBool,
}

/// Pins the actual native descriptor, rather than just its reusable handle ID.
pub(in crate::devices::virtio::fs) struct HandleLease(Arc<HandleData>);

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
struct LinuxDirent64 {
    d_ino: libc::ino64_t,
    d_off: libc::off64_t,
    d_reclen: libc::c_ushort,
    d_ty: libc::c_uchar,
}
unsafe impl ByteValued for LinuxDirent64 {}

macro_rules! scoped_cred {
    ($name:ident, $ty:ty, $syscall_nr:expr_2021, $get_current:expr_2021) => {
        #[derive(Debug)]
        struct $name {
            old: $ty,
        }

        impl $name {
            // Changes the effective uid/gid of the current thread to `val`.  Changes
            // the thread's credentials back to the previous value when the returned
            // struct is dropped.
            fn new(val: $ty) -> io::Result<Option<$name>> {
                // We want credential changes to be per-thread because otherwise
                // we might interfere with operations being carried out on other
                // threads with different uids/gids.  However, posix requires that
                // all threads in a process share the same credentials.  To do this
                // libc uses signals to ensure that when one thread changes its
                // credentials the other threads do the same thing.
                //
                // So instead we invoke the syscall directly in order to get around
                // this limitation.  Another option is to use the setfsuid and
                // setfsgid systems calls.   However since those calls have no way to
                // return an error, it's preferable to do this instead.

                // Drop must restore the caller's effective id, including a
                // non-root server with CAP_SETUID/CAP_SETGID. Restoring root
                // would make the next non-root switch clear effective
                // capabilities and prevent subsequent credential restoration
                // (capabilities(7), "Effect of user ID changes on capabilities").
                let old = unsafe { $get_current() } as $ty;

                // This call is safe because it doesn't modify any memory and we
                // check the return value.
                let res = unsafe { libc::syscall($syscall_nr, -1, val, -1) };
                if res == 0 {
                    Ok(Some($name { old }))
                } else {
                    Err(io::Error::last_os_error())
                }
            }
        }

        impl Drop for $name {
            fn drop(&mut self) {
                let res = unsafe { libc::syscall($syscall_nr, -1, self.old, -1) };
                if res < 0 {
                    error!(
                        "failed to restore credentials: {}",
                        io::Error::last_os_error(),
                    );
                }
            }
        }
    };
}
scoped_cred!(ScopedUid, libc::uid_t, libc::SYS_setresuid, libc::geteuid);
scoped_cred!(ScopedGid, libc::gid_t, libc::SYS_setresgid, libc::getegid);

#[must_use]
pub struct ScopedCaps {
    cap: Capability,
}

impl ScopedCaps {
    fn new(cap: Capability) -> io::Result<Option<Self>> {
        if has_cap(None, CapSet::Effective, cap).map_err(|e| {
            error!("couldn't check {cap:?} capability: {e}");
            einval()
        })? {
            caps::drop(None, CapSet::Effective, cap).map_err(|e| {
                error!("couldn't drop {cap:?} capability: {e}");
                einval()
            })?;
            Ok(Some(Self { cap }))
        } else {
            Ok(None)
        }
    }
}

impl Drop for ScopedCaps {
    fn drop(&mut self) {
        caps::raise(None, CapSet::Effective, self.cap)
            .unwrap_or_else(|e| panic!("couldn't restore {:?} capability: {e}", self.cap));
    }
}

pub fn drop_effective_cap(cap: Capability) -> io::Result<Option<ScopedCaps>> {
    ScopedCaps::new(cap)
}

fn ebadf() -> io::Error {
    io::Error::from_raw_os_error(libc::EBADF)
}

fn einval() -> io::Error {
    io::Error::from_raw_os_error(libc::EINVAL)
}

fn stat(f: &File) -> io::Result<libc::stat64> {
    let mut st = MaybeUninit::<libc::stat64>::zeroed();

    // Safe because this is a constant value and a valid C string.
    let pathname = unsafe { CStr::from_bytes_with_nul_unchecked(EMPTY_CSTR) };

    // Safe because the kernel will only write data in `st` and we check the return
    // value.
    let res = unsafe {
        libc::fstatat64(
            f.as_raw_fd(),
            pathname.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if res >= 0 {
        // Safe because the kernel guarantees that the struct is now fully initialized.
        Ok(unsafe { st.assume_init() })
    } else {
        Err(io::Error::last_os_error())
    }
}

// Linux exposes the statx ABI through libc on glibc targets, but musl's libc
// bindings do not consistently expose it for every supported version.  Keep a
// local definition of the ABI and invoke the syscall directly so this code is
// independent of the C library's headers.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct LinuxStatxTimestamp {
    tv_sec: i64,
    tv_nsec: u32,
    _reserved: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct LinuxStatx {
    stx_mask: u32,
    stx_blksize: u32,
    stx_attributes: u64,
    stx_nlink: u32,
    stx_uid: u32,
    stx_gid: u32,
    stx_mode: u16,
    _spare0: u16,
    stx_ino: u64,
    stx_size: u64,
    stx_blocks: u64,
    stx_attributes_mask: u64,
    stx_atime: LinuxStatxTimestamp,
    stx_btime: LinuxStatxTimestamp,
    stx_ctime: LinuxStatxTimestamp,
    stx_mtime: LinuxStatxTimestamp,
    stx_rdev_major: u32,
    stx_rdev_minor: u32,
    stx_dev_major: u32,
    stx_dev_minor: u32,
    stx_mnt_id: u64,
    stx_dio_mem_align: u32,
    stx_dio_offset_align: u32,
    _spare3: [u64; 12],
}

const STATX_BASIC_STATS: u32 = 0x0000_07ff;
const STATX_MNT_ID: u32 = 0x0000_1000;
const STATX_ATTR_DAX: u64 = 0x0020_0000;

fn buffered_write_file_eligible(file: &File, exported: bool) -> io::Result<bool> {
    buffered_write_fd_eligible(file.as_raw_fd(), exported)
}

/// The caller retains the actual File owner throughout these live queries.
/// No status flags, backing attributes or eligibility decisions are cached.
fn buffered_write_fd_eligible(fd: RawFd, exported: bool) -> io::Result<bool> {
    if exported {
        return Ok(false);
    }
    // Check the pinned open description, including mutable status flags.
    // Linux pwritev still appends when O_APPEND is set on the description.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if !matches!(flags & libc::O_ACCMODE, libc::O_WRONLY | libc::O_RDWR)
        || flags & (libc::O_DIRECT | libc::O_SYNC | libc::O_DSYNC | libc::O_APPEND) != 0
    {
        return Ok(false);
    }
    let mut stx = MaybeUninit::<LinuxStatx>::zeroed();
    // SAFETY: the caller pins the FD; the empty path and output remain valid through
    // the syscall. The local statx ABI also supports musl builds.
    let result = unsafe {
        libc::syscall(
            libc::SYS_statx,
            fd,
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            STATX_BASIC_STATS,
            stx.as_mut_ptr(),
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP) => Ok(false),
            _ => Err(error),
        };
    }
    // SAFETY: statx succeeded and initialized the zeroed output.
    let stx = unsafe { stx.assume_init() };
    Ok(stx.stx_mask & 1 != 0
        && u32::from(stx.stx_mode) & libc::S_IFMT == libc::S_IFREG
        && stx.stx_attributes & STATX_ATTR_DAX == 0)
}

fn statx_evidence(f: &File) -> io::Result<(libc::stat64, u64, u32)> {
    let mut stx = MaybeUninit::<LinuxStatx>::zeroed();

    // Safe because this is a constant value and a valid C string.
    let pathname = unsafe { CStr::from_bytes_with_nul_unchecked(EMPTY_CSTR) };

    // Safe because the kernel will only write data in `stx` and we check the return
    // value.
    let res = unsafe {
        libc::syscall(
            libc::SYS_statx,
            f.as_raw_fd(),
            pathname.as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            STATX_BASIC_STATS | STATX_MNT_ID,
            stx.as_mut_ptr(),
        )
    };
    if res >= 0 {
        // Safe because the kernel guarantees that the struct is now fully initialized.
        let stx = unsafe { stx.assume_init() };

        // Unfortunately, we cannot use an initializer to create the stat64 object,
        // because it may contain padding and reserved fields (depending on the
        // architecture), and it does not implement the Default trait.
        // So we take a zeroed struct and set what we can. (Zero in all fields is
        // wrong, but safe.)
        let mut st = unsafe { MaybeUninit::<libc::stat64>::zeroed().assume_init() };

        st.st_dev = libc::makedev(stx.stx_dev_major, stx.stx_dev_minor);
        st.st_ino = stx.stx_ino;
        st.st_mode = stx.stx_mode as _;
        st.st_nlink = stx.stx_nlink as _;
        st.st_uid = stx.stx_uid;
        st.st_gid = stx.stx_gid;
        st.st_rdev = libc::makedev(stx.stx_rdev_major, stx.stx_rdev_minor);
        st.st_size = stx.stx_size as _;
        st.st_blksize = stx.stx_blksize as _;
        st.st_blocks = stx.stx_blocks as _;
        st.st_atime = stx.stx_atime.tv_sec;
        st.st_atime_nsec = stx.stx_atime.tv_nsec as _;
        st.st_mtime = stx.stx_mtime.tv_sec;
        st.st_mtime_nsec = stx.stx_mtime.tv_nsec as _;
        st.st_ctime = stx.stx_ctime.tv_sec;
        st.st_ctime_nsec = stx.stx_ctime.tv_nsec as _;
        Ok((st, stx.stx_mnt_id, stx.stx_mask))
    } else {
        Err(io::Error::last_os_error())
    }
}

fn statx(f: &File) -> io::Result<(libc::stat64, u64)> {
    statx_evidence(f).map(|(stat, mount, _)| (stat, mount))
}

fn lower_data_identity_from_evidence(
    flags: i32,
    stat: &libc::stat64,
    mount_id: u64,
    mask: u32,
) -> Option<LowerDataIdentity> {
    const REQUIRED: u32 = 0x0001 | 0x0004 | 0x0100 | 0x0200 | STATX_MNT_ID;
    (flags & libc::O_ACCMODE == libc::O_RDONLY
        && flags & (libc::O_DIRECT | libc::O_PATH) == 0
        && mask & REQUIRED == REQUIRED
        && mount_id != 0
        && stat.st_mode & libc::S_IFMT == libc::S_IFREG
        && stat.st_nlink == 1
        && stat.st_size >= 0)
        .then_some(LowerDataIdentity {
            dev: stat.st_dev,
            ino: stat.st_ino,
            mount_id,
            size: stat.st_size as u64,
        })
}

// Complete statx replaces fstat only for this already pinned descriptor.
// Incomplete/unsupported statx must not narrow FLUSH's independent contract.
fn qualify_readonly_file_evidence(
    file: &File,
    flags: i32,
    evidence: io::Result<(libc::stat64, u64, u32)>,
    want_flush: bool,
    profile: &pvisor_overlay_core::profile::Profile,
) -> ReadonlyOpenQualification {
    let data_identity = evidence.as_ref().ok().and_then(|(stat, mount, mask)| {
        lower_data_identity_from_evidence(flags, stat, *mount, *mask)
    });
    let regular = evidence
        .as_ref()
        .ok()
        .filter(|(_, _, mask)| mask & 0x0001 != 0)
        .map(|(stat, _, _)| stat.st_mode & libc::S_IFMT == libc::S_IFREG);
    let flush_regular = want_flush
        && flags & libc::O_ACCMODE == libc::O_RDONLY
        && regular.unwrap_or_else(|| {
            profile.add("open_qualification_fstat_attempts", 1);
            file.metadata().is_ok_and(|metadata| metadata.is_file())
        });
    if data_identity.is_none() && !flush_regular {
        return ReadonlyOpenQualification::default();
    }
    let mut filesystem = MaybeUninit::<libc::statfs>::zeroed();
    profile.add("open_qualification_fstatfs_attempts", 1);
    // SAFETY: caller pins the file; initialized output remains writable.
    if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } < 0 {
        return ReadonlyOpenQualification::default();
    }
    // SAFETY: successful fstatfs initialized the complete output.
    let filesystem = unsafe { filesystem.assume_init() };
    if !local_readonly_flush_filesystem(filesystem.f_type as u64 & 0xffff_ffff) {
        return ReadonlyOpenQualification::default();
    }
    ReadonlyOpenQualification {
        data_identity,
        local_flush: flush_regular,
    }
}

/// The caching policy that the file system should report to the FUSE client. By default the FUSE
/// protocol uses close-to-open consistency. This means that any cached contents of the file are
/// invalidated the next time that file is opened.
#[derive(Default, Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CachePolicy {
    /// The client should never cache file data and all I/O should be directly forwarded to the
    /// server. This policy must be selected when file contents may change without the knowledge of
    /// the FUSE client (i.e., the file system does not have exclusive access to the directory).
    Never,

    /// The client is free to choose when and how to cache file data. This is the default policy and
    /// uses close-to-open consistency as described in the enum documentation.
    #[default]
    Auto,

    /// The client should always cache file data. This means that the FUSE client will not
    /// invalidate any cached data that was returned by the file system the last time the file was
    /// opened. This policy should only be selected when the file system has exclusive access to the
    /// directory.
    Always,
}

impl FromStr for CachePolicy {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "never" | "Never" | "NEVER" => Ok(CachePolicy::Never),
            "auto" | "Auto" | "AUTO" => Ok(CachePolicy::Auto),
            "always" | "Always" | "ALWAYS" => Ok(CachePolicy::Always),
            _ => Err("invalid cache policy"),
        }
    }
}

pub use crate::api::PermissionSemantics;

/// Options that configure the behavior of the file system.
#[derive(Debug, Clone)]
pub struct Config {
    /// How long the FUSE client should consider directory entries to be valid. If the contents of a
    /// directory can only be modified by the FUSE client (i.e., the file system has exclusive
    /// access), then this should be a large value.
    ///
    /// The default value for this option is 5 seconds.
    pub entry_timeout: Duration,

    /// How long the FUSE client should consider file and directory attributes to be valid. If the
    /// attributes of a file or directory can only be modified by the FUSE client (i.e., the file
    /// system has exclusive access), then this should be set to a large value.
    ///
    /// The default value for this option is 5 seconds.
    pub attr_timeout: Duration,

    /// The caching policy the file system should use. See the documentation of `CachePolicy` for
    /// more details.
    pub cache_policy: CachePolicy,

    /// Whether the file system should enabled writeback caching. This can improve performance as it
    /// allows the FUSE client to cache and coalesce multiple writes before sending them to the file
    /// system. However, enabling this option can increase the risk of data corruption if the file
    /// contents can change without the knowledge of the FUSE client (i.e., the server does **NOT**
    /// have exclusive access). Additionally, the file system should have read access to all files
    /// in the directory it is serving as the FUSE client may send read requests even for files
    /// opened with `O_WRONLY`.
    ///
    /// Therefore callers should only enable this option when they can guarantee that: 1) the file
    /// system has exclusive access to the directory and 2) the file system has read permissions for
    /// all files in that directory.
    ///
    /// The default value for this option is `false`.
    pub writeback: bool,

    /// The path of the root directory.
    ///
    /// The default is `/`.
    pub root_dir: String,

    /// Whether the file system should support Extended Attributes (xattr). Enabling this feature may
    /// have a significant impact on performance, especially on write parallelism. This is the result
    /// of FUSE attempting to remove the special file privileges after each write request.
    ///
    /// The default value for this options is `false`.
    pub xattr: bool,

    /// Optional file descriptor for /proc/self/fd. Callers can obtain a file descriptor and pass it
    /// here, so there's no need to open it in PassthroughFs::new(). This is specially useful for
    /// sandboxing.
    ///
    /// The default is `None`.
    pub proc_sfd_rawfd: Option<RawFd>,

    /// ID of this filesystem to uniquely identify exports.
    pub export_fsid: u64,

    /// Table of exported FDs to share with other subsystems.
    pub export_table: Option<ExportTable>,

    /// The permission semantics to be emulated. See the documentation for `PermissionSemantics` for
    /// more details.
    pub semantics: PermissionSemantics,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            entry_timeout: Duration::from_secs(5),
            attr_timeout: Duration::from_secs(5),
            cache_policy: Default::default(),
            writeback: false,
            root_dir: String::from("/"),
            xattr: true,
            proc_sfd_rawfd: None,
            export_fsid: 0,
            export_table: None,
            semantics: PermissionSemantics::LinuxComplete,
        }
    }
}

/// A file system that simply "passes through" all requests it receives to the underlying file
/// system. To keep the implementation simple it servers the contents of its root directory. Users
/// that wish to serve only a specific directory should set up the environment so that that
/// directory ends up as the root of the file system process. One way to accomplish this is via a
/// combination of mount namespaces and the pivot_root system call.
pub struct PassthroughFs {
    // File descriptors for various points in the file system tree. These fds are always opened with
    // the `O_PATH` option so they cannot be used for reading or writing any data. See the
    // documentation of the `O_PATH` flag in `open(2)` for more details on what one can and cannot
    // do with an fd opened with this flag.
    inodes: RwLock<MultikeyBTreeMap<Inode, InodeAltKey, Arc<InodeData>>>,
    inode_alloc: Arc<InodeAllocator>,

    // File descriptors for open files and directories. Unlike the fds in `inodes`, these _can_ be
    // used for reading and writing data.
    handles: RwLock<BTreeMap<Handle, Arc<HandleData>>>,
    next_handle: AtomicU64,

    // File descriptor pointing to the `/proc/self/fd` directory. This is used to convert an fd from
    // `inodes` into one that can go into `handles`. This is accomplished by reading the
    // `/proc/self/fd/{}` symlink. We keep an open fd here in case the file system tree that we are
    // meant to be serving doesn't have access to `/proc/self/fd`.
    proc_self_fd: File,

    // Whether writeback caching is enabled for this directory. This will only be true when
    // `cfg.writeback` is true and `init` was called with `FsOptions::WRITEBACK_CACHE`.
    writeback: AtomicBool,
    announce_submounts: AtomicBool,
    my_uid: Option<libc::uid_t>,
    my_gid: Option<libc::gid_t>,
    cap_fowner: bool,

    cfg: Config,
}

/// Some operations can only be performed on opened FDs without O_PATH, or on symlink paths.
/// This enum encodes a fallback to handle those symlinks separately.
enum FileOrLink {
    File(File),
    Link(CString),
}

impl PassthroughFs {
    /// Read one regular-file child's attributes from its retained native parent.
    /// This reply-only path does not allocate an inode or a leaf descriptor.
    /// Non-regular children retain the ordinary lookup/submount path.
    pub(in crate::devices::virtio::fs) fn regular_lookup_attributes(
        &self,
        parent: Inode,
        name: &CStr,
    ) -> io::Result<Option<(libc::stat64, Duration, Duration)>> {
        // Only a single validated name is allowed: fstatat must not follow an
        // intermediate symlink which the ordinary component walk would reject.
        if name.to_bytes().is_empty()
            || name.to_bytes().contains(&b'/')
            || matches!(name.to_bytes(), b"." | b"..")
        {
            return Err(einval());
        }
        let parent = self
            .inodes
            .read()
            .unwrap()
            .get(&parent)
            .cloned()
            .ok_or_else(ebadf)?;
        let mut attr = MaybeUninit::<libc::stat64>::zeroed();
        // SAFETY: the Arc owns the directory FD throughout the syscall, and
        // both the NUL-terminated single name and output remain valid. The
        // no-follow query preserves the inode bound by ROOT_ID/parent lookup.
        let result = unsafe {
            libc::fstatat64(
                parent.file.as_raw_fd(),
                name.as_ptr(),
                attr.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful fstatat initialized the complete output.
        let attr = unsafe { attr.assume_init() };
        Ok((attr.st_mode & libc::S_IFMT == libc::S_IFREG).then_some((
            attr,
            self.cfg.attr_timeout,
            self.cfg.entry_timeout,
        )))
    }

    pub(in crate::devices::virtio::fs) fn with_dax_file(
        &self,
        inode: Inode,
        handle: Handle,
        map: impl FnOnce(&File) -> io::Result<()>,
    ) -> io::Result<()> {
        // Linux SETUPMAPPING supplies fh = -1: DAX ranges belong to an inode,
        // rather than a particular open description. Borrow an already opened
        // descriptor, so mapping never reopens a path or grants new authority.
        // The Arc pins it if RELEASE races with this request.
        let lease = if handle == u64::MAX {
            self.handles
                .read()
                .unwrap()
                .values()
                .find(|data| data.inode == inode)
                .cloned()
                .map(HandleLease)
                .ok_or_else(ebadf)?
        } else {
            self.lease_handle(inode, handle)?
        };
        let file = lease.0.file.read().unwrap();
        map(&file)
    }
    pub(in crate::devices::virtio::fs) fn lease_handle(
        &self,
        inode: Inode,
        handle: Handle,
    ) -> io::Result<HandleLease> {
        self.handles
            .read()
            .unwrap()
            .get(&handle)
            .filter(|data| data.inode == inode)
            .cloned()
            .map(HandleLease)
            .ok_or_else(ebadf)
    }

    pub(in crate::devices::virtio::fs) fn read_leased<W: io::Write + ZeroCopyWriter>(
        &self,
        lease: &HandleLease,
        mut writer: W,
        size: u32,
        offset: u64,
    ) -> io::Result<usize> {
        let file = lease.0.file.read().unwrap();
        writer.write_from(&file, size as usize, offset)
    }

    pub(in crate::devices::virtio::fs) fn flush_leased(
        &self,
        lease: &HandleLease,
    ) -> io::Result<()> {
        // Serialize descriptor effects against writes and sync; File's
        // shared-reference methods do not imply shared operation semantics.
        #[allow(clippy::readonly_write_lock)]
        let file = lease.0.file.write().unwrap();
        // Match FUSE FLUSH's existing dup/close behavior while keeping this
        // descriptor's lock and ownership through both syscalls.
        let duplicate = unsafe { libc::dup(file.as_raw_fd()) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::close(duplicate) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Classify this actual pinned open description, not a cached pathname.
    /// Unknown/remote filesystems and failed evidence retain ordinary FLUSH.
    pub(in crate::devices::virtio::fs) fn readonly_local_flush_eligible_profiled(
        &self,
        inode: Inode,
        handle: Handle,
        profile: &pvisor_overlay_core::profile::Profile,
    ) -> bool {
        profile.add("open_qualification_legacy_flush_attempts", 1);
        let Ok(lease) = self.lease_handle(inode, handle) else {
            return false;
        };
        let file = lease.0.file.read().unwrap();
        profile.add("open_qualification_fstat_attempts", 1);
        if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
            return false;
        }
        // SAFETY: the lease and read guard pin this descriptor throughout all
        // evidence queries. No descriptor flags or file contents are changed.
        profile.add("open_qualification_fgetfl_attempts", 1);
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 || flags & libc::O_ACCMODE != libc::O_RDONLY {
            return false;
        }
        let mut filesystem = MaybeUninit::<libc::statfs>::zeroed();
        // SAFETY: the output is writable and the descriptor remains pinned.
        profile.add("open_qualification_fstatfs_attempts", 1);
        if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } < 0 {
            return false;
        }
        // SAFETY: successful fstatfs initialized the complete output.
        let filesystem = unsafe { filesystem.assume_init() };
        local_readonly_flush_filesystem(filesystem.f_type as u64 & 0xffff_ffff)
    }

    /// Gather identity and EOF from the actual successful native OPEN. This
    /// optional optimization rejects incomplete evidence without failing OPEN.
    pub(in crate::devices::virtio::fs) fn lower_data_identity_profiled(
        &self,
        inode: Inode,
        handle: Handle,
        profile: &pvisor_overlay_core::profile::Profile,
    ) -> Option<LowerDataIdentity> {
        profile.add("open_qualification_legacy_data_attempts", 1);
        let lease = self.lease_handle(inode, handle).ok()?;
        let file = lease.0.file.read().unwrap();
        // SAFETY: the Arc lease and read guard pin the open description.
        profile.add("open_qualification_fgetfl_attempts", 1);
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || flags & libc::O_ACCMODE != libc::O_RDONLY
            || flags & (libc::O_DIRECT | libc::O_PATH) != 0
        {
            return None;
        }
        profile.add("open_qualification_fstat_attempts", 1);
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return None;
        }
        profile.add("open_qualification_statx_attempts", 1);
        let (stat, mount_id, mask) = statx_evidence(&file).ok()?;
        const REQUIRED: u32 = 0x0001 | 0x0004 | 0x0100 | 0x0200 | STATX_MNT_ID;
        if mask & REQUIRED != REQUIRED
            || mount_id == 0
            || stat.st_size < 0
            || stat.st_dev != metadata.dev()
            || stat.st_ino != metadata.ino()
            || stat.st_size as u64 != metadata.len()
            || stat.st_nlink != 1
        {
            return None;
        }
        let mut filesystem = MaybeUninit::<libc::statfs>::zeroed();
        // SAFETY: initialized output storage; the descriptor remains pinned.
        profile.add("open_qualification_fstatfs_attempts", 1);
        if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } < 0 {
            return None;
        }
        // SAFETY: successful fstatfs initialized this structure.
        let filesystem = unsafe { filesystem.assume_init() };
        if !local_readonly_flush_filesystem(filesystem.f_type as u64 & 0xffff_ffff) {
            return None;
        }
        Some(LowerDataIdentity {
            dev: metadata.dev(),
            ino: metadata.ino(),
            mount_id,
            size: metadata.len(),
        })
    }

    /// Share actual-FD evidence within this OPEN only. DATA requires complete
    /// statx identity; FLUSH can retain its broader regular-file qualification
    /// even when DATA declines. The caller still owns all outer admission.
    pub(in crate::devices::virtio::fs) fn qualify_readonly_open(
        &self,
        inode: Inode,
        handle: Handle,
        want_flush: bool,
        profile: &pvisor_overlay_core::profile::Profile,
    ) -> ReadonlyOpenQualification {
        profile.add("open_qualification_shared_attempts", 1);
        let Ok(lease) = self.lease_handle(inode, handle) else {
            return ReadonlyOpenQualification::default();
        };
        let file = lease.0.file.read().unwrap();
        profile.add("open_qualification_fgetfl_attempts", 1);
        // SAFETY: the lease/read guard pins this actual open description.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 || flags & libc::O_ACCMODE != libc::O_RDONLY {
            return ReadonlyOpenQualification::default();
        }
        profile.add("open_qualification_statx_attempts", 1);
        let evidence = statx_evidence(&file);
        qualify_readonly_file_evidence(&file, flags, evidence, want_flush, profile)
    }

    #[cfg(test)]
    pub(in crate::devices::virtio::fs) fn readonly_local_flush_eligible(
        &self,
        inode: Inode,
        handle: Handle,
    ) -> bool {
        self.readonly_local_flush_eligible_profiled(inode, handle, &Default::default())
    }

    #[cfg(test)]
    pub(in crate::devices::virtio::fs) fn lower_data_identity(
        &self,
        inode: Inode,
        handle: Handle,
    ) -> Option<LowerDataIdentity> {
        self.lower_data_identity_profiled(inode, handle, &Default::default())
    }

    pub(in crate::devices::virtio::fs) fn sync_leased(
        &self,
        lease: &HandleLease,
        datasync: bool,
    ) -> io::Result<()> {
        // Serialize descriptor effects against writes and sync; File's
        // shared-reference methods do not imply shared operation semantics.
        #[allow(clippy::readonly_write_lock)]
        let file = lease.0.file.write().unwrap();
        let result = unsafe {
            if datasync {
                libc::fdatasync(file.as_raw_fd())
            } else {
                libc::fsync(file.as_raw_fd())
            }
        };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub(in crate::devices::virtio::fs) fn write_leased<R: io::Read + ZeroCopyReader>(
        &self,
        _ctx: Context,
        lease: &HandleLease,
        mut reader: R,
        size: u32,
        offset: u64,
        kill_priv: bool,
    ) -> io::Result<usize> {
        let _killpriv_guard = if kill_priv {
            drop_effective_cap(Capability::CAP_FSETID)?
        } else {
            None
        };
        // Serialize writes and synchronization on this opened descriptor only.
        // Serialize descriptor effects against writes and sync; File's
        // shared-reference methods do not imply shared operation semantics.
        #[allow(clippy::readonly_write_lock)]
        let file = lease.0.file.write().unwrap();
        reader.read_to(&file, size as usize, offset)
    }

    /// Qualify this pinned native FD for asynchronous positional writes.
    /// Callers must separately exclude guest DAX mappings and order exports.
    pub(in crate::devices::virtio::fs) fn buffered_write_eligible(
        &self,
        lease: &HandleLease,
    ) -> io::Result<bool> {
        let file = lease.0.file.read().unwrap();
        buffered_write_file_eligible(&file, lease.0.exported.load(Ordering::Relaxed))
    }

    /// Query the same live qualification without waiting for native I/O.
    /// The lease's Arc retains the never-replaced File behind qualification_fd;
    /// no File reference is created or borrowed through the I/O lock. Native
    /// flags, exported status and statx attributes are still queried each time.
    /// Callers must separately exclude guest DAX mappings and order exports.
    pub(in crate::devices::virtio::fs) fn buffered_write_eligible_unlocked(
        &self,
        lease: &HandleLease,
    ) -> io::Result<bool> {
        buffered_write_fd_eligible(
            lease.0.qualification_fd,
            lease.0.exported.load(Ordering::Relaxed),
        )
    }

    /// Submit up to 64 host-owned contiguous chunks in one positional syscall.
    /// Returns the actual count, including a successful short write; the owner
    /// must retain/report its unwritten suffix. EINTR retries have no progress.
    pub(in crate::devices::virtio::fs) fn write_buffered_leased(
        &self,
        lease: &HandleLease,
        chunks: &[&[u8]],
        offset: u64,
        kill_priv: bool,
    ) -> io::Result<usize> {
        let length = chunks.iter().try_fold(0usize, |length, chunk| {
            length.checked_add(chunk.len()).ok_or_else(einval)
        })?;
        if chunks.len() > 64
            || length > isize::MAX as usize
            || offset > i64::MAX as u64
            || offset
                .checked_add(length as u64)
                .is_none_or(|end| end > i64::MAX as u64)
        {
            return Err(einval());
        }
        let _killpriv_guard = if kill_priv {
            drop_effective_cap(Capability::CAP_FSETID)?
        } else {
            None
        };
        #[allow(clippy::readonly_write_lock)]
        let file = lease.0.file.write().unwrap();
        if !buffered_write_file_eligible(&file, lease.0.exported.load(Ordering::Relaxed))? {
            return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP));
        }
        let mut vectors: [libc::iovec; 64] = std::array::from_fn(|_| libc::iovec {
            iov_base: std::ptr::null_mut(),
            iov_len: 0,
        });
        for (vector, chunk) in vectors.iter_mut().zip(chunks) {
            vector.iov_base = chunk.as_ptr().cast_mut().cast();
            vector.iov_len = chunk.len();
        }
        loop {
            // SAFETY: the native write lock pins the FD; every immutable
            // host-owned chunk and the bounded iovec array outlive pwritev.
            let result = unsafe {
                libc::pwritev(
                    file.as_raw_fd(),
                    vectors.as_ptr(),
                    chunks.len() as libc::c_int,
                    offset as libc::off_t,
                )
            };
            if result >= 0 {
                return Ok(result as usize);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    pub fn new(cfg: Config, inode_alloc: Arc<InodeAllocator>) -> io::Result<PassthroughFs> {
        let fd = if let Some(fd) = cfg.proc_sfd_rawfd {
            fd
        } else {
            // Safe because this is a constant value and a valid C string.
            let proc_cstr = unsafe { CStr::from_bytes_with_nul_unchecked(PROC_CSTR) };

            // Safe because this doesn't modify any memory and we check the return value.
            let fd = unsafe {
                libc::openat(
                    libc::AT_FDCWD,
                    proc_cstr.as_ptr(),
                    libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }

            fd
        };

        let my_uid = if has_cap(None, CapSet::Effective, Capability::CAP_SETUID).unwrap_or_default()
        {
            None
        } else {
            // SAFETY: This syscall is always safe to call and always succeeds.
            Some(unsafe { libc::getuid() })
        };

        let my_gid = if has_cap(None, CapSet::Effective, Capability::CAP_SETGID).unwrap_or_default()
        {
            None
        } else {
            // SAFETY: This syscall is always safe to call and always succeeds.
            Some(unsafe { libc::getgid() })
        };

        let cap_fowner =
            has_cap(None, CapSet::Effective, Capability::CAP_FOWNER).unwrap_or_default();

        // Safe because we just opened this fd or it was provided by our caller.
        let proc_self_fd = unsafe { File::from_raw_fd(fd) };

        Ok(PassthroughFs {
            inodes: RwLock::new(MultikeyBTreeMap::new()),
            inode_alloc,

            handles: RwLock::new(BTreeMap::new()),
            next_handle: AtomicU64::new(1),

            proc_self_fd,

            writeback: AtomicBool::new(false),
            announce_submounts: AtomicBool::new(false),
            my_uid,
            my_gid,
            cap_fowner,
            cfg,
        })
    }

    fn open_inode(&self, inode: Inode, mut flags: i32) -> io::Result<File> {
        let data = self
            .inodes
            .read()
            .unwrap()
            .get(&inode)
            .cloned()
            .ok_or_else(ebadf)?;

        let pathname = CString::new(format!("{}", data.file.as_raw_fd()))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // When writeback caching is enabled, the kernel may send read requests even if the
        // userspace program opened the file write-only. So we need to ensure that we have opened
        // the file for reading as well as writing.
        let writeback = self.writeback.load(Ordering::Relaxed);
        if writeback && flags & libc::O_ACCMODE == libc::O_WRONLY {
            flags &= !libc::O_ACCMODE;
            flags |= libc::O_RDWR;
        }

        // With writeback caching, the guest kernel handles O_APPEND, so clear
        // it on the host descriptor. External writes can invalidate the cached
        // end-of-file offset and break append atomicity; callers must enable
        // writeback only for directories not shared with other writers.
        if writeback && flags & libc::O_APPEND != 0 {
            flags &= !libc::O_APPEND;
        }

        // Safe because this doesn't modify any memory and we check the return value. We don't
        // really check `flags` because if the kernel can't handle poorly specified flags then we
        // have much bigger problems. Also, clear the `O_NOFOLLOW` flag if it is set since we need
        // to follow the `/proc/self/fd` symlink to get the file.
        let fd = unsafe {
            libc::openat(
                self.proc_self_fd.as_raw_fd(),
                pathname.as_ptr(),
                (flags | libc::O_CLOEXEC) & (!libc::O_NOFOLLOW),
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }

        // Safe because we just opened this fd.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn open_inode_or_path(&self, inode: Inode, flags: i32) -> io::Result<FileOrLink> {
        match self.open_inode(inode, flags) {
            Ok(a) => Ok(FileOrLink::File(a)),
            Err(e) => {
                if e.raw_os_error() == Some(libc::ELOOP) {
                    let data = self
                        .inodes
                        .read()
                        .unwrap()
                        .get(&inode)
                        .cloned()
                        .ok_or_else(ebadf)?;

                    let pathname = CString::new(format!("/proc/self/fd/{}", data.file.as_raw_fd()))
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    Ok(FileOrLink::Link(pathname))
                } else {
                    Err(e)
                }
            }
        }
    }

    fn do_readdir<F>(
        &self,
        inode: Inode,
        handle: Handle,
        size: u32,
        offset: u64,
        mut add_entry: F,
    ) -> io::Result<()>
    where
        F: FnMut(DirEntry) -> io::Result<usize>,
    {
        if size == 0 {
            return Ok(());
        }

        let data = self
            .handles
            .read()
            .unwrap()
            .get(&handle)
            .filter(|hd| hd.inode == inode)
            .cloned()
            .ok_or_else(ebadf)?;

        if let Some(entries) = &data.directory_snapshot {
            let start = if offset == 0 {
                0
            } else {
                entries
                    .iter()
                    .position(|e| e.offset == offset)
                    .ok_or_else(einval)?
                    + 1
            };
            for entry in &entries[start..] {
                if add_entry(DirEntry {
                    ino: entry.ino,
                    offset: entry.offset,
                    type_: entry.type_ as u32,
                    name: &entry.name,
                })? == 0
                {
                    break;
                }
            }
            return Ok(());
        }
        let mut buf = vec![0; size as usize];

        {
            // Since we are going to work with the kernel offset, we have to acquire the file lock
            // for both the `lseek64` and `getdents64` syscalls to ensure that no other thread
            // changes the kernel offset while we are using it.
            let dir = data.file.write().unwrap();

            // Safe because this doesn't modify any memory and we check the return value.
            let res =
                unsafe { libc::lseek64(dir.as_raw_fd(), offset as libc::off64_t, libc::SEEK_SET) };
            if res < 0 {
                return Err(io::Error::last_os_error());
            }

            // Safe because the kernel guarantees that it will only write to `buf` and we check the
            // return value.
            let res = unsafe {
                libc::syscall(
                    libc::SYS_getdents64,
                    dir.as_raw_fd(),
                    buf.as_mut_ptr() as *mut LinuxDirent64,
                    size as libc::c_int,
                )
            };
            if res < 0 {
                return Err(io::Error::last_os_error());
            }
            buf.resize(res as usize, 0);

            // Explicitly drop the lock so that it's not held while we fill in the fuse buffer.
            mem::drop(dir);
        }

        let mut rem = &buf[..];
        while !rem.is_empty() {
            // We only use debug asserts here because these values are coming from the kernel and we
            // trust them implicitly.
            debug_assert!(
                rem.len() >= size_of::<LinuxDirent64>(),
                "not enough space left in `rem`"
            );

            let (front, back) = rem.split_at(size_of::<LinuxDirent64>());

            let dirent64 =
                LinuxDirent64::from_slice(front).expect("unable to get LinuxDirent64 from slice");

            let namelen = dirent64.d_reclen as usize - size_of::<LinuxDirent64>();
            debug_assert!(namelen <= back.len(), "back is smaller than `namelen`");

            let name = &back[..namelen];
            let term = name
                .iter()
                .position(|&a| a == 0)
                .expect("LinuxDirent64 name not NUL-terminated");
            let name = &name[..term];
            let res = if name.starts_with(CURRENT_DIR_CSTR) || name.starts_with(PARENT_DIR_CSTR) {
                // We don't want to report the "." and ".." entries. However, returning `Ok(0)` will
                // break the loop so return `Ok` with a non-zero value instead.
                Ok(1)
            } else {
                add_entry(DirEntry {
                    ino: dirent64.d_ino,
                    offset: dirent64.d_off as u64,
                    type_: u32::from(dirent64.d_ty),
                    name,
                })
            };

            debug_assert!(
                rem.len() >= dirent64.d_reclen as usize,
                "rem is smaller than `d_reclen`"
            );

            match res {
                Ok(0) => break,
                Ok(_) => rem = &rem[dirent64.d_reclen as usize..],
                Err(e) => return Err(e),
            }
        }

        Ok(())
    }

    fn do_open(
        &self,
        inode: Inode,
        kill_priv: bool,
        mut flags: u32,
    ) -> io::Result<(Option<Handle>, OpenOptions)> {
        debug!("do_open: {inode:?}");
        if !self.cap_fowner {
            // O_NOATIME can only be used with CAP_FOWNER or if we are the file
            // owner. Not worth checking the latter, just drop it if we don't
            // have the cap. This makes overlayfs mounts with virtiofs lower dirs
            // work.
            flags &= !(libc::O_NOATIME as u32);
        }

        let file = {
            let _killpriv_guard = if kill_priv {
                drop_effective_cap(Capability::CAP_FSETID)?
            } else {
                None
            };
            self.open_inode(inode, flags as i32)?
        };

        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        let data = HandleData {
            inode,
            qualification_fd: file.as_raw_fd(),
            file: RwLock::new(file),
            exported: Default::default(),
            directory_snapshot: None,
        };

        self.handles.write().unwrap().insert(handle, Arc::new(data));

        let mut opts = OpenOptions::empty();
        match self.cfg.cache_policy {
            // We only set the direct I/O option on files.
            CachePolicy::Never => opts.set(
                OpenOptions::DIRECT_IO,
                flags & (libc::O_DIRECTORY as u32) == 0,
            ),
            CachePolicy::Always => {
                if flags & (libc::O_DIRECTORY as u32) == 0 {
                    opts |= OpenOptions::KEEP_CACHE;
                } else {
                    opts |= OpenOptions::CACHE_DIR;
                }
            }
            _ => {}
        };

        Ok((Some(handle), opts))
    }

    fn do_release(&self, inode: Inode, handle: Handle) -> io::Result<()> {
        let mut handles = self.handles.write().unwrap();

        if let btree_map::Entry::Occupied(e) = handles.entry(handle) {
            if e.get().inode == inode {
                if e.get().exported.load(Ordering::Relaxed) {
                    self.cfg
                        .export_table
                        .as_ref()
                        .unwrap()
                        .lock()
                        .unwrap()
                        .remove(&(self.cfg.export_fsid, handle));
                }

                // We don't need to close the file here because that will happen automatically when
                // the last `Arc` is dropped.
                e.remove();
                return Ok(());
            }
        }

        Err(ebadf())
    }

    fn do_getattr(&self, inode: Inode) -> io::Result<(libc::stat64, Duration)> {
        let data = self
            .inodes
            .read()
            .unwrap()
            .get(&inode)
            .cloned()
            .ok_or_else(ebadf)?;

        let st = stat(&data.file)?;

        Ok((st, self.cfg.attr_timeout))
    }

    fn do_unlink(&self, parent: Inode, name: &CStr, flags: libc::c_int) -> io::Result<()> {
        let data = self
            .inodes
            .read()
            .unwrap()
            .get(&parent)
            .cloned()
            .ok_or_else(ebadf)?;

        // Safe because this doesn't modify any memory and we check the return value.
        let res = unsafe { libc::unlinkat(data.file.as_raw_fd(), name.as_ptr(), flags) };
        if res == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn set_creds(
        &self,
        uid: libc::uid_t,
        gid: libc::gid_t,
    ) -> io::Result<(Option<ScopedUid>, Option<ScopedGid>)> {
        // Change the gid first, since once we change the uid we lose the capability to change the gid.
        let scoped_gid = if gid == 0 || self.my_gid == Some(gid) {
            // Always allow "root" accesses even if we don't have root powers.
            // This means guest processes running as root can use /tmp (though
            // the files will not be actually owned by root), which is desirable.
            None
        } else if self.my_gid.is_some() {
            // Reject writes as any other gid if we do not have setgid
            // privileges.
            return Err(io::Error::from_raw_os_error(libc::EPERM));
        } else {
            ScopedGid::new(gid)?
        };

        // Same logic as above, for uid.
        let scoped_uid = if uid == 0 || self.my_uid == Some(uid) {
            None
        } else if self.my_uid.is_some() {
            return Err(io::Error::from_raw_os_error(libc::EPERM));
        } else {
            ScopedUid::new(uid)?
        };

        Ok((scoped_uid, scoped_gid))
    }
}

fn forget_one(
    inodes: &mut MultikeyBTreeMap<Inode, InodeAltKey, Arc<InodeData>>,
    inode: Inode,
    count: u64,
) {
    if let Some(data) = inodes.get(&inode) {
        // Acquiring the write lock on the inode map prevents new lookups from incrementing the
        // refcount but there is the possibility that a previous lookup already acquired a
        // reference to the inode data and is in the process of updating the refcount so we need
        // to loop here until we can decrement successfully.
        loop {
            let refcount = data.refcount.load(Ordering::Relaxed);

            // Saturating sub because it doesn't make sense for a refcount to go below zero and
            // we don't want misbehaving clients to cause integer overflow.
            let new_count = refcount.saturating_sub(count);

            // Synchronizes with the acquire load in `lookup`.
            if data
                .refcount
                .compare_exchange(refcount, new_count, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                if new_count == 0 {
                    // We just removed the last refcount for this inode. There's no need for an
                    // acquire fence here because we hold a write lock on the inode map and any
                    // thread that is waiting to do a forget on the same inode will have to wait
                    // until we release the lock. So there's is no other release store for us to
                    // synchronize with before deleting the entry.
                    inodes.remove(&inode);
                }
                break;
            }
        }
    }
}

#[path = "passthrough_snapshot.rs"]
mod snapshot_state;
impl FileSystem for PassthroughFs {
    fn capture_state(&self) -> io::Result<super::super::snapshot::FsSnapshot> {
        snapshot_state::capture(self)
    }
    fn restore_state(&self, state: &super::super::snapshot::FsSnapshot) -> io::Result<()> {
        snapshot_state::restore(self, state)
    }

    type Inode = Inode;
    type Handle = Handle;

    fn init(&self, capable: FsOptions) -> io::Result<FsOptions> {
        let root = CString::new(self.cfg.root_dir.as_str()).expect("CString::new failed");

        // Safe because this doesn't modify any memory and we check the return value.
        // We use `O_PATH` because we just want this for traversing the directory tree
        // and not for actually reading the contents.
        let fd = unsafe {
            libc::openat(
                libc::AT_FDCWD,
                root.as_ptr(),
                libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }

        // Safe because we just opened this fd above.
        let f = unsafe { File::from_raw_fd(fd) };

        let (st, mnt_id) = statx(&f)?;

        // Safe because this doesn't modify any memory and there is no need to check the return
        // value because this system call always succeeds. We need to clear the umask here because
        // we want the client to be able to set all the bits in the mode.
        unsafe { libc::umask(0o000) };

        let mut inodes = self.inodes.write().unwrap();

        // Not sure why the root inode gets a refcount of 2 but that's what libfuse does.
        inodes.insert(
            fuse::ROOT_ID,
            InodeAltKey {
                ino: st.st_ino,
                dev: st.st_dev,
                mnt_id,
            },
            Arc::new(InodeData {
                inode: fuse::ROOT_ID,
                file: f,
                dev: st.st_dev,
                mnt_id,
                refcount: AtomicU64::new(2),
            }),
        );

        let mut opts = FsOptions::DO_READDIRPLUS | FsOptions::READDIRPLUS_AUTO;
        let writeback = self.cfg.writeback && capable.contains(FsOptions::WRITEBACK_CACHE);
        self.writeback.store(writeback, Ordering::Relaxed);
        if writeback {
            opts |= FsOptions::WRITEBACK_CACHE;
        }

        if capable.contains(FsOptions::SUBMOUNTS) {
            opts |= FsOptions::SUBMOUNTS;
            self.announce_submounts.store(true, Ordering::Relaxed);
        }

        Ok(opts)
    }

    fn destroy(&self) {
        self.handles.write().unwrap().clear();
        self.inodes.write().unwrap().clear();
    }

    fn statfs(&self, _ctx: Context, inode: Inode) -> io::Result<libc::statvfs64> {
        let data = self
            .inodes
            .read()
            .unwrap()
            .get(&inode)
            .cloned()
            .ok_or_else(ebadf)?;

        let mut out = MaybeUninit::<libc::statvfs64>::zeroed();

        // Safe because this will only modify `out` and we check the return value.
        let res = unsafe { libc::fstatvfs64(data.file.as_raw_fd(), out.as_mut_ptr()) };
        if res == 0 {
            // Safe because the kernel guarantees that `out` has been initialized.
            Ok(unsafe { out.assume_init() })
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn lookup(&self, _ctx: Context, parent: Inode, name: &CStr) -> io::Result<Entry> {
        let p = self
            .inodes
            .read()
            .unwrap()
            .get(&parent)
            .cloned()
            .ok_or_else(ebadf)?;

        // Safe because this doesn't modify any memory and we check the return value.
        let fd = unsafe {
            libc::openat(
                p.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }

        // Safe because we just opened this fd.
        let f = unsafe { File::from_raw_fd(fd) };

        let (st, mnt_id) = statx(&f)?;

        let mut attr_flags: u32 = 0;

        if st.st_mode & libc::S_IFMT == libc::S_IFDIR
            && self.announce_submounts.load(Ordering::Relaxed)
            && (st.st_dev != p.dev || mnt_id != p.mnt_id)
        {
            attr_flags |= fuse::ATTR_SUBMOUNT;
        }

        let altkey = InodeAltKey {
            ino: st.st_ino,
            dev: st.st_dev,
            mnt_id,
        };
        // openat/statx above run without the inode table lock. Lookup/pin and
        // insertion are one table transaction: no duplicate alternate key can
        // replace an inode which another request has already received.
        let mut inodes = self.inodes.write().unwrap();
        let inode = if let Some(data) = inodes.get_alt(&altkey) {
            data.refcount.fetch_add(1, Ordering::Acquire);
            data.inode
        } else {
            let inode = self.inode_alloc.next();
            inodes.insert(
                inode,
                altkey,
                Arc::new(InodeData {
                    inode,
                    file: f,
                    dev: st.st_dev,
                    mnt_id,
                    refcount: AtomicU64::new(1),
                }),
            );
            inode
        };
        drop(inodes);

        Ok(Entry {
            inode,
            generation: 0,
            attr: st,
            attr_flags,
            attr_timeout: self.cfg.attr_timeout,
            entry_timeout: self.cfg.entry_timeout,
        })
    }

    fn forget(&self, _ctx: Context, inode: Inode, count: u64) {
        let mut inodes = self.inodes.write().unwrap();

        forget_one(&mut inodes, inode, count)
    }

    fn batch_forget(&self, _ctx: Context, requests: Vec<(Inode, u64)>) {
        let mut inodes = self.inodes.write().unwrap();

        for (inode, count) in requests {
            forget_one(&mut inodes, inode, count)
        }
    }

    fn opendir(
        &self,
        _ctx: Context,
        inode: Inode,
        flags: u32,
    ) -> io::Result<(Option<Handle>, OpenOptions)> {
        self.do_open(inode, false, flags | (libc::O_DIRECTORY as u32))
    }

    fn releasedir(
        &self,
        _ctx: Context,
        inode: Inode,
        _flags: u32,
        handle: Handle,
    ) -> io::Result<()> {
        self.do_release(inode, handle)
    }

    fn mkdir(
        &self,
        ctx: Context,
        parent: Inode,
        name: &CStr,
        mode: u32,
        umask: u32,
        extensions: Extensions,
    ) -> io::Result<Entry> {
        if extensions.secctx.is_some() {
            unimplemented!("SECURITY_CTX is not supported and should not be used by the guest");
        }

        let (_uid, _gid) = self.set_creds(ctx.uid, ctx.gid)?;
        let data = self
            .inodes
            .read()
            .unwrap()
            .get(&parent)
            .cloned()
            .ok_or_else(ebadf)?;

        // Safe because this doesn't modify any memory and we check the return value.
        let res = unsafe { libc::mkdirat(data.file.as_raw_fd(), name.as_ptr(), mode & !umask) };
        if res == 0 {
            self.lookup(ctx, parent, name)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn rmdir(&self, _ctx: Context, parent: Inode, name: &CStr) -> io::Result<()> {
        self.do_unlink(parent, name, libc::AT_REMOVEDIR)
    }

    fn readdir<F>(
        &self,
        _ctx: Context,
        inode: Inode,
        handle: Handle,
        size: u32,
        offset: u64,
        add_entry: F,
    ) -> io::Result<()>
    where
        F: FnMut(DirEntry) -> io::Result<usize>,
    {
        self.do_readdir(inode, handle, size, offset, add_entry)
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
        self.do_readdir(inode, handle, size, offset, |dir_entry| {
            // Safe because the kernel guarantees that the buffer is nul-terminated. Additionally,
            // the kernel will pad the name with '\0' bytes up to 8-byte alignment and there's no
            // way for us to know exactly how many padding bytes there are. This would cause
            // `CStr::from_bytes_with_nul` to return an error because it would think there are
            // interior '\0' bytes. We trust the kernel to provide us with properly formatted data
            // so we'll just skip the checks here.
            let name = unsafe { CStr::from_bytes_with_nul_unchecked(dir_entry.name) };
            let entry = self.lookup(ctx, inode, name)?;
            let lookup_inode = entry.inode;
            let result = add_entry(dir_entry, entry);
            if !matches!(&result, Ok(written) if *written != 0) {
                self.forget(ctx, lookup_inode, 1);
            }
            result
        })
    }

    fn open(
        &self,
        _ctx: Context,
        inode: Inode,
        kill_priv: bool,
        flags: u32,
    ) -> io::Result<(Option<Handle>, OpenOptions)> {
        self.do_open(inode, kill_priv, flags)
    }

    fn release(
        &self,
        _ctx: Context,
        inode: Inode,
        _flags: u32,
        handle: Handle,
        _flush: bool,
        _flock_release: bool,
        _lock_owner: Option<u64>,
    ) -> io::Result<()> {
        self.do_release(inode, handle)
    }

    fn create(
        &self,
        ctx: Context,
        parent: Inode,
        name: &CStr,
        mode: u32,
        kill_priv: bool,
        flags: u32,
        umask: u32,
        extensions: Extensions,
    ) -> io::Result<(Entry, Option<Handle>, OpenOptions)> {
        if extensions.secctx.is_some() {
            unimplemented!("SECURITY_CTX is not supported and should not be used by the guest");
        }

        let (_uid, _gid) = self.set_creds(ctx.uid, ctx.gid)?;
        let _killpriv_guard = if kill_priv {
            drop_effective_cap(Capability::CAP_FSETID)?
        } else {
            None
        };

        let data = self
            .inodes
            .read()
            .unwrap()
            .get(&parent)
            .cloned()
            .ok_or_else(ebadf)?;

        // Safe because this doesn't modify any memory and we check the return value. We don't
        // really check `flags` because if the kernel can't handle poorly specified flags then we
        // have much bigger problems.
        let fd = unsafe {
            libc::openat(
                data.file.as_raw_fd(),
                name.as_ptr(),
                flags as i32 | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                mode & !(umask & 0o777),
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }

        // Safe because we just opened this fd.
        let file = unsafe { File::from_raw_fd(fd) };

        let entry = self.lookup(ctx, parent, name)?;

        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        let data = HandleData {
            inode: entry.inode,
            qualification_fd: file.as_raw_fd(),
            file: RwLock::new(file),
            exported: Default::default(),
            directory_snapshot: None,
        };

        self.handles.write().unwrap().insert(handle, Arc::new(data));

        let mut opts = OpenOptions::empty();
        match self.cfg.cache_policy {
            CachePolicy::Never => opts |= OpenOptions::DIRECT_IO,
            CachePolicy::Always => opts |= OpenOptions::KEEP_CACHE,
            _ => {}
        };

        Ok((entry, Some(handle), opts))
    }

    fn unlink(&self, _ctx: Context, parent: Inode, name: &CStr) -> io::Result<()> {
        self.do_unlink(parent, name, 0)
    }

    fn read<W: io::Write + ZeroCopyWriter>(
        &self,
        _ctx: Context,
        inode: Inode,
        handle: Handle,
        w: W,
        size: u32,
        offset: u64,
        _lock_owner: Option<u64>,
        _flags: u32,
    ) -> io::Result<usize> {
        debug!("read: {inode:?}");
        let lease = self.lease_handle(inode, handle)?;
        self.read_leased(&lease, w, size, offset)
    }

    fn write<R: io::Read + ZeroCopyReader>(
        &self,
        _ctx: Context,
        inode: Inode,
        handle: Handle,
        r: R,
        size: u32,
        offset: u64,
        _lock_owner: Option<u64>,
        _delayed_write: bool,
        kill_priv: bool,
        _flags: u32,
    ) -> io::Result<usize> {
        let lease = self.lease_handle(inode, handle)?;
        self.write_leased(_ctx, &lease, r, size, offset, kill_priv)
    }

    fn getattr(
        &self,
        _ctx: Context,
        inode: Inode,
        _handle: Option<Handle>,
    ) -> io::Result<(libc::stat64, Duration)> {
        self.do_getattr(inode)
    }

    fn setattr(
        &self,
        _ctx: Context,
        inode: Inode,
        attr: libc::stat64,
        handle: Option<Handle>,
        valid: SetattrValid,
    ) -> io::Result<(libc::stat64, Duration)> {
        let inode_data = self
            .inodes
            .read()
            .unwrap()
            .get(&inode)
            .cloned()
            .ok_or_else(ebadf)?;

        enum Data {
            Handle(RawFd),
            ProcPath(CString),
        }

        // If we have a handle then use it otherwise get a new fd from the inode.
        let data = if let Some(handle) = handle {
            let hd = self
                .handles
                .read()
                .unwrap()
                .get(&handle)
                .filter(|hd| hd.inode == inode)
                .cloned()
                .ok_or_else(ebadf)?;

            let fd = hd.file.write().unwrap().as_raw_fd();
            Data::Handle(fd)
        } else {
            let pathname = CString::new(format!("{}", inode_data.file.as_raw_fd()))
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Data::ProcPath(pathname)
        };

        if valid.contains(SetattrValid::MODE) {
            // Safe because this doesn't modify any memory and we check the return value.
            let res = unsafe {
                match data {
                    Data::Handle(fd) => libc::fchmod(fd, attr.st_mode),
                    Data::ProcPath(ref p) => {
                        libc::fchmodat(self.proc_self_fd.as_raw_fd(), p.as_ptr(), attr.st_mode, 0)
                    }
                }
            };
            if res < 0 {
                return Err(io::Error::last_os_error());
            }
        }

        if valid.intersects(SetattrValid::UID | SetattrValid::GID) {
            let uid = if valid.contains(SetattrValid::UID) {
                attr.st_uid
            } else {
                // Cannot use -1 here because these are unsigned values.
                u32::MAX
            };
            let gid = if valid.contains(SetattrValid::GID) {
                attr.st_gid
            } else {
                // Cannot use -1 here because these are unsigned values.
                u32::MAX
            };

            // Safe because this is a constant value and a valid C string.
            let empty = unsafe { CStr::from_bytes_with_nul_unchecked(EMPTY_CSTR) };

            // Safe because this doesn't modify any memory and we check the return value.
            let res = unsafe {
                libc::fchownat(
                    inode_data.file.as_raw_fd(),
                    empty.as_ptr(),
                    uid,
                    gid,
                    libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if res < 0 {
                return Err(io::Error::last_os_error());
            }
        }

        if valid.contains(SetattrValid::SIZE) {
            // Safe because this doesn't modify any memory and we check the return value.
            let res = match data {
                Data::Handle(fd) => unsafe { libc::ftruncate(fd, attr.st_size) },
                _ => {
                    // There is no `ftruncateat` so we need to get a new fd and truncate it.
                    let f = self.open_inode(inode, libc::O_NONBLOCK | libc::O_RDWR)?;
                    unsafe { libc::ftruncate(f.as_raw_fd(), attr.st_size) }
                }
            };
            if res < 0 {
                return Err(io::Error::last_os_error());
            }
        }

        if valid.intersects(SetattrValid::ATIME | SetattrValid::MTIME) {
            let mut tvs = [
                libc::timespec {
                    tv_sec: 0,
                    tv_nsec: libc::UTIME_OMIT,
                },
                libc::timespec {
                    tv_sec: 0,
                    tv_nsec: libc::UTIME_OMIT,
                },
            ];

            if valid.contains(SetattrValid::ATIME_NOW) {
                tvs[0].tv_nsec = libc::UTIME_NOW;
            } else if valid.contains(SetattrValid::ATIME) {
                tvs[0].tv_sec = attr.st_atime;
                tvs[0].tv_nsec = attr.st_atime_nsec;
            }

            if valid.contains(SetattrValid::MTIME_NOW) {
                tvs[1].tv_nsec = libc::UTIME_NOW;
            } else if valid.contains(SetattrValid::MTIME) {
                tvs[1].tv_sec = attr.st_mtime;
                tvs[1].tv_nsec = attr.st_mtime_nsec;
            }

            // Safe because this doesn't modify any memory and we check the return value.
            let res = match data {
                Data::Handle(fd) => unsafe { libc::futimens(fd, tvs.as_ptr()) },
                Data::ProcPath(ref p) => unsafe {
                    libc::utimensat(self.proc_self_fd.as_raw_fd(), p.as_ptr(), tvs.as_ptr(), 0)
                },
            };
            if res < 0 {
                return Err(io::Error::last_os_error());
            }
        }

        self.do_getattr(inode)
    }

    fn rename(
        &self,
        _ctx: Context,
        olddir: Inode,
        oldname: &CStr,
        newdir: Inode,
        newname: &CStr,
        flags: u32,
    ) -> io::Result<()> {
        let old_inode = self
            .inodes
            .read()
            .unwrap()
            .get(&olddir)
            .cloned()
            .ok_or_else(ebadf)?;
        let new_inode = self
            .inodes
            .read()
            .unwrap()
            .get(&newdir)
            .cloned()
            .ok_or_else(ebadf)?;

        // Safe because this doesn't modify any memory and we check the return value.
        // The raw syscall avoids a dependency on the glibc 2.28 renameat2 wrapper.
        let res = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                old_inode.file.as_raw_fd(),
                oldname.as_ptr(),
                new_inode.file.as_raw_fd(),
                newname.as_ptr(),
                flags,
            )
        };
        if res == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn mknod(
        &self,
        ctx: Context,
        parent: Inode,
        name: &CStr,
        mode: u32,
        rdev: u32,
        umask: u32,
        extensions: Extensions,
    ) -> io::Result<Entry> {
        if extensions.secctx.is_some() {
            unimplemented!("SECURITY_CTX is not supported and should not be used by the guest");
        }

        let (_uid, _gid) = self.set_creds(ctx.uid, ctx.gid)?;
        let data = self
            .inodes
            .read()
            .unwrap()
            .get(&parent)
            .cloned()
            .ok_or_else(ebadf)?;

        // Safe because this doesn't modify any memory and we check the return value.
        let res = unsafe {
            libc::mknodat(
                data.file.as_raw_fd(),
                name.as_ptr(),
                (mode & !umask) as libc::mode_t,
                u64::from(rdev),
            )
        };

        if res < 0 {
            Err(io::Error::last_os_error())
        } else {
            self.lookup(ctx, parent, name)
        }
    }

    fn link(
        &self,
        ctx: Context,
        inode: Inode,
        newparent: Inode,
        newname: &CStr,
    ) -> io::Result<Entry> {
        let data = self
            .inodes
            .read()
            .unwrap()
            .get(&inode)
            .cloned()
            .ok_or_else(ebadf)?;
        let new_inode = self
            .inodes
            .read()
            .unwrap()
            .get(&newparent)
            .cloned()
            .ok_or_else(ebadf)?;

        let procname = CString::new(format!("{}", data.file.as_raw_fd()))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Safe because this doesn't modify any memory and we check the return value.
        let res = unsafe {
            libc::linkat(
                self.proc_self_fd.as_raw_fd(),
                procname.as_ptr(),
                new_inode.file.as_raw_fd(),
                newname.as_ptr(),
                libc::AT_SYMLINK_FOLLOW,
            )
        };
        if res == 0 {
            self.lookup(ctx, newparent, newname)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn symlink(
        &self,
        ctx: Context,
        linkname: &CStr,
        parent: Inode,
        name: &CStr,
        extensions: Extensions,
    ) -> io::Result<Entry> {
        // Set security context on symlink.
        if extensions.secctx.is_some() {
            unimplemented!("SECURITY_CTX is not supported and should not be used by the guest");
        }

        let (_uid, _gid) = self.set_creds(ctx.uid, ctx.gid)?;
        let data = self
            .inodes
            .read()
            .unwrap()
            .get(&parent)
            .cloned()
            .ok_or_else(ebadf)?;

        // Safe because this doesn't modify any memory and we check the return value.
        let res =
            unsafe { libc::symlinkat(linkname.as_ptr(), data.file.as_raw_fd(), name.as_ptr()) };
        if res == 0 {
            self.lookup(ctx, parent, name)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn readlink(&self, _ctx: Context, inode: Inode) -> io::Result<Vec<u8>> {
        let data = self
            .inodes
            .read()
            .unwrap()
            .get(&inode)
            .cloned()
            .ok_or_else(ebadf)?;

        let mut buf = vec![0; libc::PATH_MAX as usize];

        // Safe because this is a constant value and a valid C string.
        let empty = unsafe { CStr::from_bytes_with_nul_unchecked(EMPTY_CSTR) };

        // Safe because this will only modify the contents of `buf` and we check the return value.
        let res = unsafe {
            libc::readlinkat(
                data.file.as_raw_fd(),
                empty.as_ptr(),
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.len(),
            )
        };
        if res < 0 {
            return Err(io::Error::last_os_error());
        }

        buf.resize(res as usize, 0);
        Ok(buf)
    }

    fn flush(
        &self,
        _ctx: Context,
        inode: Inode,
        handle: Handle,
        _lock_owner: u64,
    ) -> io::Result<()> {
        let lease = self.lease_handle(inode, handle)?;
        self.flush_leased(&lease)
    }

    fn fsync(&self, _ctx: Context, inode: Inode, datasync: bool, handle: Handle) -> io::Result<()> {
        let lease = self.lease_handle(inode, handle)?;
        self.sync_leased(&lease, datasync)
    }

    fn fsyncdir(
        &self,
        ctx: Context,
        inode: Inode,
        datasync: bool,
        handle: Handle,
    ) -> io::Result<()> {
        self.fsync(ctx, inode, datasync, handle)
    }

    fn access(&self, ctx: Context, inode: Inode, mask: u32) -> io::Result<()> {
        let data = self
            .inodes
            .read()
            .unwrap()
            .get(&inode)
            .cloned()
            .ok_or_else(ebadf)?;

        let st = stat(&data.file)?;
        let mode = mask as i32 & (libc::R_OK | libc::W_OK | libc::X_OK);

        if mode == libc::F_OK {
            // The file exists since we were able to call `stat(2)` on it.
            return Ok(());
        }

        // We use ctx.uid/ctx.gid for these checks, but when idmapped mounts
        // support is enabled on the guest side, it means that "default_permissions"
        // flag is set on virtiofs mount and FUSE_ACCESS request should never be
        // sent to the userspace. Please, refer to the kernel commit
        // ("fs/fuse: warn if fuse_access is called when idmapped mounts are allowed").
        // In case when idmapped mounts are not enabled we are good to rely on ctx.uid/ctx.gid values.

        if (mode & libc::R_OK) != 0
            && ctx.uid != 0
            && (st.st_uid != ctx.uid || st.st_mode & 0o400 == 0)
            && (st.st_gid != ctx.gid || st.st_mode & 0o040 == 0)
            && st.st_mode & 0o004 == 0
        {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }

        if (mode & libc::W_OK) != 0
            && ctx.uid != 0
            && (st.st_uid != ctx.uid || st.st_mode & 0o200 == 0)
            && (st.st_gid != ctx.gid || st.st_mode & 0o020 == 0)
            && st.st_mode & 0o002 == 0
        {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }

        // root can only execute something if it is executable by one of the owner, the group, or
        // everyone.
        if (mode & libc::X_OK) != 0
            && (ctx.uid != 0 || st.st_mode & 0o111 == 0)
            && (st.st_uid != ctx.uid || st.st_mode & 0o100 == 0)
            && (st.st_gid != ctx.gid || st.st_mode & 0o010 == 0)
            && st.st_mode & 0o001 == 0
        {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }

        Ok(())
    }

    fn setxattr(
        &self,
        _ctx: Context,
        inode: Inode,
        name: &CStr,
        value: &[u8],
        flags: u32,
    ) -> io::Result<()> {
        if !self.cfg.xattr {
            return Err(io::Error::from_raw_os_error(libc::ENOSYS));
        }

        // The f{set,get,remove,list}xattr functions don't work on an fd opened with `O_PATH` so we
        // need to get a new fd. This doesn't work for symlinks, so we use the l* family of
        // functions in that case.
        let res = match self.open_inode_or_path(inode, libc::O_RDONLY | libc::O_NONBLOCK)? {
            FileOrLink::File(file) => {
                // Safe because this doesn't modify any memory and we check the return value.
                unsafe {
                    libc::fsetxattr(
                        file.as_raw_fd(),
                        name.as_ptr(),
                        value.as_ptr() as *const libc::c_void,
                        value.len(),
                        flags as libc::c_int,
                    )
                }
            }
            FileOrLink::Link(link) => {
                // Safe because this doesn't modify any memory and we check the return value.
                unsafe {
                    libc::lsetxattr(
                        link.as_ptr(),
                        name.as_ptr(),
                        value.as_ptr() as *const libc::c_void,
                        value.len(),
                        flags as libc::c_int,
                    )
                }
            }
        };

        if res == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn getxattr(
        &self,
        _ctx: Context,
        inode: Inode,
        name: &CStr,
        size: u32,
    ) -> io::Result<GetxattrReply> {
        if !self.cfg.xattr {
            return Err(io::Error::from_raw_os_error(libc::ENOSYS));
        }

        let mut buf = vec![0; size as usize];

        // The f{set,get,remove,list}xattr functions don't work on an fd opened with `O_PATH` so we
        // need to get a new fd. This doesn't work for symlinks, so we use the l* family of
        // functions in that case.
        let res = match self.open_inode_or_path(inode, libc::O_RDONLY | libc::O_NONBLOCK)? {
            FileOrLink::File(file) => {
                // Safe because this will only modify the contents of `buf`.
                unsafe {
                    libc::fgetxattr(
                        file.as_raw_fd(),
                        name.as_ptr(),
                        buf.as_mut_ptr() as *mut libc::c_void,
                        size as libc::size_t,
                    )
                }
            }
            FileOrLink::Link(link) => {
                // Safe because this will only modify the contents of `buf`.
                unsafe {
                    libc::lgetxattr(
                        link.as_ptr(),
                        name.as_ptr(),
                        buf.as_mut_ptr() as *mut libc::c_void,
                        size as libc::size_t,
                    )
                }
            }
        };

        if res < 0 {
            return Err(io::Error::last_os_error());
        }

        if size == 0 {
            Ok(GetxattrReply::Count(res as u32))
        } else {
            buf.resize(res as usize, 0);
            Ok(GetxattrReply::Value(buf))
        }
    }

    fn listxattr(&self, _ctx: Context, inode: Inode, size: u32) -> io::Result<ListxattrReply> {
        if !self.cfg.xattr {
            return Err(io::Error::from_raw_os_error(libc::ENOSYS));
        }

        let mut buf = vec![0; size as usize];

        // The f{set,get,remove,list}xattr functions don't work on an fd opened with `O_PATH` so we
        // need to get a new fd. This doesn't work for symlinks, so we use the l* family of
        // functions in that case.
        let res = match self.open_inode_or_path(inode, libc::O_RDONLY | libc::O_NONBLOCK)? {
            FileOrLink::File(file) => {
                // Safe because this will only modify the contents of `buf`.
                unsafe {
                    libc::flistxattr(
                        file.as_raw_fd(),
                        buf.as_mut_ptr() as *mut libc::c_char,
                        size as libc::size_t,
                    )
                }
            }
            FileOrLink::Link(link) => unsafe {
                libc::llistxattr(
                    link.as_ptr(),
                    buf.as_mut_ptr() as *mut libc::c_char,
                    size as libc::size_t,
                )
            },
        };
        if res < 0 {
            return Err(io::Error::last_os_error());
        }

        if size == 0 {
            Ok(ListxattrReply::Count(res as u32))
        } else {
            buf.resize(res as usize, 0);
            Ok(ListxattrReply::Names(buf))
        }
    }

    fn removexattr(&self, _ctx: Context, inode: Inode, name: &CStr) -> io::Result<()> {
        if !self.cfg.xattr {
            return Err(io::Error::from_raw_os_error(libc::ENOSYS));
        }

        // The f{set,get,remove,list}xattr functions don't work on an fd opened with `O_PATH` so we
        // need to get a new fd. This doesn't work for symlinks, so we use the l* family of
        // functions in that case.
        let res = match self.open_inode_or_path(inode, libc::O_RDONLY | libc::O_NONBLOCK)? {
            FileOrLink::File(file) => {
                // Safe because this doesn't modify any memory and we check the return value.
                unsafe { libc::fremovexattr(file.as_raw_fd(), name.as_ptr()) }
            }
            FileOrLink::Link(link) => {
                // Safe because this doesn't modify any memory and we check the return value.
                unsafe { libc::lremovexattr(link.as_ptr(), name.as_ptr()) }
            }
        };

        if res == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn fallocate(
        &self,
        _ctx: Context,
        inode: Inode,
        handle: Handle,
        mode: u32,
        offset: u64,
        length: u64,
    ) -> io::Result<()> {
        let data = self
            .handles
            .read()
            .unwrap()
            .get(&handle)
            .filter(|hd| hd.inode == inode)
            .cloned()
            .ok_or_else(ebadf)?;

        let fd = data.file.write().unwrap().as_raw_fd();
        // Safe because this doesn't modify any memory and we check the return value.
        let res = unsafe {
            libc::fallocate64(
                fd,
                mode as libc::c_int,
                offset as libc::off64_t,
                length as libc::off64_t,
            )
        };
        if res == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn lseek(
        &self,
        _ctx: Context,
        inode: Inode,
        handle: Handle,
        offset: u64,
        whence: u32,
    ) -> io::Result<u64> {
        let data = self
            .handles
            .read()
            .unwrap()
            .get(&handle)
            .filter(|hd| hd.inode == inode)
            .cloned()
            .ok_or_else(ebadf)?;

        let fd = data.file.write().unwrap().as_raw_fd();

        // Safe because this doesn't modify any memory and we check the return value.
        let res = unsafe { libc::lseek(fd, offset as libc::off64_t, whence as libc::c_int) };
        if res < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(res as u64)
        }
    }

    fn copyfilerange(
        &self,
        _ctx: Context,
        inode_in: Inode,
        handle_in: Handle,
        offset_in: u64,
        inode_out: Inode,
        handle_out: Handle,
        offset_out: u64,
        len: u64,
        flags: u64,
    ) -> io::Result<usize> {
        let data_in = self
            .handles
            .read()
            .unwrap()
            .get(&handle_in)
            .filter(|hd| hd.inode == inode_in)
            .cloned()
            .ok_or_else(ebadf)?;

        // Take just a read lock as we're not going to alter the file descriptor offset.
        let fd_in = data_in.file.read().unwrap().as_raw_fd();

        let data_out = self
            .handles
            .read()
            .unwrap()
            .get(&handle_out)
            .filter(|hd| hd.inode == inode_out)
            .cloned()
            .ok_or_else(ebadf)?;

        // Take just a read lock as we're not going to alter the file descriptor offset.
        let fd_out = data_out.file.read().unwrap().as_raw_fd();

        // Safe because this will only modify `offset_in` and `offset_out` and we check
        // the return value.
        let res = unsafe {
            libc::copy_file_range(
                fd_in,
                &mut (offset_in as i64) as &mut _ as *mut _,
                fd_out,
                &mut (offset_out as i64) as &mut _ as *mut _,
                len.try_into().unwrap(),
                flags.try_into().unwrap(),
            )
        };
        if res < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(res as usize)
        }
    }

    fn ioctl(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        handle: Self::Handle,
        _flags: u32,
        cmd: u32,
        _arg: u64,
        _in_size: u32,
        out_size: u32,
        _exit_code: &Arc<AtomicI32>,
    ) -> io::Result<Vec<u8>> {
        const VIRTIO_IOC_MAGIC: u8 = b'v';

        const VIRTIO_IOC_TYPE_EXPORT_FD: u8 = 1;
        const VIRTIO_IOC_EXPORT_FD_SIZE: usize = 2 * mem::size_of::<u64>();
        const VIRTIO_IOC_EXPORT_FD_REQ: u32 = request_code_read!(
            VIRTIO_IOC_MAGIC,
            VIRTIO_IOC_TYPE_EXPORT_FD,
            VIRTIO_IOC_EXPORT_FD_SIZE
        ) as u32;

        match cmd {
            VIRTIO_IOC_EXPORT_FD_REQ => {
                if out_size as usize != VIRTIO_IOC_EXPORT_FD_SIZE {
                    return Err(einval());
                }

                let mut exports = self
                    .cfg
                    .export_table
                    .as_ref()
                    .ok_or(io::Error::from_raw_os_error(libc::EOPNOTSUPP))?
                    .lock()
                    .unwrap();

                let handles = self.handles.read().unwrap();
                let data = handles
                    .get(&handle)
                    .filter(|hd| hd.inode == inode)
                    .ok_or_else(ebadf)?;

                data.exported.store(true, Ordering::Relaxed);

                let fd = data.file.read().unwrap().try_clone()?;

                exports.insert((self.cfg.export_fsid, handle), fd);

                let mut ret: Vec<_> = self.cfg.export_fsid.to_ne_bytes().into();
                ret.extend_from_slice(&handle.to_ne_bytes());
                Ok(ret)
            }
            // OverlayFS treats ENOTTY as an unsupported lower fileattr query.
            _ => Err(io::Error::from_raw_os_error(libc::ENOTTY)),
        }
    }
}

impl PassthroughFs {
    pub(crate) fn lookup_mount_id(&self, inode: u64) -> Option<u64> {
        self.inodes
            .read()
            .unwrap()
            .get(&inode)
            .map(|data| data.mnt_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffered_write_fixture(flags: i32) -> (tempfile::TempDir, PassthroughFs, HandleLease) {
        use std::os::unix::fs::OpenOptionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file"), b"abcdefgh").unwrap();
        let fs = PassthroughFs::new(
            Config {
                root_dir: root.path().to_string_lossy().into_owned(),
                ..Default::default()
            },
            Arc::new(InodeAllocator::new()),
        )
        .unwrap();
        let file = std::fs::OpenOptions::new()
            .read(flags & libc::O_ACCMODE != libc::O_WRONLY)
            .write(flags & libc::O_ACCMODE != libc::O_RDONLY)
            .custom_flags(flags & !libc::O_ACCMODE)
            .open(root.path().join("file"))
            .unwrap();
        let lease = HandleLease(Arc::new(HandleData {
            directory_snapshot: None,
            inode: 1,
            qualification_fd: file.as_raw_fd(),
            file: RwLock::new(file),
            exported: AtomicBool::new(false),
        }));
        (root, fs, lease)
    }

    #[test]
    fn buffered_write_qualification_checks_actual_flags_type_and_exports() {
        for flags in [libc::O_WRONLY, libc::O_RDWR] {
            let (_root, fs, lease) = buffered_write_fixture(flags);
            assert!(fs.buffered_write_eligible(&lease).unwrap());
            assert!(fs.buffered_write_eligible_unlocked(&lease).unwrap());
        }
        for flags in [
            libc::O_RDONLY,
            libc::O_RDWR | libc::O_APPEND,
            libc::O_RDWR | libc::O_SYNC,
            libc::O_RDWR | libc::O_DSYNC,
            libc::O_RDWR | libc::O_DIRECT,
        ] {
            let (_root, fs, lease) = buffered_write_fixture(flags);
            assert!(
                !fs.buffered_write_eligible(&lease).unwrap(),
                "flags={flags:#x}"
            );
            assert!(
                !fs.buffered_write_eligible_unlocked(&lease).unwrap(),
                "flags={flags:#x}"
            );
        }
        let (root, fs, lease) = buffered_write_fixture(libc::O_RDWR);
        lease.0.exported.store(true, Ordering::Relaxed);
        assert!(!fs.buffered_write_eligible(&lease).unwrap());
        assert!(!fs.buffered_write_eligible_unlocked(&lease).unwrap());
        let file = File::open(root.path()).unwrap();
        let directory = HandleLease(Arc::new(HandleData {
            directory_snapshot: None,
            inode: 1,
            qualification_fd: file.as_raw_fd(),
            file: RwLock::new(file),
            exported: AtomicBool::new(false),
        }));
        assert!(!fs.buffered_write_eligible(&directory).unwrap());
        assert!(!fs.buffered_write_eligible_unlocked(&directory).unwrap());
    }

    #[test]
    fn buffered_unlocked_qualification_progresses_while_native_write_lock_is_held() {
        let (_root, fs, lease) = buffered_write_fixture(libc::O_RDWR);
        std::thread::scope(|scope| {
            // Declared inside the scope closure so an assertion failure drops
            // the guard before scoped threads are joined during unwinding.
            let native_write = lease.0.file.write().unwrap();
            let (started, starting) = std::sync::mpsc::channel();
            let (locked, locked_result) = std::sync::mpsc::channel();
            let fs_ref = &fs;
            let lease_ref = &lease;
            scope.spawn(move || {
                started.send(()).unwrap();
                locked
                    .send(fs_ref.buffered_write_eligible(lease_ref))
                    .unwrap();
            });
            starting.recv_timeout(Duration::from_secs(2)).unwrap();
            let (unlocked, unlocked_result) = std::sync::mpsc::channel();
            scope.spawn(move || {
                unlocked
                    .send(fs_ref.buffered_write_eligible_unlocked(lease_ref))
                    .unwrap();
            });
            assert!(unlocked_result
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap());
            assert!(matches!(
                locked_result.recv_timeout(Duration::from_millis(30)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ));
            drop(native_write);
            assert!(locked_result
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap());
        });
    }

    #[test]
    fn buffered_unlocked_qualification_keeps_the_fd_after_release_and_name_reuse() {
        let (root, fs, _unused) = buffered_write_fixture(libc::O_RDWR);
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        let handle = fs
            .open(ctx, inode, false, libc::O_RDWR as u32)
            .unwrap()
            .0
            .unwrap();
        let lease = fs.lease_handle(inode, handle).unwrap();
        fs.release(ctx, inode, 0, handle, false, false, None)
            .unwrap();
        fs.forget(ctx, inode, 1);
        std::fs::remove_file(root.path().join("file")).unwrap();
        std::fs::create_dir(root.path().join("file")).unwrap();
        assert!(fs.buffered_write_eligible(&lease).unwrap());
        assert!(fs.buffered_write_eligible_unlocked(&lease).unwrap());
    }

    #[test]
    fn buffered_write_merges_chunks_on_pinned_object_without_moving_file_offset() {
        let (root, fs, lease) = buffered_write_fixture(libc::O_RDWR);
        std::fs::rename(root.path().join("file"), root.path().join("saved")).unwrap();
        std::fs::write(root.path().join("file"), b"replacement").unwrap();
        {
            let file = lease.0.file.read().unwrap();
            assert_eq!(
                unsafe { libc::lseek(file.as_raw_fd(), 3, libc::SEEK_SET) },
                3
            );
        }
        assert_eq!(
            fs.write_buffered_leased(&lease, &[b"12", b"3", b"45"], 2, false)
                .unwrap(),
            5
        );
        assert_eq!(
            std::fs::read(root.path().join("saved")).unwrap(),
            b"ab12345h"
        );
        assert_eq!(
            std::fs::read(root.path().join("file")).unwrap(),
            b"replacement"
        );
        let file = lease.0.file.read().unwrap();
        assert_eq!(
            unsafe { libc::lseek(file.as_raw_fd(), 0, libc::SEEK_CUR) },
            3
        );
    }

    #[test]
    fn buffered_write_rechecks_append_and_validates_vectors_and_offsets() {
        let (root, fs, lease) = buffered_write_fixture(libc::O_RDWR);
        assert!(fs.buffered_write_eligible(&lease).unwrap());
        {
            let file = lease.0.file.read().unwrap();
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
            assert!(flags >= 0);
            assert_eq!(
                unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_APPEND) },
                0
            );
        }
        assert!(!fs.buffered_write_eligible(&lease).unwrap());
        assert!(!fs.buffered_write_eligible_unlocked(&lease).unwrap());
        assert_eq!(
            fs.write_buffered_leased(&lease, &[b"x"], 0, false)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EOPNOTSUPP)
        );
        assert_eq!(
            std::fs::read(root.path().join("file")).unwrap(),
            b"abcdefgh"
        );
        for (chunks, offset) in [
            (vec![b"x".as_slice(); 65], 0),
            (Vec::new(), i64::MAX as u64 + 1),
            (vec![b"x".as_slice()], i64::MAX as u64),
        ] {
            assert_eq!(
                fs.write_buffered_leased(&lease, &chunks, offset, false)
                    .unwrap_err()
                    .raw_os_error(),
                Some(libc::EINVAL)
            );
        }
    }

    #[test]
    fn buffered_write_preserves_successful_partial_count_and_later_error() {
        const CHILD: &str = "PVISOR_NATIVE_BUFFERED_WRITE_PARTIAL_CHILD";
        if std::env::var_os(CHILD).as_deref() != Some(std::ffi::OsStr::new("1")) {
            // Isolate the process-wide file limit from all other test threads.
            let thread = std::thread::current();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", thread.name().unwrap(), "--nocapture"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "partial-write child failed: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (root, fs, lease) = buffered_write_fixture(libc::O_RDWR);
        let limit = libc::rlimit {
            rlim_cur: 4,
            rlim_max: 4,
        };
        // SAFETY: this isolated child owns its signal disposition and limits.
        unsafe {
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limit), 0);
        }
        assert_eq!(
            fs.write_buffered_leased(&lease, &[b"123", b"456"], 0, false)
                .unwrap(),
            4
        );
        assert_eq!(
            std::fs::read(root.path().join("file")).unwrap(),
            b"1234efgh"
        );
        assert_eq!(
            fs.write_buffered_leased(&lease, &[b"x"], 4, false)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EFBIG)
        );
    }

    fn qualification_profile(name: &str) -> pvisor_overlay_core::profile::Profile {
        let profile = pvisor_overlay_core::profile::Profile::enabled(name);
        for label in OPEN_QUALIFICATION_COUNTERS {
            profile.add(label, 0);
        }
        profile
    }

    #[test]
    fn local_flush_filesystems_exclude_remote_and_unknown_magic() {
        for magic in [0xef53, 0x5846_5342, 0x9123_683e, 0x0102_1994] {
            assert!(local_readonly_flush_filesystem(magic));
        }
        for magic in [0, 0x6969, 0x6573_5546, 0xff53_4d42, 0x794c_7630, u64::MAX] {
            assert!(!local_readonly_flush_filesystem(magic));
        }
    }

    #[test]
    fn shared_open_qualification_matches_separate_checks_on_each_pinned_descriptor() {
        let root = tempfile::tempdir_in("/dev/shm").unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, b"original").unwrap();
        let fs = PassthroughFs::new(
            Config {
                root_dir: root.path().to_string_lossy().into_owned(),
                ..Default::default()
            },
            Arc::new(InodeAllocator::new()),
        )
        .unwrap();
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        let ro = fs
            .open(ctx, inode, false, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        let legacy = qualification_profile("qualification-legacy");
        let identity = fs.lower_data_identity_profiled(inode, ro, &legacy).unwrap();
        assert!(fs.readonly_local_flush_eligible_profiled(inode, ro, &legacy));
        let shared = qualification_profile("qualification-shared");
        let qualified = fs.qualify_readonly_open(inode, ro, true, &shared);
        assert_eq!(qualified.data_identity, Some(identity));
        assert!(qualified.local_flush);
        let legacy = legacy.report().unwrap();
        let shared = shared.report().unwrap();
        for (label, separate, combined) in [
            ("open_qualification_fgetfl_attempts", 2, 1),
            ("open_qualification_fstat_attempts", 2, 0),
            ("open_qualification_statx_attempts", 1, 1),
            ("open_qualification_fstatfs_attempts", 2, 1),
        ] {
            assert_eq!(legacy.measurements[label].units, separate);
            assert_eq!(shared.measurements[label].units, combined);
        }
        // The descriptor remains the old object after name replacement; a new
        // lookup/open gets a separate receipt rather than a pathname cache hit.
        std::fs::rename(&path, root.path().join("saved")).unwrap();
        std::fs::write(&path, b"new object").unwrap();
        let later = qualification_profile("qualification-later");
        assert_eq!(
            fs.qualify_readonly_open(inode, ro, true, &later)
                .data_identity,
            Some(identity)
        );
        let replacement = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        let replacement_handle = fs
            .open(ctx, replacement, false, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        let new_identity = fs
            .qualify_readonly_open(replacement, replacement_handle, true, &later)
            .data_identity
            .unwrap();
        assert_ne!(new_identity.ino, identity.ino);
        assert_eq!(new_identity.size, 10);
        // Mutable status flags are rechecked, never cached across classifiers.
        let lease = fs.lease_handle(replacement, replacement_handle).unwrap();
        let descriptor = lease.0.file.read().unwrap();
        let original_flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
        assert!(original_flags >= 0);
        assert_eq!(
            unsafe {
                libc::fcntl(
                    descriptor.as_raw_fd(),
                    libc::F_SETFL,
                    original_flags | libc::O_APPEND,
                )
            },
            0
        );
        drop(descriptor);
        assert_eq!(
            fs.qualify_readonly_open(replacement, replacement_handle, true, &later)
                .data_identity,
            Some(new_identity)
        );
        drop(lease);
        std::fs::hard_link(root.path().join("saved"), root.path().join("alias")).unwrap();
        let linked = fs.qualify_readonly_open(inode, ro, true, &later);
        assert!(linked.data_identity.is_none());
        assert!(linked.local_flush); // FLUSH never required a single hardlink.
        let rw = fs
            .open(ctx, replacement, false, libc::O_RDWR as u32)
            .unwrap()
            .0
            .unwrap();
        let writable = fs.qualify_readonly_open(replacement, rw, true, &later);
        assert!(writable.data_identity.is_none() && !writable.local_flush);
        let directory = fs
            .opendir(ctx, fuse::ROOT_ID, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        let qualified_directory = fs.qualify_readonly_open(fuse::ROOT_ID, directory, true, &later);
        assert!(qualified_directory.data_identity.is_none() && !qualified_directory.local_flush);
        fs.releasedir(ctx, fuse::ROOT_ID, 0, directory).unwrap();
        assert!(fs
            .qualify_readonly_open(inode + 100, ro, true, &later)
            .data_identity
            .is_none());
        for (node, handle) in [
            (inode, ro),
            (replacement, replacement_handle),
            (replacement, rw),
        ] {
            fs.release(ctx, node, 0, handle, false, false, None)
                .unwrap();
            let closed = fs.qualify_readonly_open(node, handle, true, &later);
            assert!(closed.data_identity.is_none() && !closed.local_flush);
        }
        fs.forget(ctx, inode, 1);
        fs.forget(ctx, replacement, 1);
    }

    #[test]
    fn shared_open_evidence_rejects_incomplete_data_but_preserves_flush_fallback() {
        let root = tempfile::tempdir_in("/dev/shm").unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, b"original").unwrap();
        let file = File::open(&path).unwrap();
        // Use actual descriptor evidence and the production fallback helper;
        // control only unsupported/missing statx result bits and errno.
        for missing in [0x0001, 0x0004, 0x0100, 0x0200, STATX_MNT_ID] {
            let (stat, mount, mask) = statx_evidence(&file).unwrap();
            let profile = qualification_profile("qualification-mask");
            let qualified = qualify_readonly_file_evidence(
                &file,
                libc::O_RDONLY,
                Ok((stat, mount, mask & !missing)),
                true,
                &profile,
            );
            assert!(qualified.data_identity.is_none());
            assert!(qualified.local_flush);
            let report = profile.report().unwrap();
            assert_eq!(
                report.measurements["open_qualification_fstat_attempts"].units,
                u64::from(missing == 0x0001)
            );
            assert_eq!(
                report.measurements["open_qualification_fstatfs_attempts"].units,
                1
            );
        }
        for want_flush in [false, true] {
            let profile = qualification_profile("qualification-unsupported");
            let qualified = qualify_readonly_file_evidence(
                &file,
                libc::O_RDONLY,
                Err(io::Error::from_raw_os_error(libc::ENOSYS)),
                want_flush,
                &profile,
            );
            assert!(qualified.data_identity.is_none());
            assert_eq!(qualified.local_flush, want_flush);
            let report = profile.report().unwrap();
            for label in [
                "open_qualification_fstat_attempts",
                "open_qualification_fstatfs_attempts",
            ] {
                assert_eq!(report.measurements[label].units, u64::from(want_flush));
            }
        }
        let (mut stat, mount, mask) = statx_evidence(&file).unwrap();
        assert!(lower_data_identity_from_evidence(libc::O_RDONLY, &stat, mount, mask).is_some());
        for flags in [libc::O_WRONLY, libc::O_RDWR, libc::O_DIRECT, libc::O_PATH] {
            assert!(lower_data_identity_from_evidence(flags, &stat, mount, mask).is_none());
        }
        assert!(lower_data_identity_from_evidence(libc::O_RDONLY, &stat, 0, mask).is_none());
        stat.st_size = -1;
        assert!(lower_data_identity_from_evidence(libc::O_RDONLY, &stat, mount, mask).is_none());
    }

    #[test]
    fn local_flush_qualification_uses_live_pinned_readonly_regular_descriptors() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file"), b"original").unwrap();
        let fs = PassthroughFs::new(
            Config {
                root_dir: root.path().to_string_lossy().into_owned(),
                ..Default::default()
            },
            Arc::new(InodeAllocator::new()),
        )
        .unwrap();
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        let readonly = fs
            .open(ctx, inode, false, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        let writable = fs
            .open(ctx, inode, false, libc::O_RDWR as u32)
            .unwrap()
            .0
            .unwrap();
        assert!(!fs.readonly_local_flush_eligible(inode, writable));
        assert!(!fs.readonly_local_flush_eligible(inode, u64::MAX));
        let lease = fs.lease_handle(inode, readonly).unwrap();
        let mut filesystem = MaybeUninit::<libc::statfs>::zeroed();
        let file = lease.0.file.read().unwrap();
        // SAFETY: the leased file and initialized output live across fstatfs.
        assert_eq!(
            unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) },
            0
        );
        let magic = unsafe { filesystem.assume_init() }.f_type as u64 & 0xffff_ffff;
        drop(file);
        // Removing/replacing the pathname cannot requalify a different object.
        std::fs::remove_file(root.path().join("file")).unwrap();
        std::fs::create_dir(root.path().join("file")).unwrap();
        assert_eq!(
            fs.readonly_local_flush_eligible(inode, readonly),
            local_readonly_flush_filesystem(magic)
        );
        let directory = fs
            .opendir(ctx, fuse::ROOT_ID, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        assert!(!fs.readonly_local_flush_eligible(fuse::ROOT_ID, directory));
        fs.release(ctx, inode, 0, readonly, false, false, None)
            .unwrap();
        assert!(!fs.readonly_local_flush_eligible(inode, readonly));
        fs.release(ctx, inode, 0, writable, false, false, None)
            .unwrap();
        fs.releasedir(ctx, fuse::ROOT_ID, 0, directory).unwrap();
        fs.forget(ctx, inode, 1);
    }

    #[test]
    fn lower_data_identity_is_bound_to_the_readonly_single_link_open_description() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, b"original").unwrap();
        let fs = PassthroughFs::new(
            Config {
                root_dir: root.path().to_string_lossy().into_owned(),
                ..Default::default()
            },
            Arc::new(InodeAllocator::new()),
        )
        .unwrap();
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        let ro = fs
            .open(ctx, inode, false, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        let rw = fs
            .open(ctx, inode, false, libc::O_RDWR as u32)
            .unwrap()
            .0
            .unwrap();
        assert!(fs.lower_data_identity(inode, rw).is_none());
        assert!(fs.lower_data_identity(inode + 100, ro).is_none());
        let first = fs.lower_data_identity(inode, ro);
        if let Some(identity) = first {
            assert_eq!(identity.size, 8);
            assert_eq!(identity.ino, std::fs::metadata(&path).unwrap().ino());
            assert_ne!(identity.mount_id, 0);
        }
        std::fs::rename(&path, root.path().join("saved")).unwrap();
        std::fs::write(&path, b"replacement is longer").unwrap();
        assert_eq!(fs.lower_data_identity(inode, ro), first);
        std::fs::hard_link(root.path().join("saved"), root.path().join("alias")).unwrap();
        assert!(fs.lower_data_identity(inode, ro).is_none());
        std::fs::remove_file(root.path().join("alias")).unwrap();
        assert_eq!(fs.lower_data_identity(inode, ro), first);
        for h in [ro, rw] {
            fs.release(ctx, inode, 0, h, false, false, None).unwrap();
        }
        assert!(fs.lower_data_identity(inode, ro).is_none());
        fs.forget(ctx, inode, 1);
    }

    #[test]
    fn regular_attributes_match_native_lookup_without_reading_file_contents() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, b"attributes only").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::fs::hard_link(&path, root.path().join("alias")).unwrap();
        let time = std::time::UNIX_EPOCH + Duration::from_secs(1000);
        File::open(&path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_accessed(time)
                    .set_modified(time),
            )
            .unwrap();
        let fs = PassthroughFs::new(
            Config {
                root_dir: root.path().to_string_lossy().into_owned(),
                attr_timeout: Duration::from_secs(3),
                entry_timeout: Duration::from_secs(7),
                ..Default::default()
            },
            Arc::new(InodeAllocator::new()),
        )
        .unwrap();
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let before = std::fs::symlink_metadata(&path).unwrap();
        let (attr, attr_timeout, entry_timeout) = fs
            .regular_lookup_attributes(fuse::ROOT_ID, c"file")
            .unwrap()
            .unwrap();
        let entry = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
        macro_rules! same_fields {
            ($($field:ident),+ $(,)?) => { $(assert_eq!(attr.$field, entry.attr.$field, stringify!($field));)+ };
        }
        same_fields!(
            st_dev,
            st_ino,
            st_mode,
            st_nlink,
            st_uid,
            st_gid,
            st_rdev,
            st_size,
            st_blksize,
            st_blocks,
            st_atime,
            st_atime_nsec,
            st_mtime,
            st_mtime_nsec,
            st_ctime,
            st_ctime_nsec
        );
        assert_eq!(attr.st_nlink, 2);
        assert_eq!(attr_timeout, entry.attr_timeout);
        assert_eq!(entry_timeout, entry.entry_timeout);
        let after = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(
            (after.atime(), after.atime_nsec()),
            (before.atime(), before.atime_nsec())
        );
        fs.forget(ctx, entry.inode, 1);
        std::fs::write(&path, b"fresh host size").unwrap();
        assert_eq!(
            fs.regular_lookup_attributes(fuse::ROOT_ID, c"file")
                .unwrap()
                .unwrap()
                .0
                .st_size,
            15
        );
        assert_eq!(
            fs.regular_lookup_attributes(fuse::ROOT_ID, c"missing")
                .err()
                .unwrap()
                .raw_os_error(),
            Some(libc::ENOENT)
        );
    }

    #[test]
    fn regular_attributes_keep_single_name_nofollow_and_directory_fallback() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("directory")).unwrap();
        std::fs::write(root.path().join("file"), b"data").unwrap();
        std::os::unix::fs::symlink("file", root.path().join("symlink")).unwrap();
        let fs = PassthroughFs::new(
            Config {
                root_dir: root.path().to_string_lossy().into_owned(),
                ..Default::default()
            },
            Arc::new(InodeAllocator::new()),
        )
        .unwrap();
        fs.init(FsOptions::SUBMOUNTS).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        for name in [c"directory", c"symlink"] {
            assert!(fs
                .regular_lookup_attributes(fuse::ROOT_ID, name)
                .unwrap()
                .is_none());
            let entry = fs.lookup(ctx, fuse::ROOT_ID, name).unwrap();
            assert_ne!(entry.attr.st_mode & libc::S_IFMT, libc::S_IFREG);
            fs.forget(ctx, entry.inode, 1);
        }
        for name in [c"", c".", c"..", c"directory/../file"] {
            assert_eq!(
                fs.regular_lookup_attributes(fuse::ROOT_ID, name)
                    .err()
                    .unwrap()
                    .raw_os_error(),
                Some(libc::EINVAL)
            );
        }
        // /proc is already a distinct Linux mount: this read-only check needs
        // no test mount and exercises the original directory SUBMOUNTS reply.
        let host = PassthroughFs::new(
            Config {
                root_dir: "/".into(),
                ..Default::default()
            },
            Arc::new(InodeAllocator::new()),
        )
        .unwrap();
        host.init(FsOptions::SUBMOUNTS).unwrap();
        assert!(host
            .regular_lookup_attributes(fuse::ROOT_ID, c"proc")
            .unwrap()
            .is_none());
        let proc = host.lookup(ctx, fuse::ROOT_ID, c"proc").unwrap();
        assert_ne!(proc.attr_flags & fuse::ATTR_SUBMOUNT, 0);
        host.forget(ctx, proc.inode, 1);
    }

    // A non-zero uid the server can be made to run as, plus a guest
    // uid distinct from it and from root. Both must be mappable when
    // the test runs inside a user namespace (e.g. `buildah unshare`),
    // so keep them inside the conventional 65536-wide subid range.
    const SERVER_UID: libc::uid_t = 1;
    const SERVER_GID: libc::gid_t = 1;
    const GUEST_UID: libc::uid_t = 1000;

    /// Regression test for the `scoped_cred` credential restore.
    ///
    /// The fs server switches the worker thread's effective uid per
    /// request (`ScopedUid`) and restores it on drop. When the server
    /// runs as a non-root user that holds CAP_SETUID — e.g. an
    /// unprivileged daemon granted the capability via systemd's
    /// `AmbientCapabilities=` — restoring to a hardcoded euid 0 wedges
    /// the thread: the next switch from euid 0 to
    /// a non-zero uid clears its effective capability set, the restore
    /// after that fails EPERM, and the thread is stranded at the guest
    /// uid, failing every later request (including ones for guest
    /// root). Restoring the *previous* euid keeps switching between
    /// non-zero uids only, which never clears the capabilities.
    ///
    /// This drives the real `ScopedUid` through repeated switches and
    /// asserts the thread is left back at the server uid. Credential
    /// changes are per-thread (raw syscalls, by design), so it runs on
    /// a dedicated thread and leaves the rest of the test binary
    /// untouched. It needs CAP_SETUID and the ability to move to a
    /// non-zero uid; without them (a plain `cargo test` as a normal
    /// user) it skips rather than fails.
    #[test]
    fn scoped_uid_restores_server_uid_without_wedging() {
        let outcome = std::thread::spawn(scoped_uid_no_wedge_body)
            .join()
            .expect("worker thread panicked");
        match outcome {
            Ok(()) => {}
            Err(skip) => eprintln!("SKIP scoped_uid_restores_server_uid_without_wedging: {skip}"),
        }
    }

    /// Establish the non-root-with-CAP_SETUID substrate on the current
    /// thread, then exercise `ScopedUid`. `Err(reason)` means the
    /// substrate could not be set up and the test should skip; a real
    /// regression (the thread left wedged) panics via `assert` so the
    /// test fails.
    fn scoped_uid_no_wedge_body() -> Result<(), String> {
        if !has_cap(None, CapSet::Effective, Capability::CAP_SETUID).map_err(|e| e.to_string())? {
            return Err("missing CAP_SETUID (run as root or under a userns with caps)".into());
        }

        // Keep the permitted caps across the upcoming uid change.
        if unsafe { libc::prctl(libc::PR_SET_KEEPCAPS, 1, 0, 0, 0) } != 0 {
            return Err(format!("PR_SET_KEEPCAPS: {}", io::Error::last_os_error()));
        }
        // Per-thread setresgid/setresuid via raw syscalls, mirroring how
        // `scoped_cred` changes only the calling thread's credentials.
        // gid first — once euid is non-zero the right to setgid may be
        // gone. A failure here is a substrate problem (e.g. the target
        // uid is not mapped into the namespace), so it skips.
        if unsafe { libc::syscall(libc::SYS_setresgid, SERVER_GID, SERVER_GID, SERVER_GID) } != 0 {
            return Err(format!("setresgid: {}", io::Error::last_os_error()));
        }
        if unsafe { libc::syscall(libc::SYS_setresuid, SERVER_UID, SERVER_UID, SERVER_UID) } != 0 {
            return Err(format!("setresuid: {}", io::Error::last_os_error()));
        }
        // The euid 0 -> non-zero move cleared the effective set; raise
        // back the caps the server relies on from the retained permitted
        // set. This is the state an AmbientCapabilities= daemon boots in.
        for cap in [Capability::CAP_SETUID, Capability::CAP_SETGID] {
            caps::raise(None, CapSet::Effective, cap).map_err(|e| e.to_string())?;
        }
        assert_eq!(
            unsafe { libc::geteuid() },
            SERVER_UID,
            "precondition: thread should now run as the server uid"
        );

        // Credential setup has succeeded; subsequent failures must fail the
        // test, not skip. Repeated guest-uid switches verify that Drop restores
        // the non-root server uid without losing effective capabilities.
        for i in 0..4 {
            let scoped = ScopedUid::new(GUEST_UID)
                .expect("switching to the guest uid should succeed while caps are held");
            assert_eq!(
                unsafe { libc::geteuid() },
                GUEST_UID,
                "switch #{i} should land on the guest uid"
            );
            drop(scoped);
        }

        assert_eq!(
            unsafe { libc::geteuid() },
            SERVER_UID,
            "worker thread wedged after repeated switches: euid was not restored to \
             the server uid (the scoped_cred Drop regressed to restoring euid 0)"
        );
        Ok(())
    }
}
