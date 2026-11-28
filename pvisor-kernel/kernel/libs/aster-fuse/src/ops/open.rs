// SPDX-License-Identifier: MPL-2.0

//! `FUSE_OPEN` opens a non-directory inode, and `FUSE_OPENDIR` opens a
//! directory inode.
//!
//! Both request bodies contain [`OpenReq`] with the requested open flags, and
//! both operations return an [`OpenReply`] prefix. Negotiated pVisor OPENs
//! append the immutable object's fixed size; OPENDIR and CREATE never do.

use bitflags::bitflags;
use ostd::mm::{Infallible, VmReader, VmWriter};

use super::util::read_payload;
use crate::{FuseError, FuseFileHandle, FuseOpcode, FuseOperation, FuseResult, ReplyExpectation};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod)]
pub struct OpenReq {
    /// POSIX open flags.
    flags: u32,
    /// FUSE-specific open flags.
    open_flags: FuseOpenFlags,
}

impl OpenReq {
    pub const fn new(flags: u32) -> Self {
        Self {
            flags,
            open_flags: FuseOpenFlags::empty(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod)]
pub struct OpenReply {
    /// File handle assigned by the server.
    fh: FuseFileHandle,
    /// FUSE-specific open result flags.
    open_flags: FuseOpenFlags,
    backing_id: u32,
}

impl OpenReply {
    /// Returns the file handle assigned by the server.
    pub fn fh(&self) -> FuseFileHandle {
        self.fh
    }

    /// Returns the FUSE-specific open result flags.
    pub fn open_flags(&self) -> FuseOpenFlags {
        self.open_flags
    }

    /// A receipt identifies immutable bytes in this connection, not a name
    /// or an authorization. Validate it after taking ownership of `fh`.
    pub fn data_object(&self, fixed_size: Option<u64>) -> FuseResult<Option<(u32, usize)>> {
        if !self.open_flags.contains(FuseOpenFlags::PVISOR_DATA_OBJECT) {
            return if fixed_size.is_some() && (self.backing_id != 0 || fixed_size != Some(0)) {
                Err(FuseError::MalformedResponse)
            } else {
                Ok(None)
            };
        }
        let size = fixed_size.ok_or(FuseError::MalformedResponse)?;
        if size > isize::MAX as u64
            || self.backing_id == 0
            || self.fh == FuseFileHandle::new(0)
            || self.open_flags.intersects(
                FuseOpenFlags::FOPEN_DIRECT_IO
                    | FuseOpenFlags::FOPEN_PASSTHROUGH
                    | FuseOpenFlags::FOPEN_NONSEEKABLE
                    | FuseOpenFlags::FOPEN_STREAM,
            )
        {
            return Err(FuseError::MalformedResponse);
        }
        Ok(Some((self.backing_id, size as usize)))
    }
}

bitflags! {
    /// FUSE-specific flags returned by open replies.
    ///
    /// Reference: <https://elixir.bootlin.com/linux/v6.18/source/include/uapi/linux/fuse.h#L387-L394>
    #[repr(C)]
    #[derive(Pod)]
    pub struct FuseOpenFlags: u32 {
        /// Bypasses the page cache for this open file.
        const FOPEN_DIRECT_IO = 1 << 0;
        /// Keeps cached file data valid when this file is opened.
        const FOPEN_KEEP_CACHE = 1 << 1;
        /// Marks this open file as non-seekable.
        const FOPEN_NONSEEKABLE = 1 << 2;
        /// Allows caching directory entries for this open directory.
        const FOPEN_CACHE_DIR = 1 << 3;
        /// Marks this open file as stream-like, with no file position.
        const FOPEN_STREAM = 1 << 4;
        /// Skips flushing cached data on close unless writeback caching is enabled.
        const FOPEN_NOFLUSH = 1 << 5;
        /// Allows concurrent direct writes on the same inode.
        const FOPEN_PARALLEL_DIRECT_WRITES = 1 << 6;
        /// Enables passthrough read and write I/O for this open file.
        const FOPEN_PASSTHROUGH = 1 << 7;
        /// `backing_id` and the extended reply size identify immutable lower bytes.
        const PVISOR_DATA_OBJECT = 1 << 28;
    }
}

pub struct OpenOperation {
    open_req: OpenReq,
    extended: bool,
}

impl OpenOperation {
    pub fn new(open_req: OpenReq, extended: bool) -> Self {
        Self { open_req, extended }
    }
}

impl FuseOperation for OpenOperation {
    type Output = (OpenReply, Option<u64>);

    fn opcode(&self) -> FuseOpcode {
        FuseOpcode::Open
    }

    fn body_len(&self) -> usize {
        size_of::<OpenReq>()
    }

    fn write_body(&mut self, writer: &mut VmWriter<'_, Infallible>) -> FuseResult<()> {
        writer
            .write_val(&self.open_req)
            .map_err(|_| FuseError::BufferTooSmall)
    }

    fn reply_expectation(&self) -> ReplyExpectation {
        ReplyExpectation::payload(size_of::<OpenReply>() + if self.extended { 8 } else { 0 })
    }

    fn parse_reply(
        payload_len: usize,
        reader: &mut VmReader<'_, Infallible>,
    ) -> FuseResult<Self::Output> {
        if reader.remain() < payload_len {
            return Err(FuseError::BufferTooSmall);
        }
        let extended = match payload_len {
            16 => false,
            24 => true,
            _ => return Err(FuseError::MalformedResponse),
        };
        let reply = read_payload(payload_len, reader)?;
        let size = if extended {
            Some(read_payload(8, reader)?)
        } else {
            None
        };
        Ok((reply, size))
    }
}

pub struct OpendirOperation {
    open_req: OpenReq,
}

impl OpendirOperation {
    pub fn new(open_req: OpenReq) -> Self {
        Self { open_req }
    }
}

impl FuseOperation for OpendirOperation {
    type Output = OpenReply;

    fn opcode(&self) -> FuseOpcode {
        FuseOpcode::Opendir
    }

    fn body_len(&self) -> usize {
        size_of::<OpenReq>()
    }

    fn write_body(&mut self, writer: &mut VmWriter<'_, Infallible>) -> FuseResult<()> {
        writer.write_val(&self.open_req).unwrap();

        Ok(())
    }

    fn reply_expectation(&self) -> ReplyExpectation {
        ReplyExpectation::payload(size_of::<OpenReply>())
    }

    fn parse_reply(
        payload_len: usize,
        reader: &mut VmReader<'_, Infallible>,
    ) -> FuseResult<Self::Output> {
        read_payload(payload_len, reader)
    }
}

#[cfg(ktest)]
mod tests {
    use ostd::prelude::ktest;

    use super::*;

    fn wire(flags: u32, id: u32, size: u64) -> [u8; 24] {
        let mut bytes = [0; 24];
        bytes[0..8].copy_from_slice(&19u64.to_le_bytes());
        bytes[8..12].copy_from_slice(&flags.to_le_bytes());
        bytes[12..16].copy_from_slice(&id.to_le_bytes());
        bytes[16..24].copy_from_slice(&size.to_le_bytes());
        bytes
    }

    #[ktest]
    fn immutable_open_wire() {
        let flag = FuseOpenFlags::PVISOR_DATA_OBJECT.bits();
        for (flags, id, size, valid) in [
            (0, 0, 0, true),
            (flag, 1, 0, true),
            (flag, 7, 4097, true),
            (0, 1, 0, false),
            (0, 0, 9, false),
            (flag, 0, 0, false),
            (flag | FuseOpenFlags::FOPEN_DIRECT_IO.bits(), 7, 4, false),
            (flag, 1, u64::MAX, false),
        ] {
            let bytes = wire(flags, id, size);
            let (open, end) =
                OpenOperation::parse_reply(24, &mut VmReader::from(&bytes[..])).unwrap();
            let object = open.data_object(end);
            assert_eq!(object.is_ok(), valid);
            if valid {
                assert_eq!(object.unwrap(), (id != 0).then_some((id, size as usize)));
            }
        }
        let bytes = wire(flag, 7, 5);
        let (open, end) =
            OpenOperation::parse_reply(16, &mut VmReader::from(&bytes[..16])).unwrap();
        assert!(open.data_object(end).is_err());
        for len in [0, 15, 17, 23, 25] {
            assert!(OpenOperation::parse_reply(len, &mut VmReader::from(&bytes[..])).is_err());
        }
        assert!(OpenOperation::parse_reply(24, &mut VmReader::from(&bytes[..16])).is_err());
    }
}
