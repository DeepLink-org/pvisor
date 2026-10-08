//! Exercise the actual vendored fusermount descriptor handoff without mounts.
#![cfg(target_os = "linux")]
#[path = "../../../vendor/fuser/src/background_shutdown.rs"]
mod background_shutdown;
#[path = "../../../vendor/fuser/src/mnt/fusermount_channel.rs"]
mod fusermount_channel;

use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::process::Command;

#[test]
fn interruptible_session_joins_and_destroys_without_peer_exit_ordinary_session_still_waits() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct ObservedFilesystem(Arc<AtomicUsize>);
    impl fuser::Filesystem for ObservedFilesystem {
        fn destroy(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    for interruptible in [true, false] {
        let destroyed = Arc::new(AtomicUsize::new(0));
        let (channel, peer) = UnixStream::pair().unwrap();
        let session = fuser::Session::from_fd(
            ObservedFilesystem(destroyed.clone()),
            channel.into(),
            fuser::SessionACL::Owner,
        );
        let background = if interruptible {
            fuser::BackgroundSession::new_interruptible(session).unwrap()
        } else {
            fuser::BackgroundSession::new(session).unwrap()
        };
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || result_tx.send(background.unmount()).unwrap());
        if !interruptible {
            assert!(
                result_rx
                    .recv_timeout(std::time::Duration::from_millis(30))
                    .is_err()
            );
            assert_eq!(destroyed.load(Ordering::SeqCst), 0);
            drop(peer);
        }
        result_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(destroyed.load(Ordering::SeqCst), 1);
        waiter.join().unwrap();
    }
}

#[test]
fn last_owner_wakes_an_idle_channel_without_waiting_for_unrelated_mount_references() {
    let (channel, _peer) = UnixStream::pair().unwrap();
    let (signal, stop) = background_shutdown::StopSignal::new().unwrap();
    assert_ne!(
        unsafe { libc::fcntl(stop.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
        0
    );
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        result_tx
            .send(background_shutdown::wait_for_request(
                channel.as_raw_fd(),
                stop.as_raw_fd(),
            ))
            .unwrap();
    });
    assert!(
        result_rx
            .recv_timeout(std::time::Duration::from_millis(30))
            .is_err()
    );
    drop(signal);
    assert!(
        !result_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap()
    );
    waiter.join().unwrap();
}

#[test]
fn shutdown_wins_over_readable_requests_and_unsignalled_channel_receives_normally() {
    let (channel, peer) = UnixStream::pair().unwrap();
    let (signal, stop) = background_shutdown::StopSignal::new().unwrap();
    assert_eq!(
        unsafe { libc::write(peer.as_raw_fd(), b"x".as_ptr().cast(), 1) },
        1
    );
    assert!(background_shutdown::wait_for_request(channel.as_raw_fd(), stop.as_raw_fd()).unwrap());
    drop(signal);
    assert!(!background_shutdown::wait_for_request(channel.as_raw_fd(), stop.as_raw_fd()).unwrap());
}

#[test]
fn descriptor_probe() {
    if let Ok(fd) = std::env::var("PVISOR_TEST_FUSE_HELPER_FD") {
        let fd: i32 = fd.parse().unwrap();
        assert_eq!(unsafe { libc::write(fd, b"x".as_ptr().cast(), 1) }, 1);
    }
    if let Ok(path) = std::env::var("PVISOR_TEST_FUSE_RECEIVED_PATH") {
        for fd in std::fs::read_dir("/proc/self/fd").unwrap() {
            if let Ok(target) = std::fs::read_link(fd.unwrap().path()) {
                assert_ne!(target.to_string_lossy(), path);
            }
        }
    }
}

#[test]
fn only_helper_child_inherits_its_socket_and_parent_stays_close_on_exec() {
    let (socket, mut peer) = UnixStream::pair().unwrap();
    let fd = socket.as_raw_fd();
    let mut helper = Command::new(std::env::current_exe().unwrap());
    helper
        .args(["--exact", "descriptor_probe"])
        .env("PVISOR_TEST_FUSE_HELPER_FD", fd.to_string());
    fusermount_channel::inherit_helper_socket(&mut helper, &socket);
    assert_ne!(
        unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
        0,
        "configuring a helper must not expose its socket to another concurrent exec"
    );
    let status = helper.status().unwrap();
    assert!(status.success());
    let mut message = [0];
    peer.read_exact(&mut message).unwrap();
    assert_eq!(message, *b"x");
    assert_ne!(
        unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
        0
    );
}

#[test]
fn received_fuse_channel_is_close_on_exec_before_any_other_helper_work() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("channel");
    let source = std::fs::File::create(&path).unwrap();
    let (sender, receiver) = UnixStream::pair().unwrap();
    let mut payload = [0u8];
    let mut iov = libc::iovec {
        iov_base: payload.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control =
        vec![0u8; unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) } as usize];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len() as _;
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as u32) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<i32>(), source.as_raw_fd());
        assert_eq!(libc::sendmsg(sender.as_raw_fd(), &message, 0), 1);
    }
    let channel = fusermount_channel::receive_fusermount_message(&receiver).unwrap();
    assert_ne!(
        unsafe { libc::fcntl(channel.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
        0
    );
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "descriptor_probe"])
        .env("PVISOR_TEST_FUSE_RECEIVED_PATH", &path)
        .status()
        .unwrap();
    assert!(status.success());
}
