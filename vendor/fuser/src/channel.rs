use std::{fs::File, io, os::unix::prelude::AsRawFd, sync::Arc};

use libc::{c_int, c_void, size_t};

use crate::reply::ReplySender;

#[cfg(all(target_os = "macos", feature = "macfuse-5"))]
use crate::mnt::fuse2_sys::{fuse_chan, MacChannel};
#[cfg(not(all(target_os = "macos", feature = "macfuse-5")))]
use std::os::fd::{AsFd, BorrowedFd};

/// A raw communication channel to the FUSE kernel driver
#[derive(Clone, Debug)]
pub enum Channel {
    File(Arc<File>),
    #[cfg(all(target_os = "macos", feature = "macfuse-5"))]
    Mac(Arc<MacChannel>),
}

impl From<Arc<File>> for Channel {
    fn from(file: Arc<File>) -> Self {
        Self::File(file)
    }
}
#[cfg(all(target_os = "macos", feature = "macfuse-5"))]
impl From<Arc<MacChannel>> for Channel {
    fn from(channel: Arc<MacChannel>) -> Self {
        Self::Mac(channel)
    }
}

#[cfg(not(all(target_os = "macos", feature = "macfuse-5")))]
impl AsFd for Channel {
    fn as_fd(&self) -> BorrowedFd<'_> {
        match self {
            Self::File(file) => file.as_fd(),
        }
    }
}

impl Channel {
    /// Create a new communication channel to the kernel driver by mounting the
    /// given path. The kernel driver will delegate filesystem operations of
    /// the given path to the channel.
    pub(crate) fn new(device: impl Into<Self>) -> Self {
        device.into()
    }

    /// Receives data up to the capacity of the given buffer (can block).
    pub fn receive(&self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(all(target_os = "macos", feature = "macfuse-5"))]
            Self::Mac(ch) => {
                let mut ptr = ch.channel as *mut fuse_chan;
                let rc =
                    unsafe { (ch.api.recv)(&mut ptr, buffer.as_mut_ptr().cast(), buffer.len()) };
                if rc < 0 {
                    Err(io::Error::from_raw_os_error(-rc))
                } else {
                    Ok(rc as usize)
                }
            }
            Self::File(file) => {
                let rc = unsafe {
                    libc::read(
                        file.as_raw_fd(),
                        buffer.as_ptr() as *mut c_void,
                        buffer.len() as size_t,
                    )
                };
                if rc < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(rc as usize)
                }
            }
        }
    }

    /// Returns a sender object for this channel. The sender object can be
    /// used to send to the channel. Multiple sender objects can be used
    /// and they can safely be sent to other threads.
    pub fn sender(&self) -> ChannelSender {
        // Since write/writev syscalls are threadsafe, we can simply create
        // a sender by using the same file and use it in other threads.
        ChannelSender(self.clone())
    }
}

#[derive(Clone, Debug)]
pub struct ChannelSender(Channel);

impl ReplySender for ChannelSender {
    fn send(&self, bufs: &[io::IoSlice<'_>]) -> io::Result<()> {
        match &self.0 {
            #[cfg(all(target_os = "macos", feature = "macfuse-5"))]
            Channel::Mac(ch) => {
                let rc = unsafe {
                    (ch.api.send)(
                        ch.channel as *mut fuse_chan,
                        bufs.as_ptr().cast(),
                        bufs.len(),
                    )
                };
                if rc < 0 {
                    Err(io::Error::from_raw_os_error(-rc))
                } else {
                    Ok(())
                }
            }
            Channel::File(file) => {
                let rc = unsafe {
                    libc::writev(
                        file.as_raw_fd(),
                        bufs.as_ptr() as *const libc::iovec,
                        bufs.len() as c_int,
                    )
                };
                if rc < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    debug_assert_eq!(bufs.iter().map(|b| b.len()).sum::<usize>(), rc as usize);
                    Ok(())
                }
            }
        }
    }
}
