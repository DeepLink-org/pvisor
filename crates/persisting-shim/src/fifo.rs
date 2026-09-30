//! Container IO plumbing.
//!
//! The shim owns the task IO (the role runtime-v2 assigns to it): it opens
//! the bundle FIFOs or allocates the PTY at Create/Exec time, keeps the
//! keepalive/master ends for `CloseIO` and `ResizePty`, and passes the
//! workload-facing descriptors on to the init/exec child through fd
//! inheritance. The low-level helpers also serve the children directly when
//! a path-based open is needed.

use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::path::Path;

use anyhow::{Context, Result};
use log::warn;

use crate::plan::IoPlan;

fn cstr(path: &str) -> std::ffi::CString {
    std::ffi::CString::new(path).expect("path has no interior NUL")
}

unsafe fn file_from_raw(fd: libc::c_int) -> File {
    unsafe { File::from_raw_fd(fd) }
}

/// Open a FIFO without `O_CLOEXEC` so the descriptor survives into the
/// re-exec'd children.
unsafe fn open_fifo(path: &str, flags: libc::c_int) -> Result<File> {
    let fd = loop {
        let fd = unsafe { libc::open(cstr(path).as_ptr(), flags) };
        if fd >= 0 {
            break fd;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error).with_context(|| format!("open fifo {path}"));
    };
    Ok(unsafe { file_from_raw(fd) })
}

/// Open the workload stdin FIFO read-only. Must be called after a keepalive
/// writer exists (see [`ContainerIo`]) so the open cannot block.
pub fn open_child_stdin(path: Option<&str>) -> Result<File> {
    match path {
        None => File::open("/dev/null").context("open /dev/null for stdin"),
        Some(path) => unsafe { open_fifo(path, libc::O_RDONLY) },
    }
}

/// Open a container output FIFO for writing, retrying while the reader on
/// the other side is not up yet; falls back to /dev/null for detached IO.
pub fn open_output(kind: &str, path: Option<&str>) -> Result<File> {
    let Some(path) = path else {
        return File::open("/dev/null").with_context(|| format!("open /dev/null for {kind}"));
    };
    for _ in 0..50 {
        let fd = unsafe { libc::open(cstr(path).as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
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
    File::open("/dev/null").with_context(|| format!("open /dev/null for {kind}"))
}

/// Allocate a PTY; returns (master, slave).
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
    Ok(unsafe { (file_from_raw(master), file_from_raw(slave)) })
}

/// Hand a PTY master to containerd through the console socket via
/// `SCM_RIGHTS` (the runc/console protocol), retrying while the client's
/// listener is coming up.
pub fn send_console_master(socket_path: &str, master: &File) -> Result<()> {
    let mut last_error: Option<anyhow::Error> = None;
    for _ in 0..5 {
        match send_fd_once(socket_path, master) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("console send failed")))
}

fn send_fd_once(socket_path: &str, master: &File) -> Result<()> {
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
        header.msg_controllen = control.len() as _;
        let cmsg = libc::CMSG_FIRSTHDR(&header);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as u32) as _;
        (*(libc::CMSG_DATA(cmsg) as *mut libc::c_int)) = fd;
        libc::sendmsg(stream.as_raw_fd(), &header, 0)
    };
    if sent < 0 {
        return Err(std::io::Error::last_os_error()).context("send console master fd");
    }
    stream.flush().ok();
    Ok(())
}

/// Shim-owned IO for one process (init or exec).
pub struct ContainerIo {
    /// Workload stdin read end handed to the child.
    stdin: Option<File>,
    /// Shim-held `O_RDWR` copy of the stdin FIFO; dropping it (CloseIO) is
    /// what lets the workload observe EOF once containerd's writer leaves.
    stdin_keepalive: Option<File>,
    stdout: Option<File>,
    stderr: Option<File>,
    /// PTY master retained for ResizePty (terminal tasks only).
    master: Option<File>,
    terminal: bool,
}

impl ContainerIo {
    /// Open the IO described by `io`; for terminal tasks `io.stdout` is the
    /// console socket path (containerd convention) and receives the PTY
    /// master.
    pub fn open(io: &IoPlan) -> Result<ContainerIo> {
        if io.terminal {
            let console_socket = io
                .stdout
                .as_deref()
                .context("terminal task without console socket")?;
            let (master, slave) = open_pty(0, 0)?;
            send_console_master(console_socket, &master)?;
            let keepalive_master = master.try_clone().context("dup pty master")?;
            let stdin = slave.try_clone().context("dup pty slave")?;
            let stderr = slave.try_clone().context("dup pty slave")?;
            return Ok(ContainerIo {
                stdin: Some(stdin),
                stdin_keepalive: None,
                stdout: Some(slave),
                stderr: Some(stderr),
                master: Some(keepalive_master),
                terminal: true,
            });
        }
        // The keepalive writer must exist before the child's O_RDONLY open,
        // otherwise the open would block waiting for a writer.
        let stdin_keepalive = match io.stdin.as_deref() {
            Some(path) => Some(unsafe { open_fifo(path, libc::O_RDWR | libc::O_NONBLOCK) }?),
            None => None,
        };
        let stdin = open_child_stdin(io.stdin.as_deref())?;
        let stdout = open_output("stdout", io.stdout.as_deref())?;
        let stderr = if io.stderr.as_deref() == io.stdout.as_deref() {
            stdout.try_clone().context("dup stdout fifo")?
        } else {
            open_output("stderr", io.stderr.as_deref())?
        };
        Ok(ContainerIo {
            stdin: Some(stdin),
            stdin_keepalive,
            stdout: Some(stdout),
            stderr: Some(stderr),
            master: None,
            terminal: false,
        })
    }

    /// The three descriptors the child should wire onto 0/1/2. For
    /// terminals all three are the PTY slave.
    pub fn child_fds(&self) -> (libc::c_int, libc::c_int, libc::c_int) {
        let stdin = self
            .stdin
            .as_ref()
            .map_or(libc::STDIN_FILENO, File::as_raw_fd);
        let stdout = self
            .stdout
            .as_ref()
            .map_or(libc::STDOUT_FILENO, File::as_raw_fd);
        let stderr = self
            .stderr
            .as_ref()
            .map_or(libc::STDERR_FILENO, File::as_raw_fd);
        (stdin, stdout, stderr)
    }

    /// Drop the workload-facing descriptors after the child has been
    /// spawned; it holds its own copies through fd inheritance.
    pub fn release_child_fds(&mut self) {
        self.stdin = None;
        self.stdout = None;
        self.stderr = None;
    }

    /// Take the workload-facing descriptors out (VM exec relays: no child
    /// process inherits them, the shim pumps the streams itself).
    pub fn take_child_fds(&mut self) -> Option<(File, File, File)> {
        Some((self.stdin.take()?, self.stdout.take()?, self.stderr.take()?))
    }

    /// CloseIO: release the shim-held stdin keepalive so the workload can
    /// see EOF. Idempotent.
    pub fn close_stdin(&mut self) {
        if self.stdin_keepalive.take().is_some() {
            log::debug!("stdin keepalive released (CloseIO)");
        }
    }

    /// ResizePty: resize the retained PTY master.
    pub fn resize(&self, width: u32, height: u32) -> Result<()> {
        let Some(master) = self.master.as_ref() else {
            anyhow::bail!("task is not a terminal task");
        };
        let size = libc::winsize {
            ws_row: height as u16,
            ws_col: width as u16,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) } != 0 {
            return Err(std::io::Error::last_os_error()).context("TIOCSWINSZ");
        }
        Ok(())
    }

    pub fn is_terminal(&self) -> bool {
        self.terminal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_paths_fall_back_to_dev_null() {
        // /dev/null is one of the few paths guaranteed to exist on dev hosts.
        let stdin = open_child_stdin(None).expect("stdin");
        assert!(stdin.as_raw_fd() >= 0);
        let out = open_output("stdout", None).expect("stdout");
        assert!(out.as_raw_fd() >= 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detached_io_end_to_end() {
        use crate::plan::IoPlan;

        let dir = tempfile::tempdir().expect("tempdir");
        let fifo = |name: &str| {
            let path = dir.path().join(name);
            let cpath = cstr(path.to_str().expect("utf8 path"));
            assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
            path.to_str().expect("utf8 path").to_string()
        };
        let io = IoPlan {
            terminal: false,
            stdin: Some(fifo("stdin")),
            stdout: Some(fifo("stdout")),
            stderr: Some(fifo("stderr")),
        };
        let mut owned = ContainerIo::open(&io).expect("open io");
        let (in_fd, out_fd, err_fd) = owned.child_fds();
        assert!(in_fd >= 0 && out_fd >= 0 && err_fd >= 0);
        assert!(owned.resize(80, 24).is_err(), "no master on pipes");
        owned.close_stdin();
    }
}
