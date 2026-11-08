//! Linux sandbox-internal exec broker. Workloads never inherit its channel.
//! The PID-namespace init has already installed the original Landlock/network
//! policy; every exec child inherits that policy, cwd, environment and limits.
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Arc;

const MAX_PACKET: usize = 64 * 1024;
const MAX_JOBS: usize = 4;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub argv: Vec<String>,
    pub tty: bool,
}

impl Request {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            !self.argv.is_empty() && self.argv.len() <= 256,
            "exec needs 1–256 arguments"
        );
        ensure!(
            !self.argv[0].is_empty() && self.argv.iter().all(|s| !s.contains('\0')),
            "invalid exec arguments"
        );
        ensure!(
            serde_json::to_vec(self)?.len() < MAX_PACKET,
            "exec request too large"
        );
        Ok(())
    }
}

pub(crate) struct Broker {
    socket: UnixDatagram,
}

impl Broker {
    pub(crate) fn pair() -> anyhow::Result<(Arc<Self>, UnixDatagram)> {
        let (socket, child) = UnixDatagram::pair()?;
        socket.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
        Ok((Arc::new(Self { socket }), child))
    }

    pub(crate) fn start(&self, request: &Request, stdio: &[OwnedFd]) -> anyhow::Result<UnixStream> {
        request.validate()?;
        ensure!(
            stdio.len() == 3,
            "exec needs exactly three stdio descriptors"
        );
        let (reply, child) = UnixStream::pair()?;
        let fds = [
            child.as_raw_fd(),
            stdio[0].as_raw_fd(),
            stdio[1].as_raw_fd(),
            stdio[2].as_raw_fd(),
        ];
        send(self.socket.as_raw_fd(), &serde_json::to_vec(request)?, &fds)?;
        Ok(reply)
    }
}

/// A bounded ancillary transfer. No pathname or PID can substitute for an FD.
pub(crate) fn send(socket: RawFd, bytes: &[u8], fds: &[RawFd]) -> anyhow::Result<()> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_PACKET && fds.len() <= 4,
        "invalid exec packet"
    );
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    // usize storage provides cmsghdr alignment; the bound fits four descriptors.
    let mut control = [0usize; 16];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    if !fds.is_empty() {
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = unsafe { libc::CMSG_SPACE(std::mem::size_of_val(fds) as u32) } as _;
        unsafe {
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as u32) as _;
            std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(cmsg).cast(), fds.len());
        }
    }
    let count = unsafe { libc::sendmsg(socket, &msg, libc::MSG_NOSIGNAL) };
    if count < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    ensure!(
        count as usize == bytes.len(),
        "short exec packet write; not retried"
    );
    Ok(())
}

pub(crate) fn receive(socket: RawFd, limit: usize) -> anyhow::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut bytes = vec![0u8; limit];
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut control = [0usize; 16];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = std::mem::size_of_val(&control) as _;
    let count = unsafe { libc::recvmsg(socket, &mut msg, libc::MSG_CMSG_CLOEXEC) };
    if count < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut fds = Vec::new();
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let size = ((*cmsg).cmsg_len as usize).saturating_sub(libc::CMSG_LEN(0) as usize);
                for index in 0..size / std::mem::size_of::<RawFd>() {
                    fds.push(OwnedFd::from_raw_fd(
                        *libc::CMSG_DATA(cmsg).cast::<RawFd>().add(index),
                    ));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    ensure!(
        count > 0 && msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) == 0,
        "closed or truncated exec packet"
    );
    ensure!(fds.len() <= 4, "too many exec descriptors");
    bytes.truncate(count as usize);
    Ok((bytes, fds))
}

fn reply(stream: &mut UnixStream, value: serde_json::Value) {
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(1)));
    let _ = crate::host_transport::write_host_frame_sync(stream, &value);
}

fn spawn(request: Request, fds: Vec<OwnedFd>) -> anyhow::Result<libc::pid_t> {
    request.validate()?;
    ensure!(fds.len() == 3, "exec stdio transfer incomplete");
    let mut fds = fds.into_iter();
    let mut command = Command::new(&request.argv[0]);
    command
        .args(&request.argv[1..])
        .stdin(Stdio::from(fds.next().unwrap()))
        .stdout(Stdio::from(fds.next().unwrap()))
        .stderr(Stdio::from(fds.next().unwrap()));
    if request.tty {
        if std::env::var_os("TERM").is_none() {
            command.env("TERM", "xterm-256color");
        }
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    } else {
        command.process_group(0);
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    // Command::spawn replaces signal handlers on exec. The broker is single
    // threaded and all inherited control descriptors are CLOEXEC.
    let child = command
        .spawn()
        .context("spawn command inside existing sandbox")?;
    Ok(child.id() as libc::pid_t)
}

// A per-exec subreaper owns only its own children, including double-forked
// descendants. Keep it alive through cleanup; never signal a recorded host PID.
fn cleanup_children() {
    loop {
        let path = format!("/proc/self/task/{}/children", unsafe { libc::getpid() });
        if let Ok(children) = std::fs::read_to_string(path) {
            for pid in children
                .split_whitespace()
                .filter_map(|p| p.parse::<libc::pid_t>().ok())
            {
                let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as libc::c_int;
                if fd < 0 {
                    continue;
                }
                let fd = unsafe { OwnedFd::from_raw_fd(fd) };
                let parent = std::fs::read_to_string(format!("/proc/{pid}/status"))
                    .ok()
                    .and_then(|s| {
                        s.lines().find_map(|line| {
                            line.strip_prefix("PPid:")
                                .and_then(|p| p.trim().parse::<libc::pid_t>().ok())
                        })
                    });
                if parent == Some(unsafe { libc::getpid() }) {
                    unsafe {
                        libc::syscall(
                            libc::SYS_pidfd_send_signal,
                            fd.as_raw_fd(),
                            libc::SIGKILL,
                            std::ptr::null::<libc::siginfo_t>(),
                            0,
                        );
                    }
                }
            }
        }
        let mut status = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD) {
            return;
        }
        if pid <= 0 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

fn worker(request: Request, fds: Vec<OwnedFd>, mut response: UnixStream) -> ! {
    if super::sandbox::install_namespace_signal_handlers(libc::SIG_DFL).is_err() {
        unsafe { libc::_exit(125) };
    }
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) } != 0 {
        reply(
            &mut response,
            serde_json::json!({"error":"exec subreaper unavailable"}),
        );
        unsafe { libc::_exit(125) };
    }
    // Refuse before executing user code when this sandbox cannot perform the
    // descriptor-based descendant cleanup required by disconnect semantics.
    let probe = (|| -> anyhow::Result<()> {
        std::fs::read_to_string(format!("/proc/self/task/{}/children", unsafe {
            libc::getpid()
        }))?;
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0) } as libc::c_int;
        ensure!(fd >= 0, "pidfd cleanup unavailable");
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        ensure!(
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    0,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            } == 0,
            "pidfd signaling unavailable"
        );
        Ok(())
    })();
    if let Err(error) = probe {
        reply(
            &mut response,
            serde_json::json!({"error":error.to_string()}),
        );
        unsafe { libc::_exit(125) };
    }
    let child = match spawn(request, fds) {
        Ok(pid) => pid,
        Err(error) => {
            reply(
                &mut response,
                serde_json::json!({"error":error.to_string()}),
            );
            unsafe { libc::_exit(125) };
        }
    };
    reply(&mut response, serde_json::json!({"started":true}));
    loop {
        let mut status = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid == child {
            let code = if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else {
                128 + libc::WTERMSIG(status)
            };
            cleanup_children();
            reply(&mut response, serde_json::json!({"code":code}));
            unsafe { libc::_exit(0) };
        }
        let mut pfd = libc::pollfd {
            fd: response.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe {
            libc::poll(&mut pfd, 1, 50);
        }
        if pfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLIN) != 0 {
            cleanup_children();
            unsafe { libc::_exit(0) };
        }
    }
}

/// Runs only in the existing sandbox PID 1. Ending PID 1 is the kernel-owned
/// cleanup boundary for the main workload, all exec jobs and daemonized children.
pub(crate) fn supervise(agent: libc::pid_t, socket: UnixDatagram) -> ! {
    let mut jobs: Vec<libc::pid_t> = Vec::new();
    loop {
        let mut status = 0;
        loop {
            let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if pid == agent {
                super::sandbox::exit_with_wait_status(status);
            }
            if pid <= 0 {
                break;
            }
            jobs.retain(|job| *job != pid);
        }
        let mut pfd = libc::pollfd {
            fd: socket.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut pfd, 1, 50) } <= 0 || pfd.revents & libc::POLLIN == 0 {
            continue;
        }
        let Ok((bytes, mut fds)) = receive(socket.as_raw_fd(), MAX_PACKET) else {
            continue;
        };
        if fds.len() != 4 {
            continue;
        }
        let mut response = UnixStream::from(fds.remove(0));
        let result = (|| -> anyhow::Result<Request> {
            ensure!(jobs.len() < MAX_JOBS, "Session exec limit reached");
            let request: Request = serde_json::from_slice(&bytes)?;
            request.validate()?;
            Ok(request)
        })();
        match result {
            Ok(request) => {
                let pid = unsafe { libc::fork() };
                if pid == 0 {
                    unsafe {
                        libc::close(socket.as_raw_fd());
                    }
                    worker(request, fds, response);
                }
                if pid > 0 {
                    jobs.push(pid);
                } else {
                    reply(
                        &mut response,
                        serde_json::json!({"error":"exec supervisor fork failed"}),
                    );
                }
            }
            Err(error) => reply(
                &mut response,
                serde_json::json!({"error":error.to_string()}),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_bounds_and_ancillary_ownership() {
        for argv in [
            vec![],
            vec!["".into()],
            vec!["a\0b".into()],
            vec!["x".repeat(MAX_PACKET)],
        ] {
            assert!(Request { argv, tty: false }.validate().is_err());
        }
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        let file = std::fs::File::open("/dev/null").unwrap();
        send(sender.as_raw_fd(), b"E", &[file.as_raw_fd()]).unwrap();
        let (bytes, descriptors) = receive(receiver.as_raw_fd(), 1).unwrap();
        assert_eq!(bytes, b"E");
        assert_eq!(descriptors.len(), 1);
        let flags = unsafe { libc::fcntl(descriptors[0].as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        drop(descriptors);
        assert!(
            file.metadata().is_ok(),
            "transfer must not consume sender ownership"
        );
        send(sender.as_raw_fd(), b"too long", &[file.as_raw_fd()]).unwrap();
        assert!(receive(receiver.as_raw_fd(), 1).is_err());
        assert!(send(sender.as_raw_fd(), b"E", &[file.as_raw_fd(); 5]).is_err());
    }
}
