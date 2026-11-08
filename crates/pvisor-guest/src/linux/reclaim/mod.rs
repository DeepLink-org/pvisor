//! Cooperative preload worker control. No target suspension or code injection.
use pvisor_guest::GuestConfig;
use serde_json::{Value, json};
use std::{
    fs, io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{fs::MetadataExt, net::UnixStream, process::CommandExt},
    },
    process::Command,
    time::{Duration, Instant},
};

const MAGIC: u32 = 0x50565231;
const LIMIT: Duration = Duration::from_secs(2);
const ENV: &str = "PVISOR_RECLAIM_CHANNEL";

pub(super) struct Channel {
    socket: UnixStream,
    sequence: u64,
}

fn pair() -> io::Result<(Channel, OwnedFd)> {
    let mut fds = [-1; 2];
    // SAFETY: fds is a writable array; successful descriptors get unique owners below.
    super::check(unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    })?;
    // UnixStream's ownership/descriptor operations also support this seqpacket socket.
    // SAFETY: socketpair returned two new, distinct descriptors.
    let socket = unsafe { UnixStream::from_raw_fd(fds[0]) };
    let child = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    let enabled = 1i32;
    // SAFETY: enabled is a valid call-local option buffer.
    super::check(unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PASSCRED,
            (&enabled as *const i32).cast(),
            4,
        )
    })?;
    Ok((
        Channel {
            socket,
            sequence: 0,
        },
        child,
    ))
}

pub(super) fn configure(
    config: &GuestConfig,
    command: &mut Command,
) -> io::Result<Option<Channel>> {
    command.env_remove(ENV);
    let Some(path) = config
        .process
        .as_ref()
        .and_then(|options| options.allocator_reclaim.as_ref())
    else {
        return Ok(None);
    };
    if !fs::metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reclaim SO is not a regular file",
        ));
    }
    let (channel, child) = pair()?;
    let inode = fs::metadata(format!("/proc/self/fd/{}", child.as_raw_fd()))?.ino();
    let preload = config
        .env
        .get("LD_PRELOAD")
        .filter(|value| !value.is_empty());
    command.env(
        "LD_PRELOAD",
        match preload {
            Some(value) => format!("{value}:{}", path.display()),
            None => path.display().to_string(),
        },
    );
    command.env(ENV, format!("{}:{inode}:0", child.as_raw_fd()));
    // SAFETY: pre_exec only clears CLOEXEC using fcntl; the closure owns the descriptor.
    unsafe {
        command.pre_exec(move || super::check(libc::fcntl(child.as_raw_fd(), libc::F_SETFD, 0)))
    };
    Ok(Some(channel))
}

fn rss(pid: u32) -> Option<u64> {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("RssAnon:")?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()?
                .checked_mul(1024)
        })
}

impl Channel {
    fn exchange(&mut self, pid: u32, trim: bool) -> io::Result<Value> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| io::Error::other("reclaim sequence exhausted"))?;
        let mut request = [0u8; 16];
        request[..4].copy_from_slice(&MAGIC.to_le_bytes());
        request[4..8].copy_from_slice(&u32::from(trim).to_le_bytes());
        request[8..].copy_from_slice(&self.sequence.to_le_bytes());
        let before = rss(pid);
        // SAFETY: request is readable for its full length; MSG_NOSIGNAL preserves workload signals.
        let sent = unsafe {
            libc::send(
                self.socket.as_raw_fd(),
                request.as_ptr().cast(),
                request.len(),
                libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
            )
        };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        if sent != request.len() as isize {
            return Err(io::Error::other("short reclaim request"));
        }
        let deadline = Instant::now() + LIMIT;
        let mut response = [0u8; 32];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "reclaim worker timed out; channel retired, workload continues",
                ));
            }
            let mut poll = libc::pollfd {
                fd: self.socket.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll is a valid single-entry output buffer.
            if unsafe { libc::poll(&mut poll, 1, remaining.as_millis().max(1) as i32) } < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            let mut control = [0usize; 8];
            let mut iov = libc::iovec {
                iov_base: response.as_mut_ptr().cast(),
                iov_len: response.len(),
            };
            // SAFETY: all-zero msghdr is valid before assigning its writable buffers.
            let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
            message.msg_iov = &mut iov;
            message.msg_iovlen = 1;
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen = std::mem::size_of_val(&control) as _;
            // SAFETY: message's data and aligned ancillary buffers live through recvmsg.
            let size = unsafe {
                libc::recvmsg(
                    self.socket.as_raw_fd(),
                    &mut message,
                    libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
                )
            };
            if size < 0 {
                let error = io::Error::last_os_error();
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) {
                    continue;
                }
                return Err(error);
            }
            let mut sender = None;
            let mut rights = false;
            // SAFETY: CMSG traversal stays in the ancillary buffer initialized by recvmsg.
            unsafe {
                let mut header = libc::CMSG_FIRSTHDR(&message);
                while !header.is_null() {
                    if (*header).cmsg_level == libc::SOL_SOCKET
                        && (*header).cmsg_type == libc::SCM_CREDENTIALS
                        && (*header).cmsg_len as usize
                            == libc::CMSG_LEN(std::mem::size_of::<libc::ucred>() as _) as usize
                    {
                        sender = Some(
                            std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<libc::ucred>())
                                .pid,
                        );
                    }
                    // Never accept passed descriptors from a cooperative worker.
                    if (*header).cmsg_level == libc::SOL_SOCKET
                        && (*header).cmsg_type == libc::SCM_RIGHTS
                    {
                        let count = ((*header).cmsg_len as usize)
                            .saturating_sub(libc::CMSG_LEN(0) as usize)
                            / 4;
                        for index in 0..count {
                            libc::close(std::ptr::read_unaligned(
                                libc::CMSG_DATA(header).cast::<i32>().add(index),
                            ));
                        }
                        rights = true;
                    }
                    header = libc::CMSG_NXTHDR(&message, header);
                }
            }
            let u32_at =
                |offset| u32::from_le_bytes(response[offset..offset + 4].try_into().unwrap());
            if size != 32
                || rights
                || message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0
                || sender != Some(pid as i32)
                || u32_at(0) != MAGIC
                || u32_at(8) != pid
                || u64::from_le_bytes(response[16..24].try_into().unwrap()) != self.sequence
            {
                return Err(io::Error::other("invalid or disconnected reclaim worker"));
            }
            let status = u32_at(4);
            if status == 1 {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "target malloc provider is not confirmed glibc",
                ));
            }
            let result = u32_at(24);
            if status != 0 || !matches!(result, 0 | 1) || u32_at(12) == 0 || u32_at(28) != 1 {
                return Err(io::Error::other("invalid reclaim worker status"));
            }
            let mut report = json!({"status":if trim {"trimmed"} else {"inspected"}, "pid":pid, "allocator":"glibc", "worker_tid":u32_at(12)});
            if trim {
                report["malloc_trim_result"] = json!(result);
                report["rss_anon_before_bytes"] = json!(before);
                report["rss_anon_after_bytes"] = json!(rss(pid));
            }
            return Ok(report);
        }
    }
}

pub(super) fn run(channel: &mut Option<Channel>, pid: u32, trim: bool) -> io::Result<Value> {
    let worker = channel.as_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "allocator reclaim preload channel unavailable",
        )
    })?;
    let result = worker.exchange(pid, trim);
    if result.is_err() {
        *channel = None;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(
        mut channel: Option<Channel>,
        peer: OwnedFd,
        fault: u8,
        expected_pid: u32,
    ) -> (Option<Channel>, io::Result<Value>) {
        let worker = std::thread::spawn(move || {
            let mut request = [0u8; 16];
            // SAFETY: valid descriptor and full writable request buffer.
            assert_eq!(
                unsafe { libc::recv(peer.as_raw_fd(), request.as_mut_ptr().cast(), 16, 0) },
                16
            );
            let mut response = [0u8; 32];
            response[..4].copy_from_slice(&MAGIC.to_le_bytes());
            response[8..12].copy_from_slice(&expected_pid.to_le_bytes());
            response[12..16].copy_from_slice(&1u32.to_le_bytes());
            response[16..24].copy_from_slice(&request[8..16]);
            response[28..].copy_from_slice(&1u32.to_le_bytes());
            if fault == 2 {
                response[16] ^= 1;
            }
            if fault == 3 {
                let file = fs::File::open("/dev/null").unwrap();
                let mut control = [0usize; 4];
                let mut iov = libc::iovec {
                    iov_base: response.as_mut_ptr().cast(),
                    iov_len: response.len(),
                };
                // SAFETY: writable, aligned ancillary storage and a readable owned fd.
                unsafe {
                    let mut message: libc::msghdr = std::mem::zeroed();
                    message.msg_iov = &mut iov;
                    message.msg_iovlen = 1;
                    message.msg_control = control.as_mut_ptr().cast();
                    message.msg_controllen = libc::CMSG_SPACE(4) as _;
                    let header = libc::CMSG_FIRSTHDR(&message);
                    (*header).cmsg_level = libc::SOL_SOCKET;
                    (*header).cmsg_type = libc::SCM_RIGHTS;
                    (*header).cmsg_len = libc::CMSG_LEN(4) as _;
                    std::ptr::write_unaligned(
                        libc::CMSG_DATA(header).cast::<i32>(),
                        file.as_raw_fd(),
                    );
                    assert_eq!(
                        libc::sendmsg(peer.as_raw_fd(), &message, libc::MSG_NOSIGNAL),
                        32
                    );
                }
                return;
            }
            // SAFETY: full readable response buffer and a connected socket descriptor.
            unsafe {
                libc::send(
                    peer.as_raw_fd(),
                    response.as_ptr().cast(),
                    if fault == 1 { 31 } else { 32 },
                    libc::MSG_NOSIGNAL,
                )
            };
        });
        let result = run(&mut channel, expected_pid, false);
        worker.join().unwrap();
        (channel, result)
    }

    #[test]
    fn protocol_checks_packet_size_sequence_and_kernel_sender() {
        // SAFETY: getpid has no pointer arguments.
        let pid = unsafe { libc::getpid() } as u32;
        for (fault, expected_pid) in [(0, pid), (1, pid), (0, pid + 1), (2, pid), (3, pid)] {
            let fds = fs::read_dir("/proc/self/fd").unwrap().count();
            let (channel, peer) = pair().unwrap();
            let (channel, result) = reply(Some(channel), peer, fault, expected_pid);
            if fault == 0 && expected_pid == pid {
                assert_eq!(result.unwrap()["status"], "inspected");
                assert!(channel.is_some());
            } else {
                assert!(result.is_err());
                assert!(channel.is_none());
                assert_eq!(fs::read_dir("/proc/self/fd").unwrap().count(), fds);
            }
        }
    }

    #[test]
    fn timeout_retires_channel_without_target_signals() {
        let (channel, _peer) = pair().unwrap();
        let mut channel = Some(channel);
        // SAFETY: this is the test process, which remains alive throughout the exchange.
        let error = run(&mut channel, unsafe { libc::getpid() } as u32, true).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(channel.is_none());
    }

    #[test]
    fn launch_without_optin_scrubs_binding_and_creates_no_channel() {
        let config = GuestConfig {
            env: [(ENV.into(), "3:1:0".into())].into(),
            ..Default::default()
        };
        let mut command = Command::new("/bin/true");
        assert!(configure(&config, &mut command).unwrap().is_none());
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == ENV && value.is_none())
        );
        assert!(!command.get_envs().any(|(key, _)| key == "LD_PRELOAD"));
    }
}
