// SPDX-License-Identifier: MPL-2.0

//! `FUSE_FORGET` releases lookup references held for an inode.
//!
//! The request body contains [`ForgetReq`] for the inode named by the request
//! header. `FUSE_FORGET` is a one-way notification; the server sends no reply.

use ostd::mm::{Infallible, VmReader, VmWriter};

use crate::{FuseError, FuseNodeId, FuseOpcode, FuseOperation, FuseResult, ReplyExpectation};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod)]
pub struct ForgetReq {
    /// Number of lookup references being released.
    nlookup: u64,
}

impl ForgetReq {
    pub const fn new(nlookup: u64) -> Self {
        Self { nlookup }
    }
}

pub struct ForgetOperation {
    forget_req: ForgetReq,
}

impl ForgetOperation {
    pub fn new(forget_req: ForgetReq) -> Self {
        Self { forget_req }
    }
}

impl FuseOperation for ForgetOperation {
    type Output = ();

    fn opcode(&self) -> FuseOpcode {
        FuseOpcode::Forget
    }

    fn body_len(&self) -> usize {
        size_of::<ForgetReq>()
    }

    fn write_body(&mut self, writer: &mut VmWriter<'_, Infallible>) -> FuseResult<()> {
        writer
            .write_val(&self.forget_req)
            .map_err(|_| FuseError::BufferTooSmall)
    }

    fn reply_expectation(&self) -> ReplyExpectation {
        ReplyExpectation::None
    }

    fn parse_reply(
        _payload_len: usize,
        _reader: &mut VmReader<'_, Infallible>,
    ) -> FuseResult<Self::Output> {
        Ok(())
    }
}

/// Releases multiple lookup references on the high-priority queue.
pub struct BatchForgetOperation<'a>(pub &'a [FuseNodeId]);

impl FuseOperation for BatchForgetOperation<'_> {
    type Output = ();

    fn opcode(&self) -> FuseOpcode {
        FuseOpcode::BatchForget
    }

    fn body_len(&self) -> usize {
        8 + 16 * self.0.len()
    }

    fn write_body(&mut self, writer: &mut VmWriter<'_, Infallible>) -> FuseResult<()> {
        writer
            .write_val(&(self.0.len() as u32))
            .map_err(|_| FuseError::BufferTooSmall)?;
        writer
            .write_val(&0u32)
            .map_err(|_| FuseError::BufferTooSmall)?;
        for nodeid in self.0 {
            writer
                .write_val(nodeid)
                .map_err(|_| FuseError::BufferTooSmall)?;
            writer
                .write_val(&1u64)
                .map_err(|_| FuseError::BufferTooSmall)?;
        }
        Ok(())
    }

    fn reply_expectation(&self) -> ReplyExpectation {
        ReplyExpectation::None
    }

    fn parse_reply(_payload_len: usize, _reader: &mut VmReader<'_, Infallible>) -> FuseResult<()> {
        Ok(())
    }
}
