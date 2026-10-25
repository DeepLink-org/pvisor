//! Guest metadata for private, host-account-owned managed directories.
use std::{
    ffi::CString,
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd},
};

pub(super) const KEY: &std::ffi::CStr = c"user.containers.override_stat";

fn path(file: &File) -> CString {
    CString::new(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap()
}

pub(super) fn attributes(file: &File, mut st: libc::stat64) -> io::Result<libc::stat64> {
    st.st_uid = 0;
    st.st_gid = 0;
    if !matches!(st.st_mode & libc::S_IFMT, libc::S_IFREG | libc::S_IFDIR) {
        return Ok(st);
    }
    let mut buf = [0u8; 128];
    // SAFETY: both C strings and the bounded output buffer live across the call.
    let n = unsafe {
        libc::getxattr(
            path(file).as_ptr(),
            KEY.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if n < 0 {
        let e = io::Error::last_os_error();
        return if e.raw_os_error() == Some(libc::ENODATA) {
            Ok(st)
        } else {
            Err(e)
        };
    }
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "invalid managed inode metadata");
    let text = std::str::from_utf8(&buf[..n as usize]).map_err(|_| bad())?;
    let mut fields = text.split(':');
    st.st_uid = fields.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
    st.st_gid = fields.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
    let mode = u32::from_str_radix(fields.next().ok_or_else(bad)?, 8).map_err(|_| bad())?;
    let kind = mode & libc::S_IFMT;
    if fields.next().is_some()
        || mode & !(libc::S_IFMT | 0o7777) != 0
        || (kind != 0 && kind != st.st_mode & libc::S_IFMT)
    {
        return Err(bad());
    }
    st.st_mode = (st.st_mode & libc::S_IFMT) | (mode & 0o7777);
    Ok(st)
}

/// A fresh open description makes flock serialize metadata changes across VMs
/// and hardlink aliases. The caller retains this file until its update finishes.
pub(super) fn lock(file: &File) -> io::Result<File> {
    // SAFETY: the pinned inode's proc path and flags are valid; result is owned.
    let fd = unsafe {
        libc::open(
            path(file).as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    loop {
        if unsafe { libc::flock(fd, libc::LOCK_EX) } == 0 {
            return Ok(file);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

pub(super) fn store(file: &File, st: &libc::stat64) -> io::Result<()> {
    let mode = match st.st_mode & libc::S_IFMT {
        libc::S_IFDIR => 0o700,
        libc::S_IFREG => 0o600,
        _ => return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP)),
    };
    let path = path(file);
    let value = format!("{}:{}:0{:o}", st.st_uid, st.st_gid, st.st_mode);
    // SAFETY: the pinned proc link follows the owned inode, never a guest symlink.
    if unsafe { libc::chmod(path.as_ptr(), mode) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe {
        libc::setxattr(
            path.as_ptr(),
            KEY.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn kill_priv(file: &File) -> io::Result<()> {
    let _guard = lock(file)?;
    let mut native = std::mem::MaybeUninit::uninit();
    if unsafe { libc::fstat64(file.as_raw_fd(), native.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut st = attributes(file, unsafe { native.assume_init() })?;
    if st.st_mode & 0o6000 != 0 {
        st.st_mode &= !0o6000;
        store(file, &st)?;
    }
    Ok(())
}
