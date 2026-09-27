use super::{fuse2_sys::*, with_fuse_args, MountOption};
#[cfg(not(all(target_os = "macos", feature = "macfuse-5")))]
use log::warn;
#[cfg(not(all(target_os = "macos", feature = "macfuse-5")))]
use std::{fs::File, os::fd::FromRawFd};
#[cfg(all(target_os = "macos", feature = "macfuse-5"))]
type Device = MacChannel;
#[cfg(not(all(target_os = "macos", feature = "macfuse-5")))]
type Device = File;
use std::{ffi::CString, io, os::unix::prelude::OsStrExt, path::Path, sync::Arc};

/// Ensures that an os error is never 0/Success
fn ensure_last_os_error() -> io::Error {
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(0) => io::Error::new(io::ErrorKind::Other, "Unspecified Error"),
        _ => err,
    }
}

#[derive(Debug)]
pub struct Mount {
    #[cfg(not(all(target_os = "macos", feature = "macfuse-5")))]
    mountpoint: CString,
    #[cfg(all(target_os = "macos", feature = "macfuse-5"))]
    pub(super) channel: Arc<MacChannel>,
}
impl Mount {
    #[cfg(all(target_os = "macos", feature = "macfuse-5"))]
    pub(crate) fn unmount_gracefully(&self) -> io::Result<()> {
        unsafe {
            (self.channel.api.chan_unmount)(self.channel.channel as *mut fuse_chan);
            // Disk Arbitration waits for clients such as Spotlight before
            // unmounting (observed >10s). Keep serving flush/write requests
            // throughout that grace period instead of cutting them off at 5s.
            for _ in 0..1500 {
                if (self.channel.api.chan_not_mounted)(self.channel.channel as *mut fuse_chan) {
                    self.channel
                        .stopped
                        .store(true, std::sync::atomic::Ordering::Release);
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        self.channel
            .stopped
            .store(true, std::sync::atomic::Ordering::Release);
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "macFUSE graceful unmount did not complete; cached writes may not have reached the filesystem",
        ))
    }
    pub fn new(mountpoint: &Path, options: &[MountOption]) -> io::Result<(Arc<Device>, Mount)> {
        let mountpoint = CString::new(mountpoint.as_os_str().as_bytes()).unwrap();
        #[cfg(all(target_os = "macos", feature = "macfuse-5"))]
        let macfuse = MacFuseApi::load()?;
        with_fuse_args(options, |args| {
            #[cfg(all(target_os = "macos", feature = "macfuse-5"))]
            return unsafe {
                let channel = (macfuse.mount)(mountpoint.as_ptr(), args);
                if channel.is_null() {
                    return Err(ensure_last_os_error());
                }
                // libfuse's receive callback requires an attached session even
                // though fuser, rather than libfuse, dispatches the requests.
                unsafe extern "C" fn exited(data: *mut libc::c_void) -> libc::c_int {
                    // The boxed flag lives until session_destroy returns.
                    unsafe { &*data.cast::<std::sync::atomic::AtomicBool>() }
                        .load(std::sync::atomic::Ordering::Acquire)
                        as libc::c_int
                }
                let mut stopped = Box::new(std::sync::atomic::AtomicBool::new(false));
                let ops = SessionOps {
                    exited: Some(exited),
                    ..Default::default()
                };
                let session = (macfuse.session_new)(
                    &ops,
                    (&mut *stopped as *mut std::sync::atomic::AtomicBool).cast(),
                );
                if session.is_null() {
                    (macfuse.unmount)(mountpoint.as_ptr(), channel);
                    return Err(io::Error::from_raw_os_error(libc::ENOMEM));
                }
                (macfuse.session_add_chan)(session, channel);
                let channel = Arc::new(MacChannel {
                    channel: channel as usize,
                    session: session as usize,
                    stopped,
                    api: macfuse,
                });
                Ok((Arc::clone(&channel), Mount { channel }))
            };
            #[cfg(not(all(target_os = "macos", feature = "macfuse-5")))]
            let fd = unsafe { fuse_mount_compat25(mountpoint.as_ptr(), args) };
            #[cfg(not(all(target_os = "macos", feature = "macfuse-5")))]
            if fd < 0 {
                Err(ensure_last_os_error())
            } else {
                let file = unsafe { File::from_raw_fd(fd) };
                Ok((Arc::new(file), Mount { mountpoint }))
            }
        })
    }
}
impl Drop for Mount {
    fn drop(&mut self) {
        #[cfg(all(target_os = "macos", feature = "macfuse-5"))]
        unsafe {
            // Do not use fuse_unmount: it can destroy a not-yet-mounted
            // channel while a receiver still holds it. The session owns it.
            // Keep servicing teardown requests until the asynchronous native
            // unmount finishes. Interrupting first leaves a dead mounted volume.
            if !self
                .channel
                .stopped
                .load(std::sync::atomic::Ordering::Acquire)
            {
                let _ = self.unmount_gracefully();
            }
            self.channel
                .stopped
                .store(true, std::sync::atomic::Ordering::Release);
            (self.channel.api.chan_interrupt)(self.channel.channel as *mut fuse_chan);
        }
        #[cfg(not(all(target_os = "macos", feature = "macfuse-5")))]
        {
            use std::io::ErrorKind::PermissionDenied;

            // fuse_unmount_compat22 unfortunately doesn't return a status. Additionally,
            // it attempts to call realpath, which in turn calls into the filesystem. So
            // if the filesystem returns an error, the unmount does not take place, with
            // no indication of the error available to the caller. So we call unmount
            // directly, which is what osxfuse does anyway, since we already converted
            // to the real path when we first mounted.
            if let Err(err) = super::libc_umount(&self.mountpoint) {
                // Linux always returns EPERM for non-root users.  We have to let the
                // library go through the setuid-root "fusermount -u" to unmount.
                if err.kind() == PermissionDenied {
                    #[cfg(not(any(
                        target_os = "macos",
                        target_os = "freebsd",
                        target_os = "dragonfly",
                        target_os = "openbsd",
                        target_os = "netbsd"
                    )))]
                    unsafe {
                        fuse_unmount_compat22(self.mountpoint.as_ptr());
                        return;
                    }
                }
                warn!("umount failed with {:?}", err);
            }
        }
    }
}
