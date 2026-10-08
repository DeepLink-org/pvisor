//! FD-bearing marker for the synchronous Job transport (not host_transport's JSON framing).
use anyhow::ensure;
use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    os::unix::net::UnixStream,
    sync::{Mutex, MutexGuard},
};

static FD_EXEC: Mutex<()> = Mutex::new(());
/// macOS has no MSG_CMSG_CLOEXEC. Every internal spawn and descriptor receipt
/// uses this lock until all newly installed descriptors have CLOEXEC.
pub(super) fn exec_guard() -> MutexGuard<'static, ()> {
    FD_EXEC.lock().unwrap_or_else(|e| e.into_inner())
}
pub(super) fn send(stream: &UnixStream, fds: &[RawFd]) -> anyhow::Result<()> {
    ensure!(
        !fds.is_empty() && fds.len() <= 253,
        "invalid descriptor count"
    );
    let mut marker = [0x46u8];
    let mut iov = libc::iovec {
        iov_base: marker.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let size = unsafe { libc::CMSG_SPACE(std::mem::size_of_val(fds) as u32) } as usize;
    let mut control = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = size as _;
    unsafe {
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as u32) as _;
        std::ptr::copy_nonoverlapping(
            fds.as_ptr().cast::<u8>(),
            libc::CMSG_DATA(c),
            std::mem::size_of_val(fds),
        );
        ensure!(
            libc::sendmsg(stream.as_raw_fd(), &msg, 0) == 1,
            "send descriptors: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}
pub(super) fn receive(stream: &UnixStream, expected: usize) -> anyhow::Result<Vec<OwnedFd>> {
    #[cfg(not(target_os = "linux"))]
    let _exec_guard = exec_guard();
    let mut marker = [0u8];
    let mut iov = libc::iovec {
        iov_base: marker.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 32];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = std::mem::size_of_val(&control) as _;
    #[cfg(target_os = "linux")]
    let flags = libc::MSG_CMSG_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let flags = 0;
    let n = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, flags) };
    if n < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut fds = Vec::new();
    let mut malformed = false;
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let length = (*c).cmsg_len as usize;
                let header = libc::CMSG_LEN(0) as usize;
                malformed |= length < header
                    || !length
                        .saturating_sub(header)
                        .is_multiple_of(std::mem::size_of::<RawFd>());
                // Adopt EVERY installed FD before any fallible validation or
                // flag operation. This includes rights accompanying bad markers
                // and the installed prefix of a truncated rights message.
                for i in 0..length.saturating_sub(header) / std::mem::size_of::<RawFd>() {
                    fds.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(
                        libc::CMSG_DATA(c).cast::<RawFd>().add(i),
                    )));
                }
            } else {
                malformed = true;
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
    }
    #[cfg(not(target_os = "linux"))]
    for fd in &fds {
        ensure!(
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
            "set descriptor CLOEXEC"
        );
    }
    ensure!(
        n == 1
            && !malformed
            && msg.msg_flags & libc::MSG_CTRUNC == 0
            && marker[0] == 0x46
            && fds.len() == expected,
        "invalid descriptor marker, truncated rights, or descriptor count"
    );
    Ok(fds)
}

#[cfg(test)]
mod frame_tests {
    use super::*;
    use pvisor::host_transport::{
        read_host_frame_sync as read_frame, write_host_frame_sync as write_frame,
    };

    #[test]
    fn worker_socket_stays_connected_after_acknowledged_transfer() {
        for _ in 0..1000 {
            let (mut sender, mut receiver) = UnixStream::pair().unwrap();
            let (mut authority, worker) = UnixStream::pair().unwrap();
            write_frame(&mut sender, &42u32).unwrap();
            send(&sender, &[worker.as_raw_fd()]).unwrap();
            let writer = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(1));
                write_frame(&mut authority, &"bootstrap").unwrap();
                authority
            });
            assert_eq!(read_frame::<u32>(&mut receiver).unwrap(), 42);
            let mut rights = receive(&receiver, 1).unwrap();
            let mut received = UnixStream::from(rights.remove(0));
            received
                .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                .unwrap();
            assert_eq!(read_frame::<String>(&mut received).unwrap(), "bootstrap");
            let mut authority = writer.join().unwrap();
            drop(worker);
            write_frame(&mut authority, &"after acknowledgment").unwrap();
            assert_eq!(
                read_frame::<String>(&mut received).unwrap(),
                "after acknowledgment"
            );
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{fs::File, io::Write};
    // Isolate the FD table from parallel tests; leak assertions are per child.
    fn isolated(name: &str, test: impl FnOnce()) {
        if std::env::var("PVISOR_FD_TEST_CHILD").as_deref() == Ok(name) {
            test();
            return;
        }
        let module = module_path!().split_once("::").unwrap().1;
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &format!("{module}::{name}"), "--test-threads=1"])
            .env("PVISOR_FD_TEST_CHILD", name)
            .status()
            .unwrap();
        assert!(status.success());
    }
    fn count() -> usize {
        std::fs::read_dir("/proc/self/fd").unwrap().count()
    }
    #[test]
    fn malformed_and_truncated_rights_never_leak() {
        isolated("malformed_and_truncated_rights_never_leak", || {
            let (a, b) = UnixStream::pair().unwrap();
            let file = File::open("/dev/null").unwrap();
            let before = count();
            send(&a, &[file.as_raw_fd(); 200]).unwrap();
            assert!(receive(&b, 3).is_err());
            assert_eq!(count(), before);
            send(&a, &[file.as_raw_fd(); 4]).unwrap();
            assert!(receive(&b, 3).is_err());
            assert_eq!(count(), before);
            // Rewrite the marker after receiving rights without consuming them.
            // sendmsg with a wrong marker tests the same RAII rejection path.
            let mut marker = [0x00u8];
            let mut iov = libc::iovec {
                iov_base: marker.as_mut_ptr().cast(),
                iov_len: 1,
            };
            let mut control = [0usize; 8];
            let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = unsafe { libc::CMSG_SPACE(12) } as _;
            unsafe {
                let c = libc::CMSG_FIRSTHDR(&msg);
                (*c).cmsg_level = libc::SOL_SOCKET;
                (*c).cmsg_type = libc::SCM_RIGHTS;
                (*c).cmsg_len = libc::CMSG_LEN(12) as _;
                std::ptr::copy_nonoverlapping(
                    [file.as_raw_fd(); 3].as_ptr().cast::<u8>(),
                    libc::CMSG_DATA(c),
                    12,
                );
                assert_eq!(libc::sendmsg(a.as_raw_fd(), &msg, 0), 1);
            }
            assert!(receive(&b, 3).is_err());
            assert_eq!(count(), before);
        });
    }
    #[test]
    fn newline_frames_do_not_consume_rights_markers_or_pipelined_frames() {
        isolated(
            "newline_frames_do_not_consume_rights_markers_or_pipelined_frames",
            || {
                let (mut a, mut b) = UnixStream::pair().unwrap();
                let file = File::open("/dev/null").unwrap();
                let before = count();
                for _ in 0..8 {
                    a.write_all(b"42\n43\n").unwrap();
                    send(&a, &[file.as_raw_fd(); 3]).unwrap();
                    pvisor::host_transport::write_host_frame_sync(&mut a, &44).unwrap();
                    assert_eq!(
                        super::super::host_service::read_frame::<u32>(&mut b).unwrap(),
                        42
                    );
                    assert_eq!(
                        super::super::host_service::read_frame::<u32>(&mut b).unwrap(),
                        43
                    );
                    let rights = receive(&b, 3).unwrap();
                    assert_eq!(rights.len(), 3);
                    assert_eq!(
                        super::super::host_service::read_frame::<u32>(&mut b).unwrap(),
                        44
                    );
                    drop(rights);
                    assert_eq!(count(), before);
                }
            },
        );
    }

    #[test]
    fn json_rejection_drops_all_rights_and_cloexec_is_atomic() {
        isolated(
            "json_rejection_drops_all_rights_and_cloexec_is_atomic",
            || {
                let (mut a, mut b) = UnixStream::pair().unwrap();
                let file = File::open("/dev/null").unwrap();
                let before = count();
                send(&a, &[file.as_raw_fd(); 3]).unwrap();
                a.write_all(b"bad\n").unwrap();
                let rejected = (|| -> anyhow::Result<()> {
                    let fds = receive(&b, 3)?;
                    assert!(
                        fds.iter()
                            .all(|fd| unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) }
                                & libc::FD_CLOEXEC
                                != 0)
                    );
                    let _: serde_json::Value = super::super::host_service::read_frame(&mut b)?;
                    Ok(())
                })();
                assert!(rejected.is_err());
                assert_eq!(count(), before);
            },
        );
    }
}
