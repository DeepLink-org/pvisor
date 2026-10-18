// Copyright 2019 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(target_os = "macos")]
use crate::utils::worker_message::WorkerMessage;
#[cfg(target_os = "macos")]
use crossbeam_channel::Sender;

use std::convert::TryInto;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;

use vm_memory::ByteValued;

use super::super::linux_errno::linux_error;
use super::bindings;
use super::descriptor_utils::{Reader, Writer};
use super::filesystem::{
    Context, DirEntry, Entry, Extensions, FileSystem, GetxattrReply, ListxattrReply, SecContext,
    ZeroCopyReader, ZeroCopyWriter,
};
use super::fs_utils::einval;
use super::fuse::*;
use super::{FsError as Error, Result};
use crate::devices::virtio::VirtioShmRegion;

const _: () = assert!(size_of::<OpenOut>() == 16);
const MAX_BUFFER_SIZE: u32 = 1 << 20;
const BUFFER_HEADER_SIZE: u32 = 0x1000;
/// FUSE counts `InitOut.max_pages` in guest pages. The firmware guest uses
/// 4 KiB pages and accepts at most 256 pages in one request. Deriving the
/// count from the host page size advertises 64 pages on a 16 KiB host, which
/// that guest treats as a 256 KiB read.
const GUEST_PAGE_SIZE: u32 = 4096;
const FUSE_MAX_MAX_PAGES: u32 = 256;
const NEGOTIATED_MAX_PAGES: u16 = (MAX_BUFFER_SIZE / GUEST_PAGE_SIZE) as u16;
const _: () = assert!(NEGOTIATED_MAX_PAGES as u32 == FUSE_MAX_MAX_PAGES);
const _: () = assert!(NEGOTIATED_MAX_PAGES as u32 * GUEST_PAGE_SIZE == MAX_BUFFER_SIZE);
const DIRENT_PADDING: [u8; 8] = [0; 8];
pub(super) const PROFILE_PHASE_IOCTL: u32 = 0x4008_76f1;

struct ZCReader<'a>(Reader<'a>);

impl ZeroCopyReader for ZCReader<'_> {
    fn read_to(&mut self, f: &File, count: usize, off: u64) -> io::Result<usize> {
        self.0.read_to_at(f, count, off)
    }
}

impl io::Read for ZCReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

struct ZCWriter<'a>(Writer<'a>);

impl ZeroCopyWriter for ZCWriter<'_> {
    fn write_from(&mut self, f: &File, count: usize, off: u64) -> io::Result<usize> {
        self.0.write_from_at(f, count, off)
    }
}

impl io::Write for ZCWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

pub struct Server<F: FileSystem + Sync> {
    profile: pvisor_overlay_core::profile::Profile,
    dispatch_profile: pvisor_overlay_core::profile::Profile,
    profile_scope: Option<String>,
    fs: F,
    guest_cache: bool,
    options: AtomicU64,
}

/// Accepted READDIRPLUS entries acquire references before they are serialized.
/// Until the complete successful reply header is written, the RPC owns those
/// references: an error discards the whole payload and the guest cannot FORGET
/// any of them. The filesystem cancels only the current callback's zero/error.
struct DirectoryReplyLookups<'a, F: FileSystem> {
    fs: &'a F,
    ctx: Context,
    accepted: Vec<u64>,
}

impl<F: FileSystem> Drop for DirectoryReplyLookups<'_, F> {
    fn drop(&mut self) {
        for inode in self.accepted.drain(..) {
            self.fs.forget(self.ctx, inode.into(), 1);
        }
    }
}

impl<F: FileSystem + Sync> Server<F> {
    pub fn new(fs: F, guest_cache: bool) -> Server<F> {
        Server {
            profile: pvisor_overlay_core::profile::Profile::from_env("virtio-fs-protocol"),
            dispatch_profile: pvisor_overlay_core::profile::Profile::from_env("virtio-fs-dispatch"),
            profile_scope: None,
            fs,
            guest_cache,
            options: AtomicU64::new(FsOptions::empty().bits()),
        }
    }

    pub(super) fn configure_profile_scope(&mut self, scope: &str) {
        if self.profile.report().is_some() {
            self.profile_scope = Some(
                if scope == "/dev/root" {
                    "rootfs"
                } else {
                    scope
                }
                .into(),
            );
        }
    }

    pub(super) fn dispatch_profile(&self) -> pvisor_overlay_core::profile::Profile {
        self.dispatch_profile.clone()
    }

    fn phase_profiles(&self, token: u64) -> Option<serde_json::Value> {
        self.profile.report().map(|protocol| {
            let profiles = std::iter::once(protocol)
                .chain(self.dispatch_profile.report())
                .chain(self.fs.diagnostic_profiles())
                .collect::<Vec<_>>();
            serde_json::json!({
                "schema": 1,
                "scope": self.profile_scope,
                "token": token,
                "profiles": profiles,
            })
        })
    }

    pub fn capture_state(&self, next_inode: u64) -> io::Result<super::snapshot::ServerSnapshot> {
        self.profile.emit_checkpoint();
        let options = self.options.load(Ordering::Relaxed);
        if options & (FsOptions::PVISOR_LOCAL_FLUSH | FsOptions::PVISOR_DATA_OBJECT).bits() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "snapshots of negotiated experimental flush/data-object sessions are unsupported",
            ));
        }
        let fs = self.fs.capture_state()?;
        fs.validate_writeback(
            FsOptions::from_bits_truncate(options).contains(FsOptions::WRITEBACK_CACHE),
        )?;
        Ok(super::snapshot::ServerSnapshot {
            options,
            next_inode,
            fs,
        })
    }
    pub fn restore_state(&self, state: &super::snapshot::ServerSnapshot) -> io::Result<()> {
        if FsOptions::from_bits(state.options).is_none() {
            return Err(super::snapshot::invalid("invalid FUSE options"));
        }
        if state.options & (FsOptions::PVISOR_LOCAL_FLUSH | FsOptions::PVISOR_DATA_OBJECT).bits()
            != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "restoring a negotiated experimental flush/data-object session is unsupported",
            ));
        }
        state.fs.validate_writeback(
            FsOptions::from_bits_truncate(state.options).contains(FsOptions::WRITEBACK_CACHE),
        )?;
        self.fs.restore_state(&state.fs)?;
        self.options.store(state.options, Ordering::Relaxed);
        Ok(())
    }

    pub fn handle_message(
        &self,
        message: (InHeader, Reader),
        w: Writer,
        allow_idmap: bool,
        shm_region: &Option<VirtioShmRegion>,
        exit_code: &Arc<AtomicI32>,
        #[cfg(target_os = "macos")] map_sender: &Option<Sender<WorkerMessage>>,
    ) -> Result<usize> {
        // The execution layer decodes the header once before choosing the
        // session guard. Never reread guest-controlled opcode after admission.
        let (in_header, r) = message;

        let label = match in_header.opcode {
            x if x == Opcode::Lookup as u32 => "LOOKUP",
            x if x == Opcode::Getattr as u32 => "GETATTR",
            x if x == Opcode::Open as u32 => "OPEN",
            x if x == Opcode::Read as u32 => "READ",
            x if x == Opcode::Write as u32 => "WRITE",
            x if x == Opcode::Opendir as u32 => "OPENDIR",
            x if x == Opcode::Readdir as u32 => "READDIR",
            x if x == Opcode::Readdirplus as u32 => "READDIRPLUS",
            x if x == Opcode::Create as u32 => "CREATE",
            x if x == Opcode::Fsync as u32 => "FSYNC",
            x if x == Opcode::Syncfs as u32 => "SYNCFS",
            x if x == Opcode::Release as u32 => "RELEASE",
            x if x == Opcode::Setattr as u32 => "SETATTR",
            x if x == Opcode::Readlink as u32 => "READLINK",
            x if x == Opcode::Symlink as u32 => "SYMLINK",
            x if x == Opcode::Mkdir as u32 => "MKDIR",
            x if x == Opcode::Flush as u32 => "FLUSH",
            x if x == Opcode::Access as u32 => "ACCESS",
            x if x == Opcode::Forget as u32 => "FORGET",
            x if x == Opcode::Fsyncdir as u32 => "FSYNCDIR",
            x if x == Opcode::Setxattr as u32 => "SETXATTR",
            x if x == Opcode::Getxattr as u32 => "GETXATTR",
            x if x == Opcode::Listxattr as u32 => "LISTXATTR",
            x if x == Opcode::Removexattr as u32 => "REMOVEXATTR",
            x if x == Opcode::Unlink as u32 => "UNLINK",
            x if x == Opcode::Rmdir as u32 => "RMDIR",
            x if x == Opcode::Rename as u32 => "RENAME",
            x if x == Opcode::Rename2 as u32 => "RENAME2",
            x if x == Opcode::Statfs as u32 => "STATFS",
            x if x == Opcode::BatchForget as u32 => "BATCHFORGET",
            x if x == Opcode::Releasedir as u32 => "RELEASEDIR",
            x if x == Opcode::ReadIndex as u32 => "READINDEX",
            x if x == Opcode::Ioctl as u32 => "IOCTL",
            _ => "OTHER",
        };
        let _span = self.profile.span(label);
        self.profile.add("request_bytes", u64::from(in_header.len));

        if in_header.len > (MAX_BUFFER_SIZE + BUFFER_HEADER_SIZE) {
            return reply_error(
                linux_error(io::Error::from_raw_os_error(libc::ENOMEM)),
                in_header.unique,
                w,
            );
        }
        debug!("opcode: {}", in_header.opcode);
        match in_header.opcode {
            x if x == Opcode::Lookup as u32 => self.lookup(in_header, r, w),
            x if x == Opcode::Forget as u32 => self.forget(in_header, r), // No reply.
            x if x == Opcode::Getattr as u32 => self.getattr(in_header, r, w),
            x if x == Opcode::Setattr as u32 => self.setattr(in_header, r, w),
            x if x == Opcode::Readlink as u32 => self.readlink(in_header, w),
            x if x == Opcode::Symlink as u32 => self.symlink(in_header, r, w),
            x if x == Opcode::Mknod as u32 => self.mknod(in_header, r, w),
            x if x == Opcode::Mkdir as u32 => self.mkdir(in_header, r, w),
            x if x == Opcode::Unlink as u32 => self.unlink(in_header, r, w),
            x if x == Opcode::Rmdir as u32 => self.rmdir(in_header, r, w),
            x if x == Opcode::Rename as u32 => self.rename(in_header, r, w),
            x if x == Opcode::Link as u32 => self.link(in_header, r, w),
            x if x == Opcode::Open as u32 => self.open(in_header, r, w),
            x if x == Opcode::Read as u32 => self.read(in_header, r, w),
            x if x == Opcode::Write as u32 => self.write(in_header, r, w),
            x if x == Opcode::Statfs as u32 => self.statfs(in_header, w),
            x if x == Opcode::Release as u32 => self.release(in_header, r, w),
            x if x == Opcode::Fsync as u32 => self.fsync(in_header, r, w),
            x if x == Opcode::Syncfs as u32 => self.syncfs(in_header, r, w),
            x if x == Opcode::Setxattr as u32 => self.setxattr(in_header, r, w),
            x if x == Opcode::Getxattr as u32 => self.getxattr(in_header, r, w),
            x if x == Opcode::Listxattr as u32 => self.listxattr(in_header, r, w),
            x if x == Opcode::Removexattr as u32 => self.removexattr(in_header, r, w),
            x if x == Opcode::Flush as u32 => self.flush(in_header, r, w),
            x if x == Opcode::Init as u32 => self.init(in_header, r, w, allow_idmap),
            x if x == Opcode::Opendir as u32 => self.opendir(in_header, r, w),
            x if x == Opcode::Readdir as u32 => self.readdir(in_header, r, w),
            x if x == Opcode::Releasedir as u32 => self.releasedir(in_header, r, w),
            x if x == Opcode::Fsyncdir as u32 => self.fsyncdir(in_header, r, w),
            x if x == Opcode::Getlk as u32 => self.getlk(in_header, r, w),
            x if x == Opcode::Setlk as u32 => self.setlk(in_header, r, w),
            x if x == Opcode::Setlkw as u32 => self.setlkw(in_header, r, w),
            x if x == Opcode::Access as u32 => self.access(in_header, r, w),
            x if x == Opcode::Create as u32 => self.create(in_header, r, w),
            x if x == Opcode::Interrupt as u32 => self.interrupt(in_header),
            x if x == Opcode::Bmap as u32 => self.bmap(in_header, r, w),
            x if x == Opcode::Destroy as u32 => self.destroy(),
            x if x == Opcode::Ioctl as u32 => self.ioctl(in_header, r, w, exit_code),
            x if x == Opcode::Poll as u32 => self.poll(in_header, r, w),
            x if x == Opcode::NotifyReply as u32 => self.notify_reply(in_header, r, w),
            x if x == Opcode::BatchForget as u32 => self.batch_forget(in_header, r, w),
            x if x == Opcode::Fallocate as u32 => self.fallocate(in_header, r, w),
            x if x == Opcode::Readdirplus as u32 => self.readdirplus(in_header, r, w),
            x if x == Opcode::ReadIndex as u32 => self.read_index(in_header, r, w),
            x if x == Opcode::Rename2 as u32 => self.rename2(in_header, r, w),
            x if x == Opcode::Lseek as u32 => self.lseek(in_header, r, w),
            x if x == Opcode::CopyFileRange as u32 => self.copyfilerange(in_header, r, w),
            x if (x == Opcode::SetupMapping as u32) && shm_region.is_some() => {
                let shm = shm_region.as_ref().unwrap();
                #[cfg(target_os = "linux")]
                let shm_base_addr = shm.host_addr;
                #[cfg(target_os = "macos")]
                let shm_base_addr = shm.guest_addr;
                self.setupmapping(
                    in_header,
                    r,
                    w,
                    shm_base_addr,
                    shm.size as u64,
                    #[cfg(target_os = "macos")]
                    map_sender,
                )
            }
            x if (x == Opcode::RemoveMapping as u32) && shm_region.is_some() => {
                let shm = shm_region.as_ref().unwrap();
                #[cfg(target_os = "linux")]
                let shm_base_addr = shm.host_addr;
                #[cfg(target_os = "macos")]
                let shm_base_addr = shm.guest_addr;
                self.removemapping(
                    in_header,
                    r,
                    w,
                    shm_base_addr,
                    shm.size as u64,
                    #[cfg(target_os = "macos")]
                    map_sender,
                )
            }
            _ => reply_error(
                linux_error(io::Error::from_raw_os_error(libc::ENOSYS)),
                in_header.unique,
                w,
            ),
        }
    }

    fn lookup(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let namelen = (in_header.len as usize)
            .checked_sub(size_of::<InHeader>())
            .ok_or(Error::InvalidHeaderLength)?;

        let mut buf = vec![0u8; namelen];

        r.read_exact(&mut buf).map_err(Error::DecodeMessage)?;

        let name = bytes_to_cstr(buf.as_ref())?;

        debug!(
            "lookup unique={} parent={} name={name:?}",
            in_header.unique, in_header.nodeid
        );

        match self
            .fs
            .lookup(Context::from(in_header), in_header.nodeid.into(), name)
        {
            Ok(entry) => {
                debug!("lookup unique={} inode={}", in_header.unique, entry.inode);
                let out = EntryOut::from(entry);

                reply_ok(Some(out), None, in_header.unique, w)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn forget(&self, in_header: InHeader, mut r: Reader) -> Result<usize> {
        let ForgetIn { nlookup } = r.read_obj().map_err(Error::DecodeMessage)?;

        self.fs
            .forget(Context::from(in_header), in_header.nodeid.into(), nlookup);

        // There is no reply for forget messages.
        Ok(0)
    }

    fn getattr(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let GetattrIn { flags, fh, .. } = r.read_obj().map_err(Error::DecodeMessage)?;

        let handle = if (flags & GETATTR_FH) != 0 {
            Some(fh.into())
        } else {
            None
        };

        match self
            .fs
            .getattr(Context::from(in_header), in_header.nodeid.into(), handle)
        {
            Ok((st, timeout)) => {
                let out = AttrOut {
                    attr_valid: timeout.as_secs(),
                    attr_valid_nsec: timeout.subsec_nanos(),
                    dummy: 0,
                    attr: Attr::with_flags(
                        st,
                        self.fs.attribute_flags(in_header.nodeid.into(), &st),
                    ),
                };
                reply_ok(Some(out), None, in_header.unique, w)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn setattr(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let setattr_in: SetattrIn = r.read_obj().map_err(Error::DecodeMessage)?;

        let handle = if setattr_in.valid & FATTR_FH != 0 {
            Some(setattr_in.fh.into())
        } else {
            None
        };

        let valid = SetattrValid::from_bits_truncate(setattr_in.valid);

        let st: bindings::stat64 = setattr_in.into();

        match self.fs.setattr(
            Context::from(in_header),
            in_header.nodeid.into(),
            st,
            handle,
            valid,
        ) {
            Ok((st, timeout)) => {
                let out = AttrOut {
                    attr_valid: timeout.as_secs(),
                    attr_valid_nsec: timeout.subsec_nanos(),
                    dummy: 0,
                    attr: st.into(),
                };
                reply_ok(Some(out), None, in_header.unique, w)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn readlink(&self, in_header: InHeader, w: Writer) -> Result<usize> {
        match self
            .fs
            .readlink(Context::from(in_header), in_header.nodeid.into())
        {
            Ok(linkname) => {
                // We need to disambiguate the option type here even though it is `None`.
                reply_ok(None::<u8>, Some(&linkname), in_header.unique, w)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn symlink(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        // Unfortunately the name and linkname are encoded one after another and
        // separated by a nul character.
        let len = (in_header.len as usize)
            .checked_sub(size_of::<InHeader>())
            .ok_or(Error::InvalidHeaderLength)?;
        let mut buf = vec![0; len];

        r.read_exact(&mut buf).map_err(Error::DecodeMessage)?;

        let mut components = buf.split_inclusive(|c| *c == b'\0');

        let name = components.next().ok_or(Error::MissingParameter)?;
        let linkname = components.next().ok_or(Error::MissingParameter)?;

        let options = FsOptions::from_bits_truncate(self.options.load(Ordering::Relaxed));

        let extensions = get_extensions(options, name.len() + linkname.len(), buf.as_slice())?;

        match self.fs.symlink(
            Context::from(in_header),
            bytes_to_cstr(linkname)?,
            in_header.nodeid.into(),
            bytes_to_cstr(name)?,
            extensions,
        ) {
            Ok(entry) => {
                let out = EntryOut::from(entry);

                reply_ok(Some(out), None, in_header.unique, w)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn mknod(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let MknodIn {
            mode, rdev, umask, ..
        } = r.read_obj().map_err(Error::DecodeMessage)?;

        let remaining_len = (in_header.len as usize)
            .checked_sub(size_of::<InHeader>())
            .and_then(|l| l.checked_sub(size_of::<MknodIn>()))
            .ok_or(Error::InvalidHeaderLength)?;
        let mut buf = vec![0; remaining_len];

        r.read_exact(&mut buf).map_err(Error::DecodeMessage)?;
        let mut components = buf.split_inclusive(|c| *c == b'\0');
        let name = components.next().ok_or(Error::MissingParameter)?;

        let options = FsOptions::from_bits_truncate(self.options.load(Ordering::Relaxed));

        let extensions = get_extensions(options, name.len(), buf.as_slice())?;

        match self.fs.mknod(
            Context::from(in_header),
            in_header.nodeid.into(),
            bytes_to_cstr(name)?,
            mode,
            rdev,
            umask,
            extensions,
        ) {
            Ok(entry) => {
                let out = EntryOut::from(entry);

                reply_ok(Some(out), None, in_header.unique, w)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn mkdir(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let MkdirIn { mode, umask } = r.read_obj().map_err(Error::DecodeMessage)?;

        let remaining_len = (in_header.len as usize)
            .checked_sub(size_of::<InHeader>())
            .and_then(|l| l.checked_sub(size_of::<MkdirIn>()))
            .ok_or(Error::InvalidHeaderLength)?;
        let mut buf = vec![0; remaining_len];

        r.read_exact(&mut buf).map_err(Error::DecodeMessage)?;
        let mut components = buf.split_inclusive(|c| *c == b'\0');
        let name = components.next().ok_or(Error::MissingParameter)?;

        let options = FsOptions::from_bits_truncate(self.options.load(Ordering::Relaxed));

        let extensions = get_extensions(options, name.len(), buf.as_slice())?;

        match self.fs.mkdir(
            Context::from(in_header),
            in_header.nodeid.into(),
            bytes_to_cstr(name)?,
            mode,
            umask,
            extensions,
        ) {
            Ok(entry) => {
                let out = EntryOut::from(entry);

                reply_ok(Some(out), None, in_header.unique, w)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn unlink(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let namelen = (in_header.len as usize)
            .checked_sub(size_of::<InHeader>())
            .ok_or(Error::InvalidHeaderLength)?;
        let mut name = vec![0; namelen];

        r.read_exact(&mut name).map_err(Error::DecodeMessage)?;

        match self.fs.unlink(
            Context::from(in_header),
            in_header.nodeid.into(),
            bytes_to_cstr(&name)?,
        ) {
            Ok(()) => reply_ok(None::<u8>, None, in_header.unique, w),
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn rmdir(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let namelen = (in_header.len as usize)
            .checked_sub(size_of::<InHeader>())
            .ok_or(Error::InvalidHeaderLength)?;
        let mut name = vec![0; namelen];

        r.read_exact(&mut name).map_err(Error::DecodeMessage)?;

        match self.fs.rmdir(
            Context::from(in_header),
            in_header.nodeid.into(),
            bytes_to_cstr(&name)?,
        ) {
            Ok(()) => reply_ok(None::<u8>, None, in_header.unique, w),
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn do_rename(
        &self,
        in_header: InHeader,
        msg_size: usize,
        newdir: u64,
        flags: u32,
        mut r: Reader,
        w: Writer,
    ) -> Result<usize> {
        let buflen = (in_header.len as usize)
            .checked_sub(size_of::<InHeader>())
            .and_then(|l| l.checked_sub(msg_size))
            .ok_or(Error::InvalidHeaderLength)?;
        let mut buf = vec![0; buflen];

        r.read_exact(&mut buf).map_err(Error::DecodeMessage)?;

        // We want to include the '\0' byte in the first slice.
        let split_pos = buf
            .iter()
            .position(|c| *c == b'\0')
            .map(|p| p + 1)
            .ok_or(Error::MissingParameter)?;

        let (oldname, newname) = buf.split_at(split_pos);

        match self.fs.rename(
            Context::from(in_header),
            in_header.nodeid.into(),
            bytes_to_cstr(oldname)?,
            newdir.into(),
            bytes_to_cstr(newname)?,
            flags,
        ) {
            Ok(()) => reply_ok(None::<u8>, None, in_header.unique, w),
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn rename(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let RenameIn { newdir } = r.read_obj().map_err(Error::DecodeMessage)?;

        self.do_rename(in_header, size_of::<RenameIn>(), newdir, 0, r, w)
    }

    fn rename2(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let Rename2In { newdir, flags, .. } = r.read_obj().map_err(Error::DecodeMessage)?;

        #[cfg(target_os = "linux")]
        let flags = flags & (libc::RENAME_EXCHANGE | libc::RENAME_NOREPLACE);

        self.do_rename(in_header, size_of::<Rename2In>(), newdir, flags, r, w)
    }

    fn link(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let LinkIn { oldnodeid } = r.read_obj().map_err(Error::DecodeMessage)?;

        let namelen = (in_header.len as usize)
            .checked_sub(size_of::<InHeader>())
            .and_then(|l| l.checked_sub(size_of::<LinkIn>()))
            .ok_or(Error::InvalidHeaderLength)?;
        let mut name = vec![0; namelen];

        r.read_exact(&mut name).map_err(Error::DecodeMessage)?;

        match self.fs.link(
            Context::from(in_header),
            oldnodeid.into(),
            in_header.nodeid.into(),
            bytes_to_cstr(&name)?,
        ) {
            Ok(entry) => {
                let out = EntryOut::from(entry);

                reply_ok(Some(out), None, in_header.unique, w)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn open(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let OpenIn {
            flags, open_flags, ..
        } = r.read_obj().map_err(Error::DecodeMessage)?;

        let kill_priv = open_flags & OPEN_KILL_SUIDGID != 0;
        let data_cap = FsOptions::from_bits_truncate(self.options.load(Ordering::Relaxed))
            .contains(FsOptions::PVISOR_DATA_OBJECT);
        // The single request/capability admission point. Actual returned OPEN
        // options are checked by OpenReply for both legacy and shared evidence.
        #[cfg(target_os = "linux")]
        let direct = flags as i32 & (libc::O_DIRECT | libc::O_PATH) != 0;
        #[cfg(not(target_os = "linux"))]
        let direct = false;
        let request_eligible = data_cap
            && !direct
            && !kill_priv
            && (flags as i32 & libc::O_ACCMODE) == libc::O_RDONLY
            && !pvisor_overlay_core::service::FilesystemService::is_write_open(flags as i32);
        match self.fs.open_for_reply(
            Context::from(in_header),
            in_header.nodeid.into(),
            kill_priv,
            flags,
            request_eligible,
        ) {
            Ok(reply) => {
                let data_object = reply
                    .allows_data_object(request_eligible)
                    .then_some(reply.data_object)
                    .flatten()
                    .filter(|object| object.id != 0);
                let mut opts = reply.options;
                if !FsOptions::from_bits_truncate(self.options.load(Ordering::Relaxed))
                    .contains(FsOptions::PVISOR_WRITEBACK)
                {
                    opts.remove(OpenOptions::PVISOR_LOWER);
                }
                if !FsOptions::from_bits_truncate(self.options.load(Ordering::Relaxed))
                    .contains(FsOptions::PVISOR_LOCAL_FLUSH)
                {
                    opts.remove(OpenOptions::PVISOR_LOCAL_FLUSH);
                }
                let fh = reply.handle.unwrap_or(0);
                opts.remove(OpenOptions::PVISOR_DATA_OBJECT);
                if data_object.is_some() {
                    opts.insert(OpenOptions::PVISOR_DATA_OBJECT);
                }
                let out = OpenOut {
                    fh,
                    open_flags: opts.bits(),
                    backing_id: data_object.map_or(0, |object| object.id),
                };
                // All OPENs in this negotiated connection have the same reply
                // shape. CREATE and OPENDIR deliberately retain the base form.
                let fixed_size = data_object.map_or(0, |object| object.size).to_le_bytes();
                reply_ok(
                    Some(out),
                    data_cap.then_some(fixed_size.as_slice()),
                    in_header.unique,
                    w,
                )
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn read(&self, in_header: InHeader, mut r: Reader, mut w: Writer) -> Result<usize> {
        let ReadIn {
            fh,
            offset,
            size,
            read_flags,
            lock_owner,
            flags,
            ..
        } = r.read_obj().map_err(Error::DecodeMessage)?;

        if size > MAX_BUFFER_SIZE {
            return reply_error(
                linux_error(io::Error::from_raw_os_error(libc::ENOMEM)),
                in_header.unique,
                w,
            );
        }

        let owner = if read_flags & READ_LOCKOWNER != 0 {
            Some(lock_owner)
        } else {
            None
        };

        // Split the writer into 2 pieces: one for the `OutHeader` and the rest for the data.
        let data_writer = ZCWriter(w.split_at(size_of::<OutHeader>()).unwrap());
        let read_completion = data_writer.0.file_read_completion();

        match self.fs.read(
            Context::from(in_header),
            in_header.nodeid.into(),
            fh.into(),
            data_writer,
            size,
            offset,
            owner,
            flags,
        ) {
            Ok(count) => {
                // Don't use `reply_ok` because we need to set a custom size length for the
                // header.
                let out = OutHeader {
                    len: (size_of::<OutHeader>() + count) as u32,
                    error: 0,
                    unique: in_header.unique,
                };

                w.write_all(out.as_slice()).map_err(Error::EncodeMessage)?;
                if let Some(completion) = read_completion {
                    completion.finish(count);
                }
                Ok(out.len as usize)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn write(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let WriteIn {
            fh,
            offset,
            size,
            write_flags,
            lock_owner,
            flags,
            ..
        } = r.read_obj().map_err(Error::DecodeMessage)?;

        if size > MAX_BUFFER_SIZE {
            return reply_error(
                linux_error(io::Error::from_raw_os_error(libc::ENOMEM)),
                in_header.unique,
                w,
            );
        }

        let owner = if write_flags & WRITE_LOCKOWNER != 0 {
            Some(lock_owner)
        } else {
            None
        };

        let delayed_write = write_flags & WRITE_CACHE != 0;
        let kill_priv = write_flags & WRITE_KILL_PRIV != 0;

        self.profile.add("write_payload_bytes", u64::from(size));
        self.profile
            .add("write_background_requests", u64::from(delayed_write));
        self.profile.add(
            match size {
                0..=1024 => "write_size_le_1024",
                1025..=4096 => "write_size_le_4096",
                4097..=16384 => "write_size_le_16384",
                16385..=65536 => "write_size_le_65536",
                65537..=131072 => "write_size_le_131072",
                131073..=1048576 => "write_size_le_1048576",
                _ => "write_size_gt_1048576",
            },
            1,
        );

        let data_reader = ZCReader(r);

        match self.fs.write(
            Context::from(in_header),
            in_header.nodeid.into(),
            fh.into(),
            data_reader,
            size,
            offset,
            owner,
            delayed_write,
            kill_priv,
            flags,
        ) {
            Ok(count) if count > size as usize || (delayed_write && count != size as usize) => {
                // Background writeback cannot retry a successful short reply.
                reply_error(
                    linux_error(io::Error::from_raw_os_error(libc::EIO)),
                    in_header.unique,
                    w,
                )
            }
            Ok(count) => {
                let out = WriteOut {
                    size: count as u32,
                    ..Default::default()
                };

                reply_ok(Some(out), None, in_header.unique, w)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn statfs(&self, in_header: InHeader, w: Writer) -> Result<usize> {
        match self
            .fs
            .statfs(Context::from(in_header), in_header.nodeid.into())
        {
            Ok(st) => reply_ok(Some(Kstatfs::from(st)), None, in_header.unique, w),
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn release(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let ReleaseIn {
            fh,
            flags,
            release_flags,
            lock_owner,
        } = r.read_obj().map_err(Error::DecodeMessage)?;

        let flush = release_flags & RELEASE_FLUSH != 0;
        let flock_release = release_flags & RELEASE_FLOCK_UNLOCK != 0;
        let lock_owner = if flush || flock_release {
            Some(lock_owner)
        } else {
            None
        };

        match self.fs.release(
            Context::from(in_header),
            in_header.nodeid.into(),
            flags,
            fh.into(),
            flush,
            flock_release,
            lock_owner,
        ) {
            Ok(()) => reply_ok(None::<u8>, None, in_header.unique, w),
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn fsync(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let FsyncIn {
            fh, fsync_flags, ..
        } = r.read_obj().map_err(Error::DecodeMessage)?;
        let datasync = fsync_flags & 0x1 != 0;

        match self.fs.fsync(
            Context::from(in_header),
            in_header.nodeid.into(),
            datasync,
            fh.into(),
        ) {
            Ok(()) => reply_ok(None::<u8>, None, in_header.unique, w),
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn syncfs(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        if (in_header.len as usize) < size_of::<InHeader>() + size_of::<SyncfsIn>() {
            return Err(Error::InvalidHeaderLength);
        }
        let _: SyncfsIn = r.read_obj().map_err(Error::DecodeMessage)?;
        match self.fs.syncfs(Context::from(in_header)) {
            Ok(()) => reply_ok(None::<u8>, None, in_header.unique, w),
            Err(error) => reply_error(error, in_header.unique, w),
        }
    }

    fn setxattr(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let SetxattrIn { size, flags } = r.read_obj().map_err(Error::DecodeMessage)?;

        // The name and value and encoded one after another and separated by a '\0' character.
        let len = (in_header.len as usize)
            .checked_sub(size_of::<InHeader>())
            .and_then(|l| l.checked_sub(size_of::<SetxattrIn>()))
            .ok_or(Error::InvalidHeaderLength)?;
        let mut buf = vec![0; len];

        r.read_exact(&mut buf).map_err(Error::DecodeMessage)?;

        // We want to include the '\0' byte in the first slice.
        let split_pos = buf
            .iter()
            .position(|c| *c == b'\0')
            .map(|p| p + 1)
            .ok_or(Error::MissingParameter)?;

        let (name, value) = buf.split_at(split_pos);

        if size != value.len() as u32 {
            return Err(Error::InvalidXattrSize((size, value.len())));
        }

        match self.fs.setxattr(
            Context::from(in_header),
            in_header.nodeid.into(),
            bytes_to_cstr(name)?,
            value,
            flags,
        ) {
            Ok(()) => reply_ok(None::<u8>, None, in_header.unique, w),
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn getxattr(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let GetxattrIn { size, .. } = r.read_obj().map_err(Error::DecodeMessage)?;

        let namelen = (in_header.len as usize)
            .checked_sub(size_of::<InHeader>())
            .and_then(|l| l.checked_sub(size_of::<GetxattrIn>()))
            .ok_or(Error::InvalidHeaderLength)?;
        let mut name = vec![0; namelen];

        r.read_exact(&mut name).map_err(Error::DecodeMessage)?;

        if size > MAX_BUFFER_SIZE {
            return reply_error(
                linux_error(io::Error::from_raw_os_error(libc::ENOMEM)),
                in_header.unique,
                w,
            );
        }

        match self.fs.getxattr(
            Context::from(in_header),
            in_header.nodeid.into(),
            bytes_to_cstr(&name)?,
            size,
        ) {
            Ok(GetxattrReply::Value(val)) => reply_ok(None::<u8>, Some(&val), in_header.unique, w),
            Ok(GetxattrReply::Count(count)) => {
                let out = GetxattrOut {
                    size: count,
                    ..Default::default()
                };

                reply_ok(Some(out), None, in_header.unique, w)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn listxattr(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let GetxattrIn { size, .. } = r.read_obj().map_err(Error::DecodeMessage)?;

        if size > MAX_BUFFER_SIZE {
            return reply_error(
                linux_error(io::Error::from_raw_os_error(libc::ENOMEM)),
                in_header.unique,
                w,
            );
        }

        match self
            .fs
            .listxattr(Context::from(in_header), in_header.nodeid.into(), size)
        {
            Ok(ListxattrReply::Names(val)) => reply_ok(None::<u8>, Some(&val), in_header.unique, w),
            Ok(ListxattrReply::Count(count)) => {
                let out = GetxattrOut {
                    size: count,
                    ..Default::default()
                };

                reply_ok(Some(out), None, in_header.unique, w)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn removexattr(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let namelen = (in_header.len as usize)
            .checked_sub(size_of::<InHeader>())
            .ok_or(Error::InvalidHeaderLength)?;

        let mut buf = vec![0; namelen];

        r.read_exact(&mut buf).map_err(Error::DecodeMessage)?;

        let name = bytes_to_cstr(&buf)?;

        match self
            .fs
            .removexattr(Context::from(in_header), in_header.nodeid.into(), name)
        {
            Ok(()) => reply_ok(None::<u8>, None, in_header.unique, w),
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn flush(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let FlushIn { fh, lock_owner, .. } = r.read_obj().map_err(Error::DecodeMessage)?;

        match self.fs.flush(
            Context::from(in_header),
            in_header.nodeid.into(),
            fh.into(),
            lock_owner,
        ) {
            Ok(()) => reply_ok(None::<u8>, None, in_header.unique, w),
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn init(
        &self,
        in_header: InHeader,
        mut r: Reader,
        w: Writer,
        allow_idmap: bool,
    ) -> Result<usize> {
        let InitInCompat {
            major,
            minor,
            max_readahead,
            flags,
        } = r.read_obj().map_err(Error::DecodeMessage)?;

        let options = FsOptions::from_bits_truncate(flags as u64);

        let InitInExt { flags2, .. } = if options.contains(FsOptions::INIT_EXT) {
            r.read_obj().map_err(Error::DecodeMessage)?
        } else {
            InitInExt::default()
        };

        if major < KERNEL_VERSION {
            error!("Unsupported fuse protocol version: {major}.{minor}");
            return reply_error(
                linux_error(io::Error::from_raw_os_error(libc::EPROTO)),
                in_header.unique,
                w,
            );
        }

        if major > KERNEL_VERSION {
            // Wait for the kernel to reply back with a 7.X version.
            let out = InitOut {
                major: KERNEL_VERSION,
                minor: KERNEL_MINOR_VERSION,
                ..Default::default()
            };

            return reply_ok(Some(out), None, in_header.unique, w);
        }

        if minor < KERNEL_MINOR_VERSION {
            error!("Unsupported fuse protocol minor version: {major}.{minor}");
            return reply_error(
                linux_error(io::Error::from_raw_os_error(libc::EPROTO)),
                in_header.unique,
                w,
            );
        }

        // These fuse features are supported by this server by default.
        let mut supported = FsOptions::ASYNC_READ
            | FsOptions::PARALLEL_DIROPS
            | FsOptions::BIG_WRITES
            | FsOptions::AUTO_INVAL_DATA
            | FsOptions::ASYNC_DIO
            | FsOptions::HAS_IOCTL_DIR
            | FsOptions::ATOMIC_O_TRUNC
            | FsOptions::MAX_PAGES
            | FsOptions::SUBMOUNTS
            | FsOptions::HANDLE_KILLPRIV_V2
            | FsOptions::INIT_EXT
            | FsOptions::PVISOR_META;

        if allow_idmap {
            supported |= FsOptions::ALLOW_IDMAP;
        }

        if cfg!(target_os = "macos") {
            supported |= FsOptions::SECURITY_CTX;
        }

        let flags_64 = ((flags2 as u64) << 32) | (flags as u64);
        let mut capable = FsOptions::from_bits_truncate(flags_64);
        if !self.guest_cache {
            capable.remove(
                FsOptions::PVISOR_META
                    | FsOptions::PVISOR_LOCAL_FLUSH
                    | FsOptions::PVISOR_DATA_OBJECT,
            );
        }

        let page_size: u32 = unsafe { libc::sysconf(libc::_SC_PAGESIZE).try_into().unwrap() };

        match self.fs.init(capable) {
            Ok(want) => {
                for label in [
                    "write_payload_bytes",
                    "write_background_requests",
                    "write_size_le_1024",
                    "write_size_le_4096",
                    "write_size_le_16384",
                    "write_size_le_65536",
                    "write_size_le_131072",
                    "write_size_le_1048576",
                    "write_size_gt_1048576",
                ] {
                    self.profile.add(label, 0);
                }
                let mut enabled = capable & (want | supported);
                if !enabled.contains(FsOptions::WRITEBACK_CACHE) {
                    enabled.remove(FsOptions::PVISOR_WRITEBACK);
                }
                if !cfg!(target_os = "linux")
                    || enabled.intersects(
                        FsOptions::WRITEBACK_CACHE
                            | FsOptions::POSIX_LOCKS
                            | FsOptions::FLOCK_LOCKS
                            | FsOptions::HAS_INODE_DAX
                            | FsOptions::MAP_ALIGNMENT,
                    )
                {
                    enabled.remove(FsOptions::PVISOR_LOCAL_FLUSH | FsOptions::PVISOR_DATA_OBJECT);
                }
                let batched = enabled.contains(FsOptions::PVISOR_WRITEBACK);
                let enabled = enabled.bits();
                self.options.store(enabled, Ordering::Relaxed);

                let out = InitOut {
                    major: KERNEL_VERSION,
                    minor: KERNEL_MINOR_VERSION,
                    max_readahead,
                    flags: enabled as u32,
                    max_background: if batched { 64 } else { u16::MAX },
                    congestion_threshold: if batched { 48 } else { (u16::MAX / 4) * 3 },
                    max_write: MAX_BUFFER_SIZE,
                    time_gran: 1, // nanoseconds
                    max_pages: NEGOTIATED_MAX_PAGES,
                    map_alignment: if enabled & FsOptions::MAP_ALIGNMENT.bits() != 0 {
                        page_size.trailing_zeros() as u16
                    } else {
                        0
                    },
                    flags2: (enabled >> 32) as u32,
                    ..Default::default()
                };

                reply_ok(Some(out), None, in_header.unique, w)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn opendir(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let OpenIn { flags, .. } = r.read_obj().map_err(Error::DecodeMessage)?;

        match self
            .fs
            .opendir(Context::from(in_header), in_header.nodeid.into(), flags)
        {
            Ok((handle, mut opts)) => {
                opts.remove(OpenOptions::PVISOR_LOCAL_FLUSH | OpenOptions::PVISOR_DATA_OBJECT);
                let out = OpenOut {
                    fh: handle.map(Into::into).unwrap_or(0),
                    open_flags: opts.bits(),
                    ..Default::default()
                };

                reply_ok(Some(out), None, in_header.unique, w)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn do_readdir(
        &self,
        in_header: InHeader,
        mut r: Reader,
        mut w: Writer,
        plus: bool,
    ) -> Result<usize> {
        let ReadIn {
            fh, offset, size, ..
        } = r.read_obj().map_err(Error::DecodeMessage)?;

        if size > MAX_BUFFER_SIZE {
            return reply_error(
                linux_error(io::Error::from_raw_os_error(libc::ENOMEM)),
                in_header.unique,
                w,
            );
        }

        let available_bytes = w.available_bytes();
        if available_bytes < size as usize {
            return reply_error(
                linux_error(io::Error::from_raw_os_error(libc::ENOMEM)),
                in_header.unique,
                w,
            );
        }

        // Skip over enough bytes for the header.
        let mut cursor = w.split_at(size_of::<OutHeader>()).unwrap();
        let mut lookups = DirectoryReplyLookups {
            fs: &self.fs,
            ctx: Context::from(in_header),
            accepted: Vec::new(),
        };

        let res = if plus {
            self.fs.readdirplus(
                Context::from(in_header),
                in_header.nodeid.into(),
                fh.into(),
                size,
                offset,
                |d, e| {
                    let inode = e.inode;
                    let written = add_dirent(&mut cursor, size, d, Some(e))?;
                    if written != 0 && inode != 0 && inode != ROOT_ID {
                        lookups.accepted.push(inode);
                    }
                    Ok(written)
                },
            )
        } else {
            self.fs.readdir(
                Context::from(in_header),
                in_header.nodeid.into(),
                fh.into(),
                size,
                offset,
                |d| add_dirent(&mut cursor, size, d, None),
            )
        };

        if let Err(e) = res {
            reply_error(e, in_header.unique, w)
        } else {
            // Don't use `reply_ok` because we need to set a custom size length for the
            // header.
            let out = OutHeader {
                len: (size_of::<OutHeader>() + cursor.bytes_written()) as u32,
                error: 0,
                unique: in_header.unique,
            };

            w.write_all(out.as_slice()).map_err(Error::EncodeMessage)?;
            // The guest now owns every accepted entry. Header encode failure
            // above leaves the guard armed, just like an overall FS error.
            lookups.accepted.clear();
            Ok(out.len as usize)
        }
    }

    fn readdir(&self, in_header: InHeader, r: Reader, w: Writer) -> Result<usize> {
        self.do_readdir(in_header, r, w, false)
    }

    fn readdirplus(&self, in_header: InHeader, r: Reader, w: Writer) -> Result<usize> {
        self.do_readdir(in_header, r, w, true)
    }

    fn read_index(&self, in_header: InHeader, mut r: Reader, mut w: Writer) -> Result<usize> {
        let options = FsOptions::from_bits_truncate(self.options.load(Ordering::Relaxed));
        if !options.contains(FsOptions::PVISOR_META) {
            return reply_error(
                linux_error(io::Error::from_raw_os_error(libc::ENOSYS)),
                in_header.unique,
                w,
            );
        }
        let ReadIn { offset, size, .. } = r.read_obj().map_err(Error::DecodeMessage)?;
        if size > MAX_BUFFER_SIZE {
            return reply_error(
                linux_error(io::Error::from_raw_os_error(libc::ENOMEM)),
                in_header.unique,
                w,
            );
        }
        match super::guest_index::read_directory_page(
            &self.fs,
            Context::from(in_header),
            in_header.nodeid.into(),
            offset,
            size,
        ) {
            Ok(mut page) => {
                let payload = super::guest_index::encode_page(&page);
                if w.available_bytes() < size_of::<OutHeader>() + payload.len() {
                    super::guest_index::cancel_directory_page(
                        &self.fs,
                        Context::from(in_header),
                        &mut page,
                    );
                    return reply_error(
                        linux_error(io::Error::from_raw_os_error(libc::ENOMEM)),
                        in_header.unique,
                        w,
                    );
                }
                let out = OutHeader {
                    len: (size_of::<OutHeader>() + payload.len()) as u32,
                    error: 0,
                    unique: in_header.unique,
                };
                if let Err(error) = w
                    .write_all(out.as_slice())
                    .and_then(|_| w.write_all(&payload))
                {
                    super::guest_index::cancel_directory_page(
                        &self.fs,
                        Context::from(in_header),
                        &mut page,
                    );
                    return Err(Error::EncodeMessage(error));
                }
                Ok(out.len as usize)
            }
            Err(error) => reply_error(error, in_header.unique, w),
        }
    }

    fn releasedir(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let ReleaseIn { fh, flags, .. } = r.read_obj().map_err(Error::DecodeMessage)?;

        match self.fs.releasedir(
            Context::from(in_header),
            in_header.nodeid.into(),
            flags,
            fh.into(),
        ) {
            Ok(()) => reply_ok(None::<u8>, None, in_header.unique, w),
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn fsyncdir(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let FsyncIn {
            fh, fsync_flags, ..
        } = r.read_obj().map_err(Error::DecodeMessage)?;
        let datasync = fsync_flags & 0x1 != 0;

        match self.fs.fsyncdir(
            Context::from(in_header),
            in_header.nodeid.into(),
            datasync,
            fh.into(),
        ) {
            Ok(()) => reply_ok(None::<u8>, None, in_header.unique, w),
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn getlk(&self, in_header: InHeader, mut _r: Reader, w: Writer) -> Result<usize> {
        if let Err(e) = self.fs.getlk() {
            reply_error(e, in_header.unique, w)
        } else {
            Ok(0)
        }
    }

    fn setlk(&self, in_header: InHeader, mut _r: Reader, w: Writer) -> Result<usize> {
        if let Err(e) = self.fs.setlk() {
            reply_error(e, in_header.unique, w)
        } else {
            Ok(0)
        }
    }

    fn setlkw(&self, in_header: InHeader, mut _r: Reader, w: Writer) -> Result<usize> {
        if let Err(e) = self.fs.setlkw() {
            reply_error(e, in_header.unique, w)
        } else {
            Ok(0)
        }
    }

    fn access(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let AccessIn { mask, .. } = r.read_obj().map_err(Error::DecodeMessage)?;

        match self
            .fs
            .access(Context::from(in_header), in_header.nodeid.into(), mask)
        {
            Ok(()) => reply_ok(None::<u8>, None, in_header.unique, w),
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn create(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let CreateIn {
            flags,
            mode,
            umask,
            open_flags,
            ..
        } = r.read_obj().map_err(Error::DecodeMessage)?;

        let namelen = (in_header.len as usize)
            .checked_sub(size_of::<InHeader>())
            .and_then(|l| l.checked_sub(size_of::<CreateIn>()))
            .ok_or(Error::InvalidHeaderLength)?;

        let mut buf = vec![0; namelen];

        r.read_exact(&mut buf).map_err(Error::DecodeMessage)?;
        let mut components = buf.split_inclusive(|c| *c == b'\0');
        let name = components.next().ok_or(Error::MissingParameter)?;

        let options = FsOptions::from_bits_truncate(self.options.load(Ordering::Relaxed));

        let extensions = get_extensions(options, name.len(), buf.as_slice())?;

        let kill_priv = open_flags & OPEN_KILL_SUIDGID != 0;

        match self.fs.create(
            Context::from(in_header),
            in_header.nodeid.into(),
            bytes_to_cstr(name)?,
            mode,
            kill_priv,
            flags,
            umask,
            extensions,
        ) {
            Ok((entry, handle, mut opts)) => {
                // CREATE returns a writable namespace object, never a
                // qualified descriptor from an immutable physical lower.
                opts.remove(OpenOptions::PVISOR_LOCAL_FLUSH | OpenOptions::PVISOR_DATA_OBJECT);
                let entry_out = EntryOut {
                    nodeid: entry.inode,
                    generation: entry.generation,
                    entry_valid: entry.entry_timeout.as_secs(),
                    attr_valid: entry.attr_timeout.as_secs(),
                    entry_valid_nsec: entry.entry_timeout.subsec_nanos(),
                    attr_valid_nsec: entry.attr_timeout.subsec_nanos(),
                    attr: entry.attr.into(),
                };
                let open_out = OpenOut {
                    fh: handle.map(Into::into).unwrap_or(0),
                    open_flags: opts.bits(),
                    ..Default::default()
                };

                // Kind of a hack to write both structs.
                reply_ok(
                    Some(entry_out),
                    Some(open_out.as_slice()),
                    in_header.unique,
                    w,
                )
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn interrupt(&self, _in_header: InHeader) -> Result<usize> {
        Ok(0)
    }

    fn bmap(&self, in_header: InHeader, mut _r: Reader, w: Writer) -> Result<usize> {
        if let Err(e) = self.fs.bmap() {
            reply_error(e, in_header.unique, w)
        } else {
            Ok(0)
        }
    }

    fn destroy(&self) -> Result<usize> {
        // No reply to this function.
        self.fs.destroy();

        Ok(0)
    }

    fn ioctl(
        &self,
        in_header: InHeader,
        mut r: Reader,
        w: Writer,
        exit_code: &Arc<AtomicI32>,
    ) -> Result<usize> {
        let IoctlIn {
            fh,
            flags,
            cmd,
            arg,
            in_size,
            out_size,
        } = r.read_obj().map_err(Error::DecodeMessage)?;

        // Linux _IOW('v', 0xf1, u64). This private diagnostic command is
        // synchronous: all counters are copied and emitted before its reply.
        // Disabled profiling retains the filesystem's ordinary ENOTTY path.
        if cmd == PROFILE_PHASE_IOCTL && self.profile.report().is_some() {
            if in_size != 8 || out_size != 0 || self.profile_scope.is_none() {
                return reply_error(
                    io::Error::from_raw_os_error(libc::EINVAL),
                    in_header.unique,
                    w,
                );
            }
            let mut bytes = [0u8; 8];
            r.read_exact(&mut bytes).map_err(Error::DecodeMessage)?;
            if let Some(record) = self.phase_profiles(u64::from_le_bytes(bytes)) {
                eprintln!("pvisor-fs-profile-phase {record}");
            }
            return reply_ok(Some(IoctlOut::default()), None, in_header.unique, w);
        }

        match self.fs.ioctl(
            Context::from(in_header),
            in_header.nodeid.into(),
            fh.into(),
            flags,
            cmd,
            arg,
            in_size,
            out_size,
            exit_code,
        ) {
            Ok(data) => {
                let out = IoctlOut {
                    result: 0,
                    ..Default::default()
                };
                reply_ok(Some(out), Some(&data), in_header.unique, w)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn poll(&self, in_header: InHeader, mut _r: Reader, w: Writer) -> Result<usize> {
        if let Err(e) = self.fs.poll() {
            reply_error(e, in_header.unique, w)
        } else {
            Ok(0)
        }
    }

    fn notify_reply(&self, in_header: InHeader, mut _r: Reader, w: Writer) -> Result<usize> {
        if let Err(e) = self.fs.notify_reply() {
            reply_error(e, in_header.unique, w)
        } else {
            Ok(0)
        }
    }

    fn batch_forget(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let BatchForgetIn { count, .. } = r.read_obj().map_err(Error::DecodeMessage)?;

        if let Some(size) = (count as usize).checked_mul(size_of::<ForgetOne>()) {
            if size > MAX_BUFFER_SIZE as usize {
                return reply_error(
                    linux_error(io::Error::from_raw_os_error(libc::ENOMEM)),
                    in_header.unique,
                    w,
                );
            }
        } else {
            return reply_error(
                linux_error(io::Error::from_raw_os_error(libc::EOVERFLOW)),
                in_header.unique,
                w,
            );
        }

        let mut requests = Vec::with_capacity(count as usize);
        for _ in 0..count {
            requests.push(
                r.read_obj::<ForgetOne>()
                    .map(|f| (f.nodeid.into(), f.nlookup))
                    .map_err(Error::DecodeMessage)?,
            );
        }

        self.fs.batch_forget(Context::from(in_header), requests);

        // No reply for forget messages.
        Ok(0)
    }

    fn fallocate(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let FallocateIn {
            fh,
            offset,
            length,
            mode,
            ..
        } = r.read_obj().map_err(Error::DecodeMessage)?;

        match self.fs.fallocate(
            Context::from(in_header),
            in_header.nodeid.into(),
            fh.into(),
            mode,
            offset,
            length,
        ) {
            Ok(()) => reply_ok(None::<u8>, None, in_header.unique, w),
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn lseek(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let LseekIn {
            fh, offset, whence, ..
        } = r.read_obj().map_err(Error::DecodeMessage)?;

        match self.fs.lseek(
            Context::from(in_header),
            in_header.nodeid.into(),
            fh.into(),
            offset,
            whence,
        ) {
            Ok(offset) => {
                let out = LseekOut { offset };

                reply_ok(Some(out), None, in_header.unique, w)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn copyfilerange(&self, in_header: InHeader, mut r: Reader, w: Writer) -> Result<usize> {
        let CopyfilerangeIn {
            fh_in,
            off_in,
            nodeid_out,
            fh_out,
            off_out,
            len,
            flags,
            ..
        } = r.read_obj().map_err(Error::DecodeMessage)?;

        match self.fs.copyfilerange(
            Context::from(in_header),
            in_header.nodeid.into(),
            fh_in.into(),
            off_in,
            nodeid_out.into(),
            fh_out.into(),
            off_out,
            len,
            flags,
        ) {
            Ok(count) => {
                let out = WriteOut {
                    size: count as u32,
                    ..Default::default()
                };

                reply_ok(Some(out), None, in_header.unique, w)
            }
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn setupmapping(
        &self,
        in_header: InHeader,
        mut r: Reader,
        w: Writer,
        host_shm_base: u64,
        shm_size: u64,
        #[cfg(target_os = "macos")] map_sender: &Option<Sender<WorkerMessage>>,
    ) -> Result<usize> {
        let SetupmappingIn {
            fh,
            foffset,
            len,
            flags,
            moffset,
        } = r.read_obj().map_err(Error::DecodeMessage)?;

        match self.fs.setupmapping(
            Context::from(in_header),
            in_header.nodeid.into(),
            fh.into(),
            foffset,
            len,
            flags,
            moffset,
            host_shm_base,
            shm_size,
            #[cfg(target_os = "macos")]
            map_sender,
        ) {
            Ok(()) => reply_ok(None::<u8>, None, in_header.unique, w),
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }

    fn removemapping(
        &self,
        in_header: InHeader,
        mut r: Reader,
        w: Writer,
        host_shm_base: u64,
        shm_size: u64,
        #[cfg(target_os = "macos")] map_sender: &Option<Sender<WorkerMessage>>,
    ) -> Result<usize> {
        let RemovemappingIn { count } = r.read_obj().map_err(Error::DecodeMessage)?;

        if let Some(size) = (count as usize).checked_mul(size_of::<RemovemappingOne>()) {
            if size > MAX_BUFFER_SIZE as usize {
                return reply_error(
                    linux_error(io::Error::from_raw_os_error(libc::ENOMEM)),
                    in_header.unique,
                    w,
                );
            }
        } else {
            return reply_error(
                linux_error(io::Error::from_raw_os_error(libc::EOVERFLOW)),
                in_header.unique,
                w,
            );
        }

        let mut requests = Vec::with_capacity(count as usize);
        for _ in 0..count {
            requests.push(
                r.read_obj::<RemovemappingOne>()
                    .map_err(Error::DecodeMessage)?,
            );
        }

        match self.fs.removemapping(
            Context::from(in_header),
            requests,
            host_shm_base,
            shm_size,
            #[cfg(target_os = "macos")]
            map_sender,
        ) {
            Ok(()) => reply_ok(None::<u8>, None, in_header.unique, w),
            Err(e) => reply_error(e, in_header.unique, w),
        }
    }
}

fn reply_ok<T: ByteValued>(
    out: Option<T>,
    data: Option<&[u8]>,
    unique: u64,
    mut w: Writer,
) -> Result<usize> {
    let mut len = size_of::<OutHeader>();

    if out.is_some() {
        len += size_of::<T>();
    }

    if let Some(data) = data {
        len += data.len();
    }

    let header = OutHeader {
        len: len as u32,
        error: 0,
        unique,
    };

    w.write_all(header.as_slice())
        .map_err(Error::EncodeMessage)?;

    if let Some(out) = out {
        w.write_all(out.as_slice()).map_err(Error::EncodeMessage)?;
    }

    if let Some(data) = data {
        w.write_all(data).map_err(Error::EncodeMessage)?;
    }

    debug_assert_eq!(len, w.bytes_written());
    Ok(w.bytes_written())
}

fn reply_error(e: io::Error, unique: u64, mut w: Writer) -> Result<usize> {
    debug!("reply error unique={unique} errno={:?}", e.raw_os_error());
    let header = OutHeader {
        len: size_of::<OutHeader>() as u32,
        error: -e.raw_os_error().unwrap_or(libc::EIO),
        unique,
    };

    w.write_all(header.as_slice())
        .map_err(Error::EncodeMessage)?;

    debug_assert_eq!(header.len as usize, w.bytes_written());
    Ok(w.bytes_written())
}

fn bytes_to_cstr(buf: &[u8]) -> Result<&CStr> {
    // Convert to a `CStr` first so that we can drop the '\0' byte at the end
    // and make sure there are no interior '\0' bytes.
    CStr::from_bytes_with_nul(buf).map_err(Error::InvalidCString)
}

fn add_dirent(
    cursor: &mut Writer,
    max: u32,
    d: DirEntry,
    entry: Option<Entry>,
) -> io::Result<usize> {
    if d.name.len() > u32::MAX as usize {
        return Err(linux_error(io::Error::from_raw_os_error(libc::EOVERFLOW)));
    }

    let dirent_len = size_of::<Dirent>()
        .checked_add(d.name.len())
        .ok_or_else(|| linux_error(io::Error::from_raw_os_error(libc::EOVERFLOW)))?;

    // Directory entries must be padded to 8-byte alignment.  If adding 7 causes
    // an overflow then this dirent cannot be properly padded.
    let padded_dirent_len = dirent_len
        .checked_add(7)
        .map(|l| l & !7)
        .ok_or_else(|| linux_error(io::Error::from_raw_os_error(libc::EOVERFLOW)))?;

    let total_len = if entry.is_some() {
        padded_dirent_len
            .checked_add(size_of::<EntryOut>())
            .ok_or_else(|| linux_error(io::Error::from_raw_os_error(libc::EOVERFLOW)))?
    } else {
        padded_dirent_len
    };

    if (max as usize).saturating_sub(cursor.bytes_written()) < total_len {
        Ok(0)
    } else {
        if let Some(entry) = entry {
            cursor.write_all(EntryOut::from(entry).as_slice())?;
        }

        let dirent = Dirent {
            ino: d.ino,
            off: d.offset,
            namelen: d.name.len() as u32,
            type_: d.type_,
        };

        cursor.write_all(dirent.as_slice())?;
        cursor.write_all(d.name)?;

        // We know that `dirent_len` <= `padded_dirent_len` due to the check above
        // so there's no need for checked arithmetic.
        let padding = padded_dirent_len - dirent_len;
        if padding > 0 {
            cursor.write_all(&DIRENT_PADDING[..padding])?;
        }

        Ok(total_len)
    }
}

fn take_object<T: ByteValued>(data: &[u8]) -> Result<(T, &[u8])> {
    if data.len() < size_of::<T>() {
        return Err(Error::DecodeMessage(einval()));
    }

    let (object_bytes, remaining_bytes) = data.split_at(size_of::<T>());
    // SAFETY: `T` implements `ByteValued` that guarantees that it is safe to instantiate
    // `T` with random data.
    let object: T = unsafe { std::ptr::read_unaligned(object_bytes.as_ptr() as *const T) };
    Ok((object, remaining_bytes))
}

fn parse_security_context(nr_secctx: u32, data: &[u8]) -> Result<Option<SecContext>> {
    // Although the FUSE security context extension allows sending several security contexts,
    // currently the guest kernel only sends one.
    if nr_secctx > 1 {
        return Err(Error::DecodeMessage(einval()));
    } else if nr_secctx == 0 {
        // No security context sent. May be no LSM supports it.
        return Ok(None);
    }

    let (secctx, data) = take_object::<Secctx>(data)?;

    if secctx.size == 0 {
        return Err(Error::DecodeMessage(einval()));
    }

    let mut components = data.split_inclusive(|c| *c == b'\0');
    let secctx_name = components.next().ok_or(Error::MissingParameter)?;
    let (_, data) = data.split_at(secctx_name.len());

    if data.len() < secctx.size as usize {
        return Err(Error::DecodeMessage(einval()));
    }

    // Fuse client aligns the whole security context block to 64 byte
    // boundary. So it is possible that after actual security context
    // of secctx.size, there are some null padding bytes left. If
    // we ever parse more data after secctx, we will have to take those
    // null bytes into account. Total size (including null bytes) is
    // available in SecctxHeader->size.
    let (remaining, _) = data.split_at(secctx.size as usize);

    let fuse_secctx = SecContext {
        name: CString::from_vec_with_nul(secctx_name.to_vec()).map_err(Error::InvalidCString2)?,
        secctx: remaining.to_vec(),
    };

    Ok(Some(fuse_secctx))
}

fn get_extensions(options: FsOptions, skip: usize, request_bytes: &[u8]) -> Result<Extensions> {
    let mut extensions = Extensions::default();

    if !(options.contains(FsOptions::SECURITY_CTX)
        || options.contains(FsOptions::CREATE_SUPP_GROUP))
    {
        return Ok(extensions);
    }

    // It's not guaranty to receive an extension even if it's supported by the guest kernel
    if request_bytes.len() < skip {
        return Err(Error::DecodeMessage(einval()));
    }

    // We need to track if a SecCtx was received, because it's valid
    // for the guest to send an empty SecCtx (i.e, nr_secctx == 0)
    let mut secctx_received = false;

    let mut buf = &request_bytes[skip..];
    while !buf.is_empty() {
        let (extension_header, remaining_bytes) = take_object::<ExtHeader>(buf)?;

        let extension_size = (extension_header.size as usize)
            .checked_sub(size_of::<ExtHeader>())
            .ok_or(Error::InvalidHeaderLength)?;

        let (current_extension_bytes, next_extension_bytes) =
            remaining_bytes.split_at(extension_size);

        let ext_type = ExtType::try_from(extension_header.ext_type)
            .map_err(|_| Error::DecodeMessage(einval()))?;

        match ext_type {
            ExtType::SecCtx(nr_secctx) => {
                if !options.contains(FsOptions::SECURITY_CTX) || secctx_received {
                    return Err(Error::DecodeMessage(einval()));
                }

                secctx_received = true;
                extensions.secctx = parse_security_context(nr_secctx, current_extension_bytes)?;
            }
            ExtType::SupGroups => {
                // We're not exposing this feature to the guest, so we shouldn't get
                // any messages including this extension.
                unimplemented!("Support for supplemental groups is not implemented");
            }
        }

        // Let's process the next extension
        buf = next_extension_bytes;
    }

    // The SupGroup extension can be missing, since it is only sent if needed.
    // A SecCtx is always sent in create/synlink/mknod/mkdir if supported.
    if options.contains(FsOptions::SECURITY_CTX) && !secctx_received {
        return Err(Error::MissingExtension);
    }

    Ok(extensions)
}

#[cfg(test)]
mod guest_cache_tests {
    use super::super::null_fs::NullFs;
    use super::*;
    use crate::devices::virtio::descriptor_utils::{
        create_descriptor_chain, DescriptorType, OwnedDescriptorChain,
    };
    use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};

    struct LocalFlushFs(FsOptions);
    impl FileSystem for LocalFlushFs {
        type Inode = u64;
        type Handle = u64;
        fn init(&self, _capable: FsOptions) -> io::Result<FsOptions> {
            Ok(self.0)
        }
        fn open(
            &self,
            _ctx: Context,
            _inode: u64,
            _kill_priv: bool,
            _flags: u32,
        ) -> io::Result<(Option<u64>, OpenOptions)> {
            Ok((
                Some(7),
                OpenOptions::PVISOR_LOCAL_FLUSH | OpenOptions::KEEP_CACHE,
            ))
        }
        fn opendir(
            &self,
            ctx: Context,
            inode: u64,
            flags: u32,
        ) -> io::Result<(Option<u64>, OpenOptions)> {
            self.open(ctx, inode, false, flags)
        }
        fn capture_state(&self) -> io::Result<super::super::snapshot::FsSnapshot> {
            NullFs.capture_state()
        }
        fn restore_state(&self, state: &super::super::snapshot::FsSnapshot) -> io::Result<()> {
            NullFs.restore_state(state)
        }
    }

    #[test]
    fn local_flush_capability_and_open_flag_require_both_sides_and_safe_session() {
        for requested in [false, true] {
            for guest_cache in [false, true] {
                for guest in [false, true] {
                    for conflict in [
                        FsOptions::empty(),
                        FsOptions::WRITEBACK_CACHE,
                        FsOptions::POSIX_LOCKS,
                        FsOptions::FLOCK_LOCKS,
                        FsOptions::HAS_INODE_DAX,
                        FsOptions::MAP_ALIGNMENT,
                    ] {
                        let want = conflict
                            | if requested {
                                FsOptions::PVISOR_LOCAL_FLUSH
                            } else {
                                FsOptions::empty()
                            };
                        let capable = conflict
                            | if guest {
                                FsOptions::PVISOR_LOCAL_FLUSH
                            } else {
                                FsOptions::empty()
                            };
                        let server = Server::new(LocalFlushFs(want), guest_cache);
                        let enabled = negotiate(&server, capable);
                        let expected = cfg!(target_os = "linux")
                            && requested
                            && guest_cache
                            && guest
                            && conflict.is_empty();
                        assert_eq!(enabled.contains(FsOptions::PVISOR_LOCAL_FLUSH), expected);
                        for opcode in [Opcode::Open, Opcode::Opendir] {
                            let (header, bytes) = request(
                                &server,
                                opcode,
                                OpenIn {
                                    flags: libc::O_RDONLY as u32,
                                    ..Default::default()
                                }
                                .as_slice(),
                            );
                            assert_eq!(header.error, 0);
                            let mut out = OpenOut::default();
                            out.as_mut_slice().copy_from_slice(&bytes);
                            assert_eq!(
                                out.open_flags & OpenOptions::PVISOR_LOCAL_FLUSH.bits() != 0,
                                expected && matches!(opcode, Opcode::Open)
                            );
                            assert_ne!(out.open_flags & OpenOptions::KEEP_CACHE.bits(), 0);
                            assert_eq!(out.open_flags & (1 << 5), 0); // never standard NOFLUSH
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn local_flush_snapshot_rejection_only_applies_to_negotiated_sessions() {
        let old = Server::new(LocalFlushFs(FsOptions::PVISOR_LOCAL_FLUSH), true);
        assert!(!negotiate(&old, FsOptions::empty()).contains(FsOptions::PVISOR_LOCAL_FLUSH));
        let mut state = old.capture_state(2).unwrap();
        Server::new(NullFs, false).restore_state(&state).unwrap();
        let enabled = Server::new(LocalFlushFs(FsOptions::PVISOR_LOCAL_FLUSH), true);
        let negotiated = negotiate(&enabled, FsOptions::PVISOR_LOCAL_FLUSH);
        if cfg!(target_os = "linux") {
            assert!(negotiated.contains(FsOptions::PVISOR_LOCAL_FLUSH));
            assert_eq!(
                enabled.capture_state(2).unwrap_err().kind(),
                io::ErrorKind::Unsupported
            );
        }
        state.options |= FsOptions::PVISOR_LOCAL_FLUSH.bits();
        assert_eq!(
            Server::new(NullFs, false)
                .restore_state(&state)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }

    struct DataObjectFs(FsOptions, Option<super::super::filesystem::LowerDataObject>);
    impl FileSystem for DataObjectFs {
        type Inode = u64;
        type Handle = u64;
        fn init(&self, _: FsOptions) -> io::Result<FsOptions> {
            Ok(self.0)
        }
        fn open(
            &self,
            _: Context,
            _: u64,
            _: bool,
            _: u32,
        ) -> io::Result<(Option<u64>, OpenOptions)> {
            Ok((
                Some(7),
                OpenOptions::PVISOR_DATA_OBJECT | OpenOptions::KEEP_CACHE,
            ))
        }
        fn lower_data_object(
            &self,
            inode: u64,
            handle: u64,
        ) -> Option<super::super::filesystem::LowerDataObject> {
            assert_eq!((inode, handle), (ROOT_ID, 7));
            self.1
        }
        fn opendir(
            &self,
            ctx: Context,
            inode: u64,
            flags: u32,
        ) -> io::Result<(Option<u64>, OpenOptions)> {
            self.open(ctx, inode, false, flags)
        }
        fn create(
            &self,
            _: Context,
            _: u64,
            _: &CStr,
            _: u32,
            _: bool,
            _: u32,
            _: u32,
            _: Extensions,
        ) -> io::Result<(Entry, Option<u64>, OpenOptions)> {
            Ok((
                Entry {
                    inode: 2,
                    generation: 0,
                    attr: unsafe { std::mem::zeroed() },
                    attr_flags: 0,
                    attr_timeout: Default::default(),
                    entry_timeout: Default::default(),
                },
                Some(7),
                OpenOptions::PVISOR_DATA_OBJECT,
            ))
        }
        fn capture_state(&self) -> io::Result<super::super::snapshot::FsSnapshot> {
            NullFs.capture_state()
        }
        fn restore_state(&self, state: &super::super::snapshot::FsSnapshot) -> io::Result<()> {
            NullFs.restore_state(state)
        }
    }

    #[test]
    fn data_object_open_wire_requires_negotiation_and_qualified_receipt() {
        use super::super::filesystem::LowerDataObject;
        for requested in [false, true] {
            for guest_cache in [false, true] {
                for guest in [false, true] {
                    for conflict in [
                        FsOptions::empty(),
                        FsOptions::WRITEBACK_CACHE,
                        FsOptions::POSIX_LOCKS,
                        FsOptions::FLOCK_LOCKS,
                        FsOptions::HAS_INODE_DAX,
                        FsOptions::MAP_ALIGNMENT,
                    ] {
                        let want = conflict
                            | if requested {
                                FsOptions::PVISOR_DATA_OBJECT
                            } else {
                                FsOptions::empty()
                            };
                        let caps = conflict
                            | if guest {
                                FsOptions::PVISOR_DATA_OBJECT
                            } else {
                                FsOptions::empty()
                            };
                        for receipt in [
                            None,
                            Some(LowerDataObject { id: 0, size: 99 }),
                            Some(LowerDataObject {
                                id: 23,
                                size: 0x1_0000_0005,
                            }),
                        ] {
                            let server = Server::new(DataObjectFs(want, receipt), guest_cache);
                            let enabled = negotiate(&server, caps);
                            let negotiated = cfg!(target_os = "linux")
                                && requested
                                && guest_cache
                                && guest
                                && conflict.is_empty();
                            assert_eq!(enabled.contains(FsOptions::PVISOR_DATA_OBJECT), negotiated);
                            let (header, bytes) = request(
                                &server,
                                Opcode::Open,
                                OpenIn {
                                    flags: libc::O_RDONLY as u32,
                                    ..Default::default()
                                }
                                .as_slice(),
                            );
                            assert_eq!(header.error, 0);
                            assert_eq!(bytes.len(), if negotiated { 24 } else { 16 });
                            let mut out = OpenOut::default();
                            out.as_mut_slice().copy_from_slice(&bytes[..16]);
                            let qualified = negotiated && receipt.is_some_and(|r| r.id != 0);
                            assert_eq!(
                                out.open_flags & OpenOptions::PVISOR_DATA_OBJECT.bits() != 0,
                                qualified
                            );
                            assert_eq!(out.backing_id, if qualified { 23 } else { 0 });
                            assert_eq!(out.fh, 7);
                            assert_ne!(out.open_flags & OpenOptions::KEEP_CACHE.bits(), 0);
                            if negotiated {
                                assert_eq!(
                                    u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
                                    if qualified { 0x1_0000_0005 } else { 0 }
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn data_object_flag_is_stripped_from_writers_directories_and_create() {
        use super::super::filesystem::LowerDataObject;
        let server = Server::new(
            DataObjectFs(
                FsOptions::PVISOR_DATA_OBJECT,
                Some(LowerDataObject { id: 1, size: 5 }),
            ),
            true,
        );
        let enabled = negotiate(&server, FsOptions::PVISOR_DATA_OBJECT);
        for (flags, open_flags) in [
            (libc::O_RDWR as u32, 0),
            ((libc::O_RDONLY | libc::O_TRUNC) as u32, 0),
            (libc::O_RDONLY as u32, OPEN_KILL_SUIDGID),
        ] {
            let (_, bytes) = request(
                &server,
                Opcode::Open,
                OpenIn { flags, open_flags }.as_slice(),
            );
            let mut out = OpenOut::default();
            out.as_mut_slice().copy_from_slice(&bytes[..16]);
            assert_eq!(out.backing_id, 0);
            assert_eq!(out.open_flags & OpenOptions::PVISOR_DATA_OBJECT.bits(), 0);
            assert_eq!(
                bytes.len(),
                if enabled.contains(FsOptions::PVISOR_DATA_OBJECT) {
                    24
                } else {
                    16
                }
            );
        }
        #[cfg(target_os = "linux")]
        {
            let (_, bytes) = request(
                &server,
                Opcode::Open,
                OpenIn {
                    flags: (libc::O_RDONLY | libc::O_DIRECT) as u32,
                    ..Default::default()
                }
                .as_slice(),
            );
            let mut out = OpenOut::default();
            out.as_mut_slice().copy_from_slice(&bytes[..16]);
            assert_eq!(out.backing_id, 0);
            assert_eq!(out.open_flags & OpenOptions::PVISOR_DATA_OBJECT.bits(), 0);
        }
        let (_, bytes) = request(&server, Opcode::Opendir, OpenIn::default().as_slice());
        assert_eq!(bytes.len(), 16);
        let mut out = OpenOut::default();
        out.as_mut_slice().copy_from_slice(&bytes);
        assert_eq!(out.backing_id, 0);
        assert_eq!(out.open_flags & OpenOptions::PVISOR_DATA_OBJECT.bits(), 0);
        let mut body = CreateIn::default().as_slice().to_vec();
        body.extend_from_slice(b"new-file\0");
        let (header, bytes) = request(&server, Opcode::Create, &body);
        assert_eq!(header.error, 0);
        assert_eq!(bytes.len(), size_of::<EntryOut>() + 16);
        out.as_mut_slice()
            .copy_from_slice(&bytes[size_of::<EntryOut>()..]);
        assert_eq!(out.backing_id, 0);
        assert_eq!(out.open_flags & OpenOptions::PVISOR_DATA_OBJECT.bits(), 0);
    }

    #[test]
    fn data_object_open_reply_keeps_request_and_returned_option_filters() {
        struct ReturnedOptionsFs(Option<u64>, OpenOptions, std::sync::atomic::AtomicU64);
        impl FileSystem for ReturnedOptionsFs {
            type Inode = u64;
            type Handle = u64;
            fn init(&self, _: FsOptions) -> io::Result<FsOptions> {
                Ok(FsOptions::PVISOR_DATA_OBJECT)
            }
            fn open(
                &self,
                _: Context,
                _: u64,
                _: bool,
                _: u32,
            ) -> io::Result<(Option<u64>, OpenOptions)> {
                Ok((self.0, self.1))
            }
            fn lower_data_object(
                &self,
                _: u64,
                _: u64,
            ) -> Option<super::super::filesystem::LowerDataObject> {
                self.2.fetch_add(1, Ordering::Relaxed);
                Some(super::super::filesystem::LowerDataObject { id: 31, size: 17 })
            }
        }
        let data = OpenOptions::PVISOR_DATA_OBJECT;
        // Bit 7 is intentionally not negotiated/declared as supported native
        // passthrough. An adapter returning it must never receive a DATA ID.
        let passthrough = unsafe { OpenOptions::from_bits_unchecked(data.bits() | (1 << 7)) };
        for (handle, options, flags, expect) in [
            (Some(7), data, libc::O_RDONLY, true),
            (None, data, libc::O_RDONLY, false),
            (Some(0), data, libc::O_RDONLY, false),
            (Some(7), OpenOptions::empty(), libc::O_RDONLY, false),
            (
                Some(7),
                data | OpenOptions::DIRECT_IO,
                libc::O_RDONLY,
                false,
            ),
            (Some(7), passthrough, libc::O_RDONLY, false),
            (Some(7), data, libc::O_RDONLY | libc::O_APPEND, false),
        ] {
            let server = Server::new(ReturnedOptionsFs(handle, options, Default::default()), true);
            let negotiated = negotiate(&server, FsOptions::PVISOR_DATA_OBJECT)
                .contains(FsOptions::PVISOR_DATA_OBJECT);
            let (header, bytes) = request(
                &server,
                Opcode::Open,
                OpenIn {
                    flags: flags as u32,
                    ..Default::default()
                }
                .as_slice(),
            );
            assert_eq!(header.error, 0);
            let mut out = OpenOut::default();
            out.as_mut_slice().copy_from_slice(&bytes[..16]);
            assert_eq!(out.backing_id != 0, expect && negotiated);
            assert_eq!(
                server.fs.2.load(Ordering::Relaxed),
                u64::from(expect && negotiated)
            );
        }
        #[cfg(target_os = "linux")]
        for flags in [libc::O_DIRECT, libc::O_PATH] {
            let server = Server::new(ReturnedOptionsFs(Some(7), data, Default::default()), true);
            negotiate(&server, FsOptions::PVISOR_DATA_OBJECT);
            let (header, bytes) = request(
                &server,
                Opcode::Open,
                OpenIn {
                    flags: flags as u32,
                    ..Default::default()
                }
                .as_slice(),
            );
            assert_eq!(header.error, 0);
            let mut out = OpenOut::default();
            out.as_mut_slice().copy_from_slice(&bytes[..16]);
            assert_eq!(out.backing_id, 0);
            assert_eq!(server.fs.2.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn data_object_snapshots_only_reject_the_negotiated_contract() {
        let old = Server::new(DataObjectFs(FsOptions::PVISOR_DATA_OBJECT, None), true);
        assert!(!negotiate(&old, FsOptions::empty()).contains(FsOptions::PVISOR_DATA_OBJECT));
        let mut state = old.capture_state(2).unwrap();
        Server::new(NullFs, false).restore_state(&state).unwrap();
        let enabled = Server::new(DataObjectFs(FsOptions::PVISOR_DATA_OBJECT, None), true);
        if negotiate(&enabled, FsOptions::PVISOR_DATA_OBJECT)
            .contains(FsOptions::PVISOR_DATA_OBJECT)
        {
            assert_eq!(
                enabled.capture_state(2).unwrap_err().kind(),
                io::ErrorKind::Unsupported
            );
        }
        state.options |= FsOptions::PVISOR_DATA_OBJECT.bits();
        assert_eq!(
            Server::new(NullFs, false)
                .restore_state(&state)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }

    fn request<F: FileSystem + Sync>(
        server: &Server<F>,
        opcode: Opcode,
        body: &[u8],
    ) -> (OutHeader, Vec<u8>) {
        try_request(server, opcode, body, body.len()).unwrap()
    }

    fn try_request<F: FileSystem + Sync>(
        server: &Server<F>,
        opcode: Opcode,
        body: &[u8],
        declared_body_len: usize,
    ) -> Result<(OutHeader, Vec<u8>)> {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let chain = create_descriptor_chain(
            &mem,
            GuestAddress(0),
            GuestAddress(0x100),
            vec![
                (DescriptorType::Readable, body.len() as u32),
                (DescriptorType::Writable, 256),
            ],
            0,
        )
        .unwrap();
        mem.write_slice(body, GuestAddress(0x100)).unwrap();
        let plan = OwnedDescriptorChain::new(chain).unwrap();
        let (reader, writer) = plan.reader_writer(&mem).unwrap();
        let count = server.handle_message(
            (
                InHeader {
                    len: (size_of::<InHeader>() + declared_body_len) as u32,
                    opcode: opcode as u32,
                    nodeid: ROOT_ID,
                    unique: 1,
                    ..Default::default()
                },
                reader,
            ),
            writer,
            false,
            &None,
            &Arc::new(AtomicI32::new(0)),
            #[cfg(target_os = "macos")]
            &None,
        )?;
        let address = 0x100 + body.len() as u64;
        let header: OutHeader = mem.read_obj(GuestAddress(address)).unwrap();
        assert_eq!(header.len as usize, count);
        let mut bytes = vec![0; count - size_of::<OutHeader>()];
        mem.read_slice(
            &mut bytes,
            GuestAddress(address + size_of::<OutHeader>() as u64),
        )
        .unwrap();
        Ok((header, bytes))
    }

    struct SyncfsFs {
        calls: std::sync::atomic::AtomicUsize,
        error: Option<i32>,
    }

    impl FileSystem for SyncfsFs {
        type Inode = u64;
        type Handle = u64;

        fn syncfs(&self, ctx: Context) -> io::Result<()> {
            assert_eq!((ctx.uid, ctx.gid, ctx.pid), (0, 0, 0));
            self.calls.fetch_add(1, Ordering::Relaxed);
            match self.error {
                Some(error) => Err(io::Error::from_raw_os_error(error)),
                None => Ok(()),
            }
        }
    }

    #[test]
    fn syncfs_dispatches_connection_barrier_and_propagates_backing_error() {
        assert_eq!(size_of::<SyncfsIn>(), 8);
        // The padding is reserved, not an inode, handle, or synchronization flag.
        let body = SyncfsIn { padding: u64::MAX };
        for error in [None, Some(libc::EIO), Some(libc::ENOSPC)] {
            let server = Server::new(
                SyncfsFs {
                    calls: Default::default(),
                    error,
                },
                false,
            );
            let (header, bytes) = request(&server, Opcode::Syncfs, body.as_slice());
            assert_eq!(header.unique, 1);
            assert_eq!(header.error, -error.unwrap_or(0));
            assert!(bytes.is_empty());
            assert_eq!(server.fs.calls.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn syncfs_keeps_unsupported_adapters_on_enosys() {
        let (header, bytes) = request(
            &Server::new(NullFs, false),
            Opcode::Syncfs,
            SyncfsIn::default().as_slice(),
        );
        assert_eq!(header.error, -bindings::LINUX_ENOSYS);
        assert!(bytes.is_empty());
    }

    #[test]
    fn syncfs_rejects_short_header_and_truncated_descriptors_before_dispatch() {
        let server = Server::new(
            SyncfsFs {
                calls: Default::default(),
                error: None,
            },
            false,
        );
        let body = SyncfsIn::default();
        for length in 0..size_of::<SyncfsIn>() {
            // Even a complete descriptor cannot fill a body excluded by len.
            assert!(matches!(
                try_request(&server, Opcode::Syncfs, body.as_slice(), length),
                Err(Error::InvalidHeaderLength)
            ));
            // Conversely, a claimed complete frame cannot extend the descriptor.
            assert!(matches!(
                try_request(
                    &server,
                    Opcode::Syncfs,
                    &body.as_slice()[..length],
                    size_of::<SyncfsIn>()
                ),
                Err(Error::DecodeMessage(_))
            ));
        }
        assert_eq!(server.fs.calls.load(Ordering::Relaxed), 0);
    }

    fn negotiate<F: FileSystem + Sync>(server: &Server<F>, capable: FsOptions) -> FsOptions {
        let capable = capable | FsOptions::INIT_EXT | FsOptions::MAX_PAGES;
        let mut body = InitInCompat {
            major: KERNEL_VERSION,
            minor: KERNEL_MINOR_VERSION,
            flags: capable.bits() as u32,
            ..Default::default()
        }
        .as_slice()
        .to_vec();
        body.extend_from_slice(
            InitInExt {
                flags2: (capable.bits() >> 32) as u32,
                ..Default::default()
            }
            .as_slice(),
        );
        let (header, bytes) = request(server, Opcode::Init, &body);
        assert_eq!(header.error, 0);
        let mut out = InitOut::default();
        out.as_mut_slice().copy_from_slice(&bytes);
        assert_eq!(out.max_pages, NEGOTIATED_MAX_PAGES);
        assert_eq!(out.max_write, MAX_BUFFER_SIZE);
        let enabled =
            FsOptions::from_bits((u64::from(out.flags2) << 32) | u64::from(out.flags)).unwrap();
        assert_eq!(
            out.max_background,
            if enabled.contains(FsOptions::PVISOR_WRITEBACK) {
                64
            } else {
                u16::MAX
            }
        );
        enabled
    }

    #[test]
    fn phase_ioctl_requires_profiling_and_exact_eight_byte_input() {
        use pvisor_overlay_core::profile::Profile;

        let token = 17_u64;
        let input = IoctlIn {
            cmd: PROFILE_PHASE_IOCTL,
            in_size: 8,
            ..Default::default()
        };
        let mut body = input.as_slice().to_vec();
        body.extend_from_slice(&token.to_le_bytes());
        let mut server = Server::new(NullFs, false);
        server.profile = Profile::default();
        server.configure_profile_scope("/dev/root");
        assert_eq!(
            request(&server, Opcode::Ioctl, &body).0.error,
            -libc::ENOTTY
        );

        server.profile = Profile::enabled("virtio-fs-protocol");
        server.dispatch_profile = Profile::enabled("virtio-fs-dispatch");
        server.configure_profile_scope("/dev/root");
        let record = server.phase_profiles(token).unwrap();
        assert_eq!(record["scope"], "rootfs");
        assert_eq!(record["token"], token);
        assert_eq!(record["profiles"].as_array().unwrap().len(), 2);
        assert_eq!(
            server.dispatch_profile().report().unwrap().instance,
            server.dispatch_profile.report().unwrap().instance
        );
        assert_eq!(request(&server, Opcode::Ioctl, &body).0.error, 0);

        for invalid in [
            IoctlIn {
                in_size: 7,
                ..input
            },
            IoctlIn {
                out_size: 8,
                ..input
            },
        ] {
            let mut malformed = invalid.as_slice().to_vec();
            malformed.extend_from_slice(&token.to_le_bytes());
            assert_eq!(
                request(&server, Opcode::Ioctl, &malformed).0.error,
                -libc::EINVAL
            );
        }
    }

    #[test]
    fn switch_controls_negotiation_and_disabled_readindex() {
        for enabled in [false, true] {
            for capable in [FsOptions::empty(), FsOptions::PVISOR_META] {
                let server = Server::new(NullFs, enabled);
                let options = negotiate(&server, capable);
                assert_eq!(
                    options.contains(FsOptions::PVISOR_META),
                    enabled && capable.contains(FsOptions::PVISOR_META)
                );
                if !options.contains(FsOptions::PVISOR_META) {
                    let (header, _) = request(
                        &server,
                        Opcode::ReadIndex,
                        ReadIn {
                            size: MAX_BUFFER_SIZE,
                            ..Default::default()
                        }
                        .as_slice(),
                    );
                    assert_eq!(header.error, -bindings::LINUX_ENOSYS);
                }
            }
        }
    }

    #[test]
    fn background_short_writes_fail_instead_of_acknowledging_lost_bytes() {
        struct ShortFs(usize);
        impl FileSystem for ShortFs {
            type Inode = u64;
            type Handle = u64;
            fn write<R: io::Read + ZeroCopyReader>(
                &self,
                _ctx: Context,
                _inode: u64,
                _handle: u64,
                _reader: R,
                _size: u32,
                _offset: u64,
                _owner: Option<u64>,
                _delayed: bool,
                _kill_priv: bool,
                _flags: u32,
            ) -> io::Result<usize> {
                Ok(self.0)
            }
        }
        for delayed in [false, true] {
            for count in [0, 2, 4, 5] {
                let mut body = WriteIn {
                    size: 4,
                    write_flags: if delayed { WRITE_CACHE } else { 0 },
                    ..Default::default()
                }
                .as_slice()
                .to_vec();
                body.extend_from_slice(b"data");
                let (header, bytes) =
                    request(&Server::new(ShortFs(count), false), Opcode::Write, &body);
                if count > 4 || (delayed && count != 4) {
                    assert_eq!(header.error, -libc::EIO);
                } else {
                    assert_eq!(header.error, 0);
                    let mut out = WriteOut::default();
                    out.as_mut_slice().copy_from_slice(&bytes);
                    assert_eq!(out.size as usize, count);
                }
            }
        }
    }

    #[test]
    fn batched_writeback_requires_both_guest_and_filesystem_and_bounds_background() {
        struct WritebackFs(bool);
        impl FileSystem for WritebackFs {
            type Inode = u64;
            type Handle = u64;
            fn init(&self, _capable: FsOptions) -> io::Result<FsOptions> {
                Ok(FsOptions::PVISOR_WRITEBACK
                    | if self.0 {
                        FsOptions::WRITEBACK_CACHE
                    } else {
                        FsOptions::empty()
                    })
            }
        }
        for filesystem in [false, true] {
            for guest_cache in [false, true] {
                for capable in [
                    FsOptions::empty(),
                    FsOptions::WRITEBACK_CACHE,
                    FsOptions::PVISOR_WRITEBACK,
                    FsOptions::PVISOR_WRITEBACK | FsOptions::WRITEBACK_CACHE,
                ] {
                    let options =
                        negotiate(&Server::new(WritebackFs(filesystem), guest_cache), capable);
                    assert_eq!(
                        options.contains(FsOptions::PVISOR_WRITEBACK),
                        filesystem
                            && capable
                                .contains(FsOptions::PVISOR_WRITEBACK | FsOptions::WRITEBACK_CACHE)
                    );
                }
            }
        }
    }

    struct DirectoryReplyFs {
        fail_after_first: bool,
        second_entry: bool,
        refs: std::sync::Mutex<std::collections::HashMap<u64, u64>>,
        forgotten: std::sync::Mutex<Vec<u64>>,
    }

    impl DirectoryReplyFs {
        fn new(fail_after_first: bool, second_entry: bool) -> Self {
            Self {
                fail_after_first,
                second_entry,
                refs: Default::default(),
                forgotten: Default::default(),
            }
        }
    }

    impl FileSystem for DirectoryReplyFs {
        type Inode = u64;
        type Handle = u64;

        fn forget(&self, _ctx: Context, inode: u64, count: u64) {
            let mut refs = self.refs.lock().unwrap();
            let current = refs.get_mut(&inode).expect("forget of an unowned entry");
            *current = current.checked_sub(count).expect("entry forgotten twice");
            self.forgotten.lock().unwrap().push(inode);
        }

        fn readdirplus<F>(
            &self,
            ctx: Context,
            _inode: u64,
            _handle: u64,
            _size: u32,
            _offset: u64,
            mut add_entry: F,
        ) -> io::Result<()>
        where
            F: FnMut(DirEntry, Entry) -> io::Result<usize>,
        {
            for inode in if self.second_entry {
                &[11, 22][..]
            } else {
                &[11][..]
            } {
                *self.refs.lock().unwrap().entry(*inode).or_default() += 1;
                // SAFETY: stat64 is a scalar POD; only the serialized fixture
                // attributes are used, with no pointers or ownership.
                let attr = unsafe { std::mem::zeroed() };
                let added = add_entry(
                    DirEntry {
                        ino: *inode,
                        offset: *inode,
                        type_: 8,
                        name: b"x",
                    },
                    Entry {
                        inode: *inode,
                        generation: 0,
                        attr,
                        attr_flags: 0,
                        attr_timeout: std::time::Duration::ZERO,
                        entry_timeout: std::time::Duration::ZERO,
                    },
                );
                match added {
                    Ok(0) => {
                        self.forget(ctx, *inode, 1);
                        return Ok(());
                    }
                    Err(error) => {
                        self.forget(ctx, *inode, 1);
                        return Err(error);
                    }
                    Ok(_) => {}
                }
                if self.fail_after_first {
                    // The next entry's policy/metadata query fails before its
                    // callback. The entire request will return an error.
                    return Err(io::Error::from_raw_os_error(libc::EACCES));
                }
            }
            Ok(())
        }

        fn readdir<F>(
            &self,
            _ctx: Context,
            _inode: u64,
            _handle: u64,
            _size: u32,
            _offset: u64,
            mut add_entry: F,
        ) -> io::Result<()>
        where
            F: FnMut(DirEntry) -> io::Result<usize>,
        {
            add_entry(DirEntry {
                ino: 11,
                offset: 1,
                type_: 8,
                name: b"x",
            })?;
            Err(io::Error::from_raw_os_error(libc::EACCES))
        }
    }

    fn one_plus_entry_bytes() -> u32 {
        (size_of::<EntryOut>() + ((size_of::<Dirent>() + 1 + 7) & !7)) as u32
    }

    #[test]
    fn readdirplus_rpc_error_releases_previously_accepted_entries() {
        let server = Server::new(DirectoryReplyFs::new(true, false), true);
        let (header, bytes) = request(
            &server,
            Opcode::Readdirplus,
            ReadIn {
                fh: 1,
                size: one_plus_entry_bytes(),
                ..Default::default()
            }
            .as_slice(),
        );
        assert_eq!(header.error, -libc::EACCES);
        assert!(bytes.is_empty());
        assert_eq!(server.fs.refs.lock().unwrap().get(&11), Some(&0));
        assert_eq!(*server.fs.forgotten.lock().unwrap(), vec![11]);
    }

    #[test]
    fn readdirplus_full_buffer_keeps_accepted_reference_and_cancels_current_once() {
        let server = Server::new(DirectoryReplyFs::new(false, true), true);
        let size = one_plus_entry_bytes();
        let (header, bytes) = request(
            &server,
            Opcode::Readdirplus,
            ReadIn {
                fh: 1,
                size,
                ..Default::default()
            }
            .as_slice(),
        );
        assert_eq!(header.error, 0);
        assert_eq!(bytes.len(), size as usize);
        assert_eq!(server.fs.refs.lock().unwrap().get(&11), Some(&1));
        assert_eq!(server.fs.refs.lock().unwrap().get(&22), Some(&0));
        assert_eq!(*server.fs.forgotten.lock().unwrap(), vec![22]);
    }

    #[test]
    fn uncommitted_directory_reply_releases_each_repeated_lookup() {
        let fs = DirectoryReplyFs::new(false, false);
        fs.refs.lock().unwrap().insert(11, 2);
        {
            let _reply = DirectoryReplyLookups {
                fs: &fs,
                ctx: Context {
                    uid: 0,
                    gid: 0,
                    pid: 0,
                },
                accepted: vec![11, 11],
            };
            // Drop is also the reply-header encoding-error path: no commit.
        }
        assert_eq!(fs.refs.lock().unwrap().get(&11), Some(&0));
        assert_eq!(*fs.forgotten.lock().unwrap(), vec![11, 11]);
    }

    #[test]
    fn readdir_error_does_not_create_lookup_rollback() {
        let server = Server::new(DirectoryReplyFs::new(true, false), true);
        let (header, _) = request(
            &server,
            Opcode::Readdir,
            ReadIn {
                fh: 1,
                size: 64,
                ..Default::default()
            }
            .as_slice(),
        );
        assert_eq!(header.error, -libc::EACCES);
        assert!(server.fs.forgotten.lock().unwrap().is_empty());
        assert!(server.fs.refs.lock().unwrap().is_empty());
    }

    #[test]
    fn restore_keeps_negotiated_mode_until_new_init() {
        for enabled in [false, true] {
            let server = Server::new(NullFs, enabled);
            let options = negotiate(&server, FsOptions::PVISOR_META);
            let state = server.capture_state(2).unwrap();
            let restored = Server::new(NullFs, !enabled);
            restored.restore_state(&state).unwrap();
            assert_eq!(restored.capture_state(2).unwrap().options, options.bits());
            restored.destroy().unwrap();
            assert_eq!(
                negotiate(&restored, FsOptions::PVISOR_META).contains(FsOptions::PVISOR_META),
                !enabled
            );
        }
    }
}
