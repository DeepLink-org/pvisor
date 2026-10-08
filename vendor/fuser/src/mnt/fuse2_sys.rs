//! Native FFI bindings to libfuse2.
//!
//! This is a small set of bindings that are required to mount/unmount FUSE filesystems and
//! open/close a fd to the FUSE kernel driver.

#![warn(missing_debug_implementations)]
#![allow(missing_docs)]

use libc::{c_char, c_int};
#[cfg(all(target_os = "macos", feature = "macfuse-5"))]
use std::{
    io,
    sync::{Arc, OnceLock},
};

#[repr(C)]
#[derive(Debug)]
pub struct fuse_args {
    pub argc: c_int,
    pub argv: *const *const c_char,
    pub allocated: c_int,
}

#[cfg(all(target_os = "macos", feature = "macfuse-5"))]
#[repr(C)]
#[derive(Debug)]
pub struct fuse_chan {
    _private: [u8; 0],
}

#[cfg(all(target_os = "macos", feature = "macfuse-5"))]
pub struct MacFuseApi {
    _library: libloading::Library,
    pub mount: unsafe extern "C" fn(*const c_char, *const fuse_args) -> *mut fuse_chan,
    pub recv: unsafe extern "C" fn(*mut *mut fuse_chan, *mut c_char, usize) -> c_int,
    pub send: unsafe extern "C" fn(*mut fuse_chan, *const libc::iovec, usize) -> c_int,
    pub session_new:
        unsafe extern "C" fn(*const SessionOps, *mut libc::c_void) -> *mut libc::c_void,
    pub session_add_chan: unsafe extern "C" fn(*mut libc::c_void, *mut fuse_chan),
    pub session_destroy: unsafe extern "C" fn(*mut libc::c_void),
    pub chan_unmount: unsafe extern "C" fn(*mut fuse_chan),
    pub chan_interrupt: unsafe extern "C" fn(*mut fuse_chan),
    pub chan_not_mounted: unsafe extern "C" fn(*mut fuse_chan) -> bool,
    pub unmount: unsafe extern "C" fn(*const c_char, *mut fuse_chan),
}

#[cfg(all(target_os = "macos", feature = "macfuse-5"))]
impl std::fmt::Debug for MacFuseApi {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("MacFuseApi").finish_non_exhaustive()
    }
}

#[cfg(all(target_os = "macos", feature = "macfuse-5"))]
impl MacFuseApi {
    pub fn load() -> io::Result<Arc<Self>> {
        static API: OnceLock<Result<Arc<MacFuseApi>, String>> = OnceLock::new();
        match API.get_or_init(|| Self::load_uncached().map_err(|error| error.to_string())) {
            Ok(api) => Ok(Arc::clone(api)),
            Err(error) => Err(io::Error::new(io::ErrorKind::NotFound, error.clone())),
        }
    }

    fn load_uncached() -> io::Result<Arc<Self>> {
        const CANDIDATES: [&str; 4] = [
            "/usr/local/lib/libfuse.2.dylib",
            "/usr/local/lib/libfuse.dylib",
            "libfuse.2.dylib",
            "libfuse.dylib",
        ];
        let mut failures = Vec::new();
        for candidate in CANDIDATES {
            let library = match unsafe { libloading::Library::new(candidate) } {
                Ok(library) => library,
                Err(error) => {
                    failures.push(format!("{candidate}: {error}"));
                    continue;
                }
            };
            return Ok(Arc::new(unsafe {
                Self {
                    mount: *library.get(b"fuse_mount\0").map_err(io::Error::other)?,
                    unmount: *library.get(b"fuse_unmount\0").map_err(io::Error::other)?,
                    recv: *library.get(b"fuse_chan_recv\0").map_err(io::Error::other)?,
                    send: *library.get(b"fuse_chan_send\0").map_err(io::Error::other)?,
                    session_new: *library
                        .get(b"fuse_session_new\0")
                        .map_err(io::Error::other)?,
                    session_add_chan: *library
                        .get(b"fuse_session_add_chan\0")
                        .map_err(io::Error::other)?,
                    session_destroy: *library
                        .get(b"fuse_session_destroy\0")
                        .map_err(io::Error::other)?,
                    chan_unmount: *library
                        .get(b"fuse_darwin_chan_unmount\0")
                        .map_err(io::Error::other)?,
                    chan_interrupt: *library
                        .get(b"fuse_darwin_chan_interrupt\0")
                        .map_err(io::Error::other)?,
                    chan_not_mounted: *library
                        .get(b"fuse_darwin_chan_not_mounted\0")
                        .map_err(io::Error::other)?,
                    _library: library,
                }
            }));
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "macFUSE runtime library not found; install macFUSE before mounting ({})",
                failures.join("; ")
            ),
        ))
    }
}

#[cfg(fuser_mount_impl = "libfuse2")]
extern "C" {
    // *_compat25 functions were introduced in FUSE 2.6 when function signatures changed.
    // Therefore, the minimum version requirement for *_compat25 functions is libfuse-2.6.0.

    #[cfg(not(all(target_os = "macos", feature = "macfuse-5")))]
    pub fn fuse_mount_compat25(mountpoint: *const c_char, args: *const fuse_args) -> c_int;
    #[cfg(not(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "openbsd",
        target_os = "netbsd"
    )))]
    pub fn fuse_unmount_compat22(mountpoint: *const c_char);
}

#[cfg(all(target_os = "macos", feature = "macfuse-5"))]
#[repr(C)]
#[derive(Debug, Default)]
pub struct SessionOps {
    pub process:
        Option<unsafe extern "C" fn(*mut libc::c_void, *const c_char, usize, *mut fuse_chan)>,
    pub exit: Option<unsafe extern "C" fn(*mut libc::c_void, c_int)>,
    pub exited: Option<unsafe extern "C" fn(*mut libc::c_void) -> c_int>,
    pub destroy: Option<unsafe extern "C" fn(*mut libc::c_void)>,
}

/// Owns the libfuse session and its channel until all Rust receivers/senders stop.
#[cfg(all(target_os = "macos", feature = "macfuse-5"))]
#[derive(Debug)]
pub struct MacChannel {
    pub channel: usize,
    pub session: usize,
    pub stopped: Box<std::sync::atomic::AtomicBool>,
    pub api: Arc<MacFuseApi>,
}

#[cfg(all(target_os = "macos", feature = "macfuse-5"))]
impl Drop for MacChannel {
    fn drop(&mut self) {
        unsafe { (self.api.session_destroy)(self.session as *mut libc::c_void) };
    }
}

#[cfg(all(test, target_os = "macos", feature = "macfuse-5"))]
mod tests {
    use super::*;
    use crate::{channel::Channel, reply::ReplySender};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[test]
    fn fdless_channel_preserves_errno_and_outlives_reply_senders() {
        static UNMOUNTS: AtomicUsize = AtomicUsize::new(0);
        static DESTROYED: AtomicBool = AtomicBool::new(false);
        unsafe extern "C" fn recv(_: *mut *mut fuse_chan, _: *mut c_char, _: usize) -> c_int {
            -libc::EACCES
        }
        unsafe extern "C" fn send(_: *mut fuse_chan, _: *const libc::iovec, _: usize) -> c_int {
            -libc::ENODEV
        }
        unsafe extern "C" fn destroy(_: *mut libc::c_void) {
            DESTROYED.store(true, Ordering::SeqCst);
        }
        unsafe extern "C" fn mount(_: *const c_char, _: *const fuse_args) -> *mut fuse_chan {
            unreachable!()
        }
        unsafe extern "C" fn unmount(_: *const c_char, _: *mut fuse_chan) {
            unreachable!()
        }
        unsafe extern "C" fn new(_: *const SessionOps, _: *mut libc::c_void) -> *mut libc::c_void {
            unreachable!()
        }
        unsafe extern "C" fn add(_: *mut libc::c_void, _: *mut fuse_chan) {
            unreachable!()
        }
        unsafe extern "C" fn detach(_: *mut fuse_chan) {}
        unsafe extern "C" fn request_unmount(_: *mut fuse_chan) {
            UNMOUNTS.fetch_add(1, Ordering::SeqCst);
        }
        unsafe extern "C" fn not_mounted(channel: *mut fuse_chan) -> bool {
            channel.is_null()
        }
        let api = Arc::new(MacFuseApi {
            _library: libloading::os::unix::Library::this().into(),
            mount,
            unmount,
            recv,
            send,
            session_new: new,
            session_add_chan: add,
            session_destroy: destroy,
            chan_unmount: request_unmount,
            chan_interrupt: detach,
            chan_not_mounted: not_mounted,
        });
        let channel = Channel::new(Arc::new(MacChannel {
            channel: 0,
            session: 0,
            stopped: Box::default(),
            api: Arc::clone(&api),
        }));
        let sender = channel.sender();
        assert_eq!(
            channel.receive(&mut [0; 8]).unwrap_err().raw_os_error(),
            Some(libc::EACCES)
        );
        drop(channel);
        assert!(!DESTROYED.load(Ordering::SeqCst));
        assert_eq!(
            sender.send(&[]).unwrap_err().raw_os_error(),
            Some(libc::ENODEV)
        );
        drop(sender);
        assert!(DESTROYED.load(Ordering::SeqCst));
        let mounted = crate::mnt::Mount {
            channel: Arc::new(MacChannel {
                channel: 0,
                session: 0,
                stopped: Box::default(),
                api: Arc::clone(&api),
            }),
        };
        mounted.unmount_gracefully().unwrap();
        drop(mounted);
        assert_eq!(UNMOUNTS.load(Ordering::SeqCst), 1);
        let stalled = crate::mnt::Mount {
            channel: Arc::new(MacChannel {
                channel: 1,
                session: 0,
                stopped: Box::default(),
                api,
            }),
        };
        assert_eq!(
            stalled.unmount_gracefully().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(stalled.channel.stopped.load(Ordering::Acquire));
        drop(stalled);
    }
}
