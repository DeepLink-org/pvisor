//! Small, cross-platform syscall wrappers.
//!
//! Keeping the unsafe boundary here makes the overlay logic easier to audit.

use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn cvt(rc: libc::c_int) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn timespec(time: SystemTime) -> libc::timespec {
    match time.duration_since(UNIX_EPOCH) {
        Ok(value) => libc::timespec {
            tv_sec: value.as_secs() as i64 as _,
            tv_nsec: value.subsec_nanos() as libc::c_long,
        },
        Err(value) => {
            let value = value.duration();
            let fractional = value.subsec_nanos();
            libc::timespec {
                tv_sec: (-(value.as_secs() as i64) - i64::from(fractional != 0)) as _,
                tv_nsec: if fractional == 0 {
                    0
                } else {
                    1_000_000_000 - fractional as libc::c_long
                },
            }
        }
    }
}

pub fn unix_time(seconds: i64, nanoseconds: i64) -> SystemTime {
    let offset = std::time::Duration::new(seconds.unsigned_abs(), 0);
    let base = if seconds < 0 {
        UNIX_EPOCH - offset
    } else {
        UNIX_EPOCH + offset
    };
    base + std::time::Duration::from_nanos(nanoseconds as u64)
}

pub fn set_times(
    path: &Path,
    atime: Option<SystemTime>,
    mtime: Option<SystemTime>,
    nofollow: bool,
) -> io::Result<()> {
    let path = c_path(path)?;
    let omit = libc::timespec {
        tv_sec: 0,
        tv_nsec: libc::UTIME_OMIT,
    };
    let times = [
        atime.map(timespec).unwrap_or(omit),
        mtime.map(timespec).unwrap_or(omit),
    ];
    let flags = if nofollow {
        libc::AT_SYMLINK_NOFOLLOW
    } else {
        0
    };
    // SAFETY: path and times are valid for the duration of the call.
    cvt(unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), flags) })
}

pub fn chown(path: &Path, uid: u32, gid: u32, nofollow: bool) -> io::Result<()> {
    let path = c_path(path)?;
    let flags = if nofollow {
        libc::AT_SYMLINK_NOFOLLOW
    } else {
        0
    };
    // SAFETY: path is a valid NUL-terminated string.
    cvt(unsafe {
        libc::fchownat(
            libc::AT_FDCWD,
            path.as_ptr(),
            uid as libc::uid_t,
            gid as libc::gid_t,
            flags,
        )
    })
}

pub fn access(path: &Path, mask: i32) -> io::Result<()> {
    let path = c_path(path)?;
    // SAFETY: path is a valid NUL-terminated string.
    cvt(unsafe { libc::access(path.as_ptr(), mask) })
}

pub fn mknod(path: &Path, mode: u32, rdev: u32) -> io::Result<()> {
    let path = c_path(path)?;
    // SAFETY: path is a valid NUL-terminated string.
    cvt(unsafe { libc::mknod(path.as_ptr(), mode as libc::mode_t, rdev as libc::dev_t) })
}

pub struct StatFs {
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub bsize: u32,
    pub namelen: u32,
    pub frsize: u32,
}

pub fn statfs(path: &Path) -> io::Result<StatFs> {
    let path = c_path(path)?;
    // SAFETY: zero is a valid initial representation for statvfs.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: path and output pointer are valid.
    let rc = unsafe { libc::statvfs(path.as_ptr(), &mut stat) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(StatFs {
        blocks: stat.f_blocks as u64,
        bfree: stat.f_bfree as u64,
        bavail: stat.f_bavail as u64,
        files: stat.f_files as u64,
        ffree: stat.f_ffree as u64,
        bsize: stat.f_bsize as u32,
        namelen: stat.f_namemax as u32,
        frsize: stat.f_frsize as u32,
    })
}

fn xattr_buffer<F>(mut call: F) -> io::Result<Vec<u8>>
where
    F: FnMut(*mut libc::c_void, usize) -> libc::ssize_t,
{
    let needed = call(std::ptr::null_mut(), 0);
    if needed < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0_u8; needed as usize];
    if buffer.is_empty() {
        return Ok(buffer);
    }
    let actual = call(buffer.as_mut_ptr().cast(), buffer.len());
    if actual < 0 {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(actual as usize);
    Ok(buffer)
}

pub fn list_xattrs(path: &Path) -> io::Result<Vec<Vec<u8>>> {
    let path = c_path(path)?;
    #[cfg(target_os = "macos")]
    let data = xattr_buffer(|buf, size| {
        // SAFETY: buffers are either null/zero or valid writable allocations.
        unsafe { libc::listxattr(path.as_ptr(), buf.cast(), size, libc::XATTR_NOFOLLOW) }
    })?;
    #[cfg(not(target_os = "macos"))]
    let data = xattr_buffer(|buf, size| {
        // SAFETY: buffers are either null/zero or valid writable allocations.
        unsafe { libc::llistxattr(path.as_ptr(), buf.cast(), size) }
    })?;
    Ok(data
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

pub fn get_xattr(path: &Path, name: &OsStr) -> io::Result<Vec<u8>> {
    let path = c_path(path)?;
    let name = c_name(name)?;
    #[cfg(target_os = "macos")]
    {
        xattr_buffer(|buf, size| {
            // SAFETY: arguments remain valid for the duration of the call.
            unsafe {
                libc::getxattr(
                    path.as_ptr(),
                    name.as_ptr(),
                    buf,
                    size,
                    0,
                    libc::XATTR_NOFOLLOW,
                )
            }
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        xattr_buffer(|buf, size| {
            // SAFETY: arguments remain valid for the duration of the call.
            unsafe { libc::lgetxattr(path.as_ptr(), name.as_ptr(), buf, size) }
        })
    }
}

pub fn set_xattr(path: &Path, name: &OsStr, value: &[u8], flags: i32) -> io::Result<()> {
    let path = c_path(path)?;
    let name = c_name(name)?;
    #[cfg(target_os = "macos")]
    let rc = unsafe {
        // SAFETY: arguments remain valid for the duration of the call.
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            flags | libc::XATTR_NOFOLLOW,
        )
    };
    #[cfg(not(target_os = "macos"))]
    let rc = unsafe {
        // SAFETY: arguments remain valid for the duration of the call.
        libc::lsetxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            flags,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn remove_xattr(path: &Path, name: &OsStr) -> io::Result<()> {
    let path = c_path(path)?;
    let name = c_name(name)?;
    #[cfg(target_os = "macos")]
    let rc = unsafe {
        // SAFETY: arguments remain valid for the duration of the call.
        libc::removexattr(path.as_ptr(), name.as_ptr(), libc::XATTR_NOFOLLOW)
    };
    #[cfg(not(target_os = "macos"))]
    let rc = unsafe {
        // SAFETY: arguments remain valid for the duration of the call.
        libc::lremovexattr(path.as_ptr(), name.as_ptr())
    };
    cvt(rc)
}

pub fn copy_xattrs(source: &Path, destination: &Path) -> io::Result<()> {
    for name in list_xattrs(source)? {
        let name = OsStr::from_bytes(&name);
        let value = get_xattr(source, name)?;
        set_xattr(destination, name, &value, 0)?;
    }
    Ok(())
}

pub fn fsync(file: &File, datasync: bool) -> io::Result<()> {
    if datasync {
        file.sync_data()
    } else {
        file.sync_all()
    }
}

/// Never report sparse length extension as physical space reservation.
pub fn allocate(file: &File, offset: i64, length: i64) -> io::Result<()> {
    if offset < 0 || length <= 0 || offset.checked_add(length).is_none() {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    #[cfg(target_os = "linux")]
    {
        cvt(unsafe { libc::fallocate(file.as_raw_fd(), 0, offset, length) })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Err(io::Error::from_raw_os_error(libc::ENOTSUP))
    }
}

#[allow(clippy::too_many_arguments)]
pub fn set_file_metadata(
    file: &File,
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    size: Option<u64>,
    atime: Option<SystemTime>,
    mtime: Option<SystemTime>,
    flags: Option<u32>,
) -> io::Result<()> {
    if let Some(size) = size {
        file.set_len(size)?;
    }
    if uid.is_some() || gid.is_some() {
        cvt(unsafe {
            libc::fchown(
                file.as_raw_fd(),
                uid.unwrap_or(u32::MAX),
                gid.unwrap_or(u32::MAX),
            )
        })?;
    }
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(mode & 0o7777))?;
    }
    if atime.is_some() || mtime.is_some() {
        let omit = libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        };
        let times = [
            atime.map(timespec).unwrap_or(omit),
            mtime.map(timespec).unwrap_or(omit),
        ];
        cvt(unsafe { libc::futimens(file.as_raw_fd(), times.as_ptr()) })?;
    }
    if let Some(flags) = flags {
        #[cfg(target_os = "macos")]
        cvt(unsafe { libc::fchflags(file.as_raw_fd(), flags) })?;
        #[cfg(not(target_os = "macos"))]
        if flags != 0 {
            return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
        }
    }
    Ok(())
}

pub fn seek(file: &File, offset: i64, whence: i32) -> io::Result<i64> {
    // SAFETY: lseek only operates on the valid owned descriptor.
    let result = unsafe { libc::lseek(file.as_raw_fd(), offset, whence) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

#[cfg(target_os = "macos")]
pub fn set_flags(path: &Path, flags: u32) -> io::Result<()> {
    let path = c_path(path)?;
    // SAFETY: path is a valid NUL-terminated string.
    cvt(unsafe { libc::chflags(path.as_ptr(), flags) })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn negative_epoch_round_trips_integral_and_fractional_timestamps() {
        for (seconds, nanos) in [(-2, 0), (-2, 123_456_789), (0, 0)] {
            let value = timespec(unix_time(seconds, nanos));
            assert_eq!(value.tv_sec, seconds);
            assert_eq!(value.tv_nsec, nanos);
        }
    }
}

/// Materialize a fixed rootfs mountpoint without following image-provided links.
/// Each create/open is anchored to an owned directory descriptor.
pub fn prepare_rooted_path(
    root: &Path,
    relative: &Path,
    directory: bool,
    writable: bool,
) -> io::Result<File> {
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Component;
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(name) => c_name(name),
            _ => Err(io::Error::from_raw_os_error(libc::EINVAL)),
        })
        .collect::<io::Result<Vec<_>>>()?;
    if components.is_empty() && !directory {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    let mut parent = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)?;
    for (index, name) in components.iter().enumerate() {
        let is_dir = index + 1 != components.len() || directory;
        if is_dir {
            let rc = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) };
            if rc != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
                return Err(io::Error::last_os_error());
            }
        }
        let flags = libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if is_dir {
                libc::O_RDONLY | libc::O_DIRECTORY
            } else {
                (if writable {
                    libc::O_RDWR
                } else {
                    libc::O_RDONLY
                }) | libc::O_CREAT
                    | libc::O_NONBLOCK
            };
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags, 0o644) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        parent = unsafe { File::from_raw_fd(fd) };
        if !is_dir && !parent.metadata()?.is_file() {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
    }
    Ok(parent)
}

#[cfg(test)]
mod rooted_tests {
    use super::*;
    #[test]
    fn rooted_preparation_rejects_links_and_type_conflicts_without_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let outside = temp.path().join("sentinel");
        std::fs::write(&outside, b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        assert!(prepare_rooted_path(&root, Path::new("link"), false, false).is_err());
        std::os::unix::fs::symlink(temp.path(), root.join("ancestor")).unwrap();
        assert!(prepare_rooted_path(&root, Path::new("ancestor/new"), false, false).is_err());
        assert!(prepare_rooted_path(&root, Path::new("../sentinel"), false, false).is_err());
        let file = prepare_rooted_path(&root, Path::new("opt/pvisor"), false, false).unwrap();
        assert!(file.metadata().unwrap().is_file());
        assert!(prepare_rooted_path(&root, Path::new("opt/pvisor"), true, false).is_err());
        assert_eq!(std::fs::read(outside).unwrap(), b"keep");
    }
}
