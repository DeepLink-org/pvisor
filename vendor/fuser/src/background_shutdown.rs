//! Explicit last-owner shutdown for mounts copied into unrelated namespaces.
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;

pub(crate) struct StopSignal(Arc<OwnedFd>);

impl StopSignal {
    pub(crate) fn new() -> io::Result<(Self, Arc<OwnedFd>)> {
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = Arc::new(unsafe { OwnedFd::from_raw_fd(fd) });
        Ok((Self(fd.clone()), fd))
    }
}

impl Drop for StopSignal {
    fn drop(&mut self) {
        let value = 1u64.to_ne_bytes();
        loop {
            let written =
                unsafe { libc::write(self.0.as_raw_fd(), value.as_ptr().cast(), value.len()) };
            if written == value.len() as isize {
                break;
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            // An already-signalled nonblocking eventfd is also a wakeup.
            debug_assert_eq!(error.raw_os_error(), Some(libc::EAGAIN));
            break;
        }
    }
}

/// There is exactly one channel reader. A stop event wins over readable data
/// so an inherited, inaccessible mount cannot retain the serving thread.
pub(crate) fn wait_for_request(channel: RawFd, stop: RawFd) -> io::Result<bool> {
    let mut descriptors = [
        libc::pollfd {
            fd: channel,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: stop,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        let result = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, -1) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
        if descriptors
            .iter()
            .any(|fd| fd.revents & libc::POLLNVAL != 0)
        {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        if descriptors[1].revents != 0 {
            return Ok(false);
        }
        if descriptors[0].revents != 0 {
            return Ok(true);
        }
    }
}
