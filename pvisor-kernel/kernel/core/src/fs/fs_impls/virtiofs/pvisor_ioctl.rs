// SPDX-License-Identifier: MPL-2.0

//! Bounded FUSE forwarding for the existing pVisor supervisor exit protocol.

use aster_fuse::{
    FuseError, FuseFileHandle, FuseOpcode, FuseOperation, FuseResult, ReplyExpectation,
    FUSE_ROOT_ID,
};
use ostd::mm::{Infallible, VmIo};

use super::inode::VirtioFsInode;
use crate::{context::current_userspace, prelude::*, util::ioctl::RawIoctl};

const CLAIM: u32 = 0x8010_7603;
const REPORT: u32 = 0x4014_7604;

pub(super) fn ioctl(inode: &VirtioFsInode, fh: FuseFileHandle, raw: RawIoctl) -> Result<i32> {
    if inode.nodeid() != FUSE_ROOT_ID || !matches!(raw.cmd(), CLAIM | REPORT) {
        return_errno_with_message!(Errno::ENOTTY, "unsupported virtiofs ioctl");
    }
    let mut input = [0u8; 20];
    let (input_size, output_size) = if raw.cmd() == REPORT {
        input = current_userspace!().read_val(raw.arg())?;
        (20u32, 0u32)
    } else {
        // Validate the destination before consuming the host's one-shot claim.
        let _ = current_userspace!().writer(raw.arg(), 16)?;
        (0u32, 16u32)
    };
    let mut header = [0u8; 32];
    VmWriter::from(&mut header[..8]).write_val(&fh).unwrap();
    header[8..12].copy_from_slice(&16u32.to_le_bytes());
    header[12..16].copy_from_slice(&raw.cmd().to_le_bytes());
    header[24..28].copy_from_slice(&input_size.to_le_bytes());
    header[28..32].copy_from_slice(&output_size.to_le_bytes());
    let reply = inode.fs_ref().session().do_fuse_op(
        inode.nodeid(),
        ExitIoctl {
            header,
            input,
            input_size: input_size as usize,
        },
    )?;
    if reply.len() != 16 + output_size as usize || reply[..16] != [0; 16] {
        return_errno_with_message!(Errno::EIO, "invalid supervisor ioctl reply");
    }
    if output_size != 0 {
        let token: [u8; 16] = reply[16..].try_into().unwrap();
        current_userspace!().write_val(raw.arg(), &token)?;
    }
    Ok(0)
}

struct ExitIoctl {
    header: [u8; 32],
    input: [u8; 20],
    input_size: usize,
}

impl FuseOperation for ExitIoctl {
    type Output = Vec<u8>;

    fn opcode(&self) -> FuseOpcode {
        FuseOpcode::Ioctl
    }

    fn body_len(&self) -> usize {
        self.header.len() + self.input_size
    }

    fn write_body(&mut self, writer: &mut VmWriter<'_, Infallible>) -> FuseResult<()> {
        if writer.avail() < self.body_len() {
            return Err(FuseError::BufferTooSmall);
        }
        writer.write_val(&self.header).unwrap();
        if self.input_size != 0 {
            writer.write_val(&self.input).unwrap();
        }
        Ok(())
    }

    fn reply_expectation(&self) -> ReplyExpectation {
        ReplyExpectation::variable_payload(32)
    }

    fn parse_reply(
        payload_len: usize,
        reader: &mut VmReader<'_, Infallible>,
    ) -> FuseResult<Self::Output> {
        if !matches!(payload_len, 16 | 32) || reader.remain() < payload_len {
            return Err(FuseError::MalformedResponse);
        }
        let mut bytes = vec![0; payload_len];
        reader.read(&mut VmWriter::from(&mut bytes[..]));
        Ok(bytes)
    }
}
