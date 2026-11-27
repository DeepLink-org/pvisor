// SPDX-License-Identifier: MPL-2.0

use super::SyscallReturn;
use crate::{
    fs,
    fs::file::file_table::{RawFileDesc, get_file_fast},
    prelude::*,
    util::VmWriterArray,
};

pub(super) fn sys_readv(
    raw_fd: RawFileDesc,
    io_vec_ptr: Vaddr,
    io_vec_count: usize,
    ctx: &Context,
) -> Result<SyscallReturn> {
    let res = do_sys_readv(raw_fd, io_vec_ptr, io_vec_count, ctx)?;
    Ok(SyscallReturn::Return(res as _))
}

pub(super) fn sys_preadv(
    raw_fd: RawFileDesc,
    io_vec_ptr: Vaddr,
    io_vec_count: usize,
    offset_low: u64,
    _offset_high: u64,
    ctx: &Context,
) -> Result<SyscallReturn> {
    // On 64-bit platforms, Linux assumes the `offset_low` contains the full
    // 64-bit offset.
    // Reference: <https://elixir.bootlin.com/linux/v6.16.9/source/fs/read_write.c#L1114-L1118>.
    let offset = offset_low.cast_signed();
    let res = do_sys_preadv(
        raw_fd,
        io_vec_ptr,
        io_vec_count,
        offset,
        RWFFlag::empty(),
        ctx,
    )?;
    Ok(SyscallReturn::Return(res as _))
}

pub(super) fn sys_preadv2(
    raw_fd: RawFileDesc,
    io_vec_ptr: Vaddr,
    io_vec_count: usize,
    offset_low: u64,
    _offset_high: u64,
    flags: u32,
    ctx: &Context,
) -> Result<SyscallReturn> {
    // On 64-bit platforms, Linux assumes the `offset_low` contains the full
    // 64-bit offset.
    // Reference: <https://elixir.bootlin.com/linux/v6.16.9/source/fs/read_write.c#L1114-L1118>.
    let offset = offset_low.cast_signed();
    let flags = match RWFFlag::from_bits(flags) {
        Some(flags) => flags,
        None => return_errno_with_message!(Errno::EOPNOTSUPP, "unsupported flags"),
    };
    let res = if offset == -1 {
        do_sys_readv(raw_fd, io_vec_ptr, io_vec_count, ctx)?
    } else {
        do_sys_preadv(raw_fd, io_vec_ptr, io_vec_count, offset, flags, ctx)?
    };
    Ok(SyscallReturn::Return(res as _))
}

fn do_sys_preadv(
    raw_fd: RawFileDesc,
    io_vec_ptr: Vaddr,
    io_vec_count: usize,
    offset: i64,
    _flags: RWFFlag,
    ctx: &Context,
) -> Result<usize> {
    debug!(
        "preadv: raw_fd = {}, io_vec_ptr = 0x{:x}, io_vec_counter = 0x{:x}, offset = 0x{:x}",
        raw_fd, io_vec_ptr, io_vec_count, offset
    );

    if offset < 0 {
        return_errno_with_message!(Errno::EINVAL, "offset cannot be negative");
    }

    let mut file_table = ctx.thread_local.borrow_file_table_mut();
    let file = get_file_fast!(&mut file_table, raw_fd.try_into()?);

    let user_space = ctx.user_space();
    let mut writer_array = VmWriterArray::from_user_io_vecs(&user_space, io_vec_ptr, io_vec_count)?;

    // Probe with a zero-length read so that unsupported files (e.g., pipes)
    // still return the correct errno even when all buffers are empty.
    if writer_array.writers_mut().is_empty() {
        let mut empty = [0u8; 0];
        let mut writer = VmWriter::from(empty.as_mut_slice()).to_fallible();
        file.read_at(offset as usize, &mut writer)?;
        return Ok(0);
    }

    let total_len = file.read_vectored(Some(offset as usize), writer_array.writers_mut())?;
    if total_len > 0 {
        fs::vfs::notify::on_access(&file);
    }

    Ok(total_len)
}

fn do_sys_readv(
    raw_fd: RawFileDesc,
    io_vec_ptr: Vaddr,
    io_vec_count: usize,
    ctx: &Context,
) -> Result<usize> {
    debug!(
        "raw_fd = {}, io_vec_ptr = 0x{:x}, io_vec_counter = 0x{:x}",
        raw_fd, io_vec_ptr, io_vec_count
    );

    let mut file_table = ctx.thread_local.borrow_file_table_mut();
    let file = get_file_fast!(&mut file_table, raw_fd.try_into()?);

    if io_vec_count == 0 {
        return Ok(0);
    }

    let user_space = ctx.user_space();
    let mut writer_array = VmWriterArray::from_user_io_vecs(&user_space, io_vec_ptr, io_vec_count)?;
    if writer_array.writers_mut().is_empty() {
        return Ok(0);
    }

    let total_len = file.read_vectored(None, writer_array.writers_mut())?;
    if total_len > 0 {
        fs::vfs::notify::on_access(&file);
    }

    Ok(total_len)
}

bitflags! {
    struct RWFFlag: u32 {
        const RWF_DSYNC = 0x00000001;
        const RWF_HIPRI = 0x00000002;
        const RWF_SYNC = 0x00000004;
        const RWF_NOWAIT = 0x00000008;
    }
}
