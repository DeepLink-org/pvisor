//! Container IO plumbing for the init child: bundle FIFOs, the console
//! socket, and PTY allocation.
//!
//! Everything here runs inside the init child between fork and pivot_root,
//! where the host path namespace is still visible.

use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::path::Path;

use anyhow::{Context, Result};
use log::warn;

fn cstr(path: &str) -> std::ffi::CString {
    std::ffi::CString::new(path).expect("path has no interior NUL")
}

fn dev_null(label: &str) -> Result<File> {
    File::open("/dev/null").with_context(|| format!("open /dev/null for {label}"))
}

unsafe fn file_from_raw(fd: libc::c_int) -> File {
    unsafe { File::from_raw_fd(fd) }
}

/// Open the container stdin FIFO.
///
/// `O_RDWR` keeps a writer end open so readers do not observe EOF when
/// containerd briefly detaches (the runc trick).
pub fn open_stdin(path: Option<&str>) -> Result<File> {
    let Some(path) = path else {
        return dev_null("stdin");
    };
    let fd = loop {
        let fd = unsafe { libc::open(cstr(path).as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd >= 0 {
            break fd;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error).with_context(|| format!("open stdin fifo {path}"));
    };
    Ok(unsafe { file_from_raw(fd) })
}

/// Open a container output FIFO for writing, retrying while containerd's
/// reader is not up yet; falls back to /dev/null for detached tasks.
pub fn open_output(kind: &str, path: Option<&str>) -> Result<File> {
    let Some(path) = path else {
        return dev_null(kind);
    };
    for _ in 0..50 {
        let fd = unsafe {
            libc::open(
                cstr(path).as_ptr(),
                libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd >= 0 {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0 {
                let error = std::io::Error::last_os_error();
                unsafe { libc::close(fd) };
                return Err(error).with_context(|| format!("clear O_NONBLOCK on {kind}"));
            }
            return Ok(unsafe { file_from_raw(fd) });
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENXIO) {
            std::thread::sleep(std::time::Duration::from_millis(20));
            continue;
        }
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error).with_context(|| format!("open {kind} fifo {path}"));
    }
    warn!("{kind} fifo {path} had no reader; using /dev/null");
    dev_null(kind)
}

/// Allocate a PTY for a terminal task; returns (master, slave).
pub fn open_pty(rows: u16, cols: u16) -> Result<(File, File)> {
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ret = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &size,
        )
    };
    if ret != 0 {
        return Err(std::io::Error::last_os_error()).context("openpty");
    }
    // Both fds keep CLOEXEC: the master is sent over the console socket, the
    // slave is dup2'ed onto 0/1/2 before exec.
    unsafe {
        libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(slave, libc::F_SETFD, libc::FD_CLOEXEC);
    }
    Ok(unsafe { (file_from_raw(master), file_from_raw(slave)) })
}

/// Hand the PTY master to containerd through the console socket via
/// `SCM_RIGHTS`, the runc/console protocol.
pub fn send_console_master(socket_path: &str, master: &File) -> Result<()> {
    let mut stream = UnixStream::connect(Path::new(socket_path))
        .with_context(|| format!("connect console socket {socket_path}"))?;
    let fd = master.as_raw_fd();
    // One dummy byte carries the ancillary data.
    let control_len =
        unsafe { libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as u32) } as usize;
    let mut control = vec![0u8; control_len];
    let sent = unsafe {
        let mut header: libc::msghdr = std::mem::zeroed();
        let mut payload: libc::iovec = libc::iovec {
            iov_base: b"1".as_ptr() as *mut libc::c_void,
            iov_len: 1,
        };
        header.msg_iov = &mut payload;
        header.msg_iovlen = 1;
        header.msg_control = control.as_mut_ptr().cast();
        header.msg_controllen = control.len();
        let cmsg = libc::CMSG_FIRSTHDR(&header);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as u32) as usize;
        (*(libc::CMSG_DATA(cmsg) as *mut libc::c_int)) = fd;
        libc::sendmsg(stream.as_raw_fd(), &header, 0)
    };
    if sent < 0 {
        return Err(std::io::Error::last_os_error()).context("send console master fd");
    }
    stream.flush().ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_paths_fall_back_to_dev_null() {
        // /dev/null is one of the few paths guaranteed to exist on dev hosts.
        let stdin = open_stdin(None).expect("stdin");
        assert!(stdin.as_raw_fd() >= 0);
        let out = open_output("stdout", None).expect("stdout");
        assert!(out.as_raw_fd() >= 0);
    }
}
