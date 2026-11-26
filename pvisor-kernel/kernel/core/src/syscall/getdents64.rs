// SPDX-License-Identifier: MPL-2.0

use core::marker::PhantomData;

use align_ext::AlignExt;

use super::SyscallReturn;
use crate::{
    fs,
    fs::{
        file::{
            InodeType,
            file_table::{RawFileDesc, get_file_fast},
        },
        utils::DirentVisitor,
    },
    prelude::*,
};

pub(super) fn sys_getdents(
    raw_fd: RawFileDesc,
    buf_addr: Vaddr,
    buf_len: usize,
    ctx: &Context,
) -> Result<SyscallReturn> {
    debug!(
        "raw_fd = {}, buf_addr = 0x{:x}, buf_len = 0x{:x}",
        raw_fd, buf_addr, buf_len
    );

    let mut file_table = ctx.thread_local.borrow_file_table_mut();
    let file = get_file_fast!(&mut file_table, raw_fd.try_into()?);
    let inode_handle = file.as_inode_handle_or_err()?;
    if inode_handle.path().type_() != InodeType::Dir {
        return_errno!(Errno::ENOTDIR);
    }
    let user_space = ctx.user_space();
    let writer = user_space.writer(buf_addr, buf_len)?;
    let mut reader = DirentBufferReader::<Dirent>::new(writer);
    let _ = inode_handle.readdir(&mut reader)?;
    let read_len = reader.read_len();
    fs::vfs::notify::on_access(&file);
    Ok(SyscallReturn::Return(read_len as _))
}

pub(super) fn sys_getdents64(
    raw_fd: RawFileDesc,
    buf_addr: Vaddr,
    buf_len: usize,
    ctx: &Context,
) -> Result<SyscallReturn> {
    debug!(
        "raw_fd = {}, buf_addr = 0x{:x}, buf_len = 0x{:x}",
        raw_fd, buf_addr, buf_len
    );

    let mut file_table = ctx.thread_local.borrow_file_table_mut();
    let file = get_file_fast!(&mut file_table, raw_fd.try_into()?);
    let inode_handle = file.as_inode_handle_or_err()?;
    if inode_handle.path().type_() != InodeType::Dir {
        return_errno!(Errno::ENOTDIR);
    }
    let user_space = ctx.user_space();
    let writer = user_space.writer(buf_addr, buf_len)?;
    let mut reader = DirentBufferReader::<Dirent64>::new(writer);
    let _ = inode_handle.readdir(&mut reader)?;
    let read_len = reader.read_len();
    fs::vfs::notify::on_access(&file);
    Ok(SyscallReturn::Return(read_len as _))
}

trait DirentHeader: Pod {
    fn new(ino: u64, offset: u64, reclen: u16, type_: InodeType) -> Self;
}

struct DirentBufferReader<'a, T: DirentHeader> {
    writer: VmWriter<'a>,
    record: Vec<u8>,
    read_len: usize,
    phantom: PhantomData<T>,
}

impl<'a, T: DirentHeader> DirentBufferReader<'a, T> {
    pub(crate) fn new(writer: VmWriter<'a>) -> Self {
        Self {
            writer,
            record: Vec::new(),
            read_len: 0,
            phantom: PhantomData,
        }
    }

    pub(crate) fn read_len(&self) -> usize {
        self.read_len
    }
}

impl<T: DirentHeader> DirentVisitor for DirentBufferReader<'_, T> {
    fn remaining_bytes(&self) -> Option<usize> {
        Some(self.writer.avail())
    }

    fn visit(&mut self, name: &str, ino: u64, type_: InodeType, offset: usize) -> Result<()> {
        if name.as_bytes().contains(&0) {
            return_errno_with_message!(Errno::E2BIG, "directory entry contains NUL");
        }
        let len = (size_of::<T>() + name.len() + 1).align_up(8);
        let reclen = u16::try_from(len).map_err(|_| Error::new(Errno::EOVERFLOW))?;
        if self.writer.avail() < len {
            return_errno_with_message!(
                Errno::EINVAL,
                "the buffer is too small for the directory entry"
            );
        }

        self.record.clear();
        self.record.reserve(len);
        let header = T::new(ino, offset as u64, reclen, type_);
        self.record.extend_from_slice(header.as_bytes());
        self.record.extend_from_slice(name.as_bytes());
        self.record.resize(len, 0);
        self.writer
            .write_fallible(&mut VmReader::from(self.record.as_slice()))?;
        self.read_len += len;

        Ok(())
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Pod)]
struct Dirent {
    d_ino: u64,
    d_off: u64,
    d_reclen: u16,
}

impl DirentHeader for Dirent {
    fn new(ino: u64, offset: u64, reclen: u16, _type_: InodeType) -> Self {
        Self {
            d_ino: ino,
            d_off: offset,
            d_reclen: reclen,
        }
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Pod)]
struct Dirent64 {
    d_ino: u64,
    d_off: u64,
    d_reclen: u16,
    d_type: u8,
}

impl DirentHeader for Dirent64 {
    fn new(ino: u64, offset: u64, reclen: u16, type_: InodeType) -> Self {
        Self {
            d_ino: ino,
            d_off: offset,
            d_reclen: reclen,
            d_type: DirentType::from(type_) as u8,
        }
    }
}

#[expect(non_camel_case_types)]
#[repr(u8)]
#[derive(Clone, Copy, Debug)]
enum DirentType {
    DT_UNKNOWN = 0,
    DT_FIFO = 1,
    DT_CHR = 2,
    DT_DIR = 4,
    DT_BLK = 6,
    DT_REG = 8,
    DT_LNK = 10,
    DT_SOCK = 12,
    #[cfg_attr(target_arch = "x86_64", expect(dead_code))]
    DT_WHT = 14,
}

impl From<InodeType> for DirentType {
    fn from(type_: InodeType) -> Self {
        match type_ {
            InodeType::Unknown => DirentType::DT_UNKNOWN,
            InodeType::File => DirentType::DT_REG,
            InodeType::Dir => DirentType::DT_DIR,
            InodeType::SymLink => DirentType::DT_LNK,
            InodeType::CharDevice => DirentType::DT_CHR,
            InodeType::BlockDevice => DirentType::DT_BLK,
            InodeType::Socket => DirentType::DT_SOCK,
            InodeType::NamedPipe => DirentType::DT_FIFO,
        }
    }
}
