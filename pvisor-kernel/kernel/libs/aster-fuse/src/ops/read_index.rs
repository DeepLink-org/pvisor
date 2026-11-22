// SPDX-License-Identifier: MPL-2.0

//! pVisor directory images: a page header followed by aligned `direntplus` records.

use alloc::{string::String, vec::Vec};

use ostd::mm::{Infallible, VmReader, VmWriter};

use super::util::read_bytes;
use crate::{
    Dirent, EntryReply, FuseError, FuseFileHandle, FuseOpcode, FuseOperation, FuseResult, ReadReq,
    ReplyExpectation,
};

const COMPLETE: u32 = 1;
const MAX_NAME_LEN: usize = 1024;

#[repr(C)]
#[derive(Clone, Copy, Pod)]
struct Header {
    next_offset: u64,
    flags: u32,
    padding: u32,
}

pub struct ReadIndexPage {
    pub next_offset: u64,
    pub complete: bool,
    /// False if the page is malformed. Even then, release lookups from decoded records.
    pub valid: bool,
    pub records: Vec<ReadIndexRecord>,
}

pub struct ReadIndexRecord {
    /// Missing for a name which cannot be looked up through this VFS.
    pub name: Option<String>,
    /// Nonzero node IDs with nonzero entry validity carry a backing lookup.
    pub entry: EntryReply,
}

pub struct ReadIndexOperation(ReadReq);

impl ReadIndexOperation {
    pub fn new(offset: u64, size: u32) -> Self {
        Self(ReadReq::new(FuseFileHandle::new(0), offset, size, 0))
    }
}

impl FuseOperation for ReadIndexOperation {
    type Output = ReadIndexPage;

    fn opcode(&self) -> FuseOpcode {
        FuseOpcode::ReadIndex
    }

    fn body_len(&self) -> usize {
        size_of::<ReadReq>()
    }

    fn write_body(&mut self, writer: &mut VmWriter<'_, Infallible>) -> FuseResult<()> {
        writer
            .write_val(&self.0)
            .map_err(|_| FuseError::BufferTooSmall)
    }

    fn reply_expectation(&self) -> ReplyExpectation {
        ReplyExpectation::variable_payload(self.0.size() as usize)
    }

    fn parse_reply(
        payload_len: usize,
        reader: &mut VmReader<'_, Infallible>,
    ) -> FuseResult<Self::Output> {
        reader.limit(payload_len);
        let header = reader
            .read_val::<Header>()
            .map_err(|_| FuseError::MalformedResponse)?;
        let mut page = ReadIndexPage {
            next_offset: header.next_offset,
            complete: header.flags & COMPLETE != 0,
            valid: header.flags & !COMPLETE == 0 && header.padding == 0,
            records: Vec::new(),
        };
        let mut name_buf = [0; MAX_NAME_LEN];
        let mut last_offset = None;
        while reader.remain() != 0 {
            let Ok(entry) = reader.read_val::<EntryReply>() else {
                page.valid = false;
                break;
            };
            let Ok(dirent) = reader.read_val::<Dirent>() else {
                page.valid = false;
                page.records.push(ReadIndexRecord { name: None, entry });
                break;
            };
            let len = dirent.namelen() as usize;
            let padded_len = len.saturating_add(7) & !7;
            if len == 0 || len > name_buf.len() || padded_len > reader.remain() {
                page.valid = false;
                page.records.push(ReadIndexRecord { name: None, entry });
                break;
            }

            read_bytes(reader, &mut name_buf[..len])?;
            reader.skip(padded_len - len);
            let bytes = &name_buf[..len];
            let name = if bytes.contains(&0) || bytes.contains(&b'/') {
                page.valid = false;
                None
            } else if bytes == b"." || bytes == b".." {
                None
            } else {
                core::str::from_utf8(bytes).ok().map(String::from)
            };
            if dirent.off() == 0 || last_offset.is_some_and(|offset| dirent.off() <= offset) {
                page.valid = false;
            }
            last_offset = Some(dirent.off());
            page.records.push(ReadIndexRecord { name, entry });
        }
        page.valid &= last_offset.is_none_or(|offset| offset == header.next_offset);
        Ok(page)
    }
}
