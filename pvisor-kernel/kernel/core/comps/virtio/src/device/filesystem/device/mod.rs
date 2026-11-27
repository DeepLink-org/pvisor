// SPDX-License-Identifier: MPL-2.0

//! Virtiofs device request handling.
//!
//! This module defines [`FileSystemDevice`], which initializes the virtiofs
//! queues, tracks in-flight requests, and sends typed FUSE operations to the
//! server.

mod queue;
mod request;
mod session;
mod virtio_ops;
mod waiter;

use alloc::{string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

use aster_fuse::{
    FuseCompleteFn, FuseError, FuseNodeId, FuseOpcode, FuseOperation, FuseUnique, ReplyExpectation,
    ReplyHeader, ReqHeader, WriteReply,
};
use ostd::{
    mm::{
        dma::{FromDevice, ToDevice},
        io::util::HasVmReaderWriter,
    },
    sync::{LocalIrqDisabled, SpinLock},
};
use queue::FsRequestQueue;
use request::{FuseRequest, IndexChange, IndexEpoch, RequestBufs};
use spin::Once;
use waiter::{FuseWaiter, ReplyBufs};

pub use self::session::{AttrVersion, FuseSession};

static REQUEST_CREDENTIALS: Once<fn() -> (u32, u32, u32)> = Once::new();

/// Installs the kernel credential provider before mounting any virtio-fs filesystem.
/// Threadless requests use PID 0 and must never impersonate a userspace process.
pub fn set_request_credentials(provider: fn() -> (u32, u32, u32)) {
    REQUEST_CREDENTIALS.call_once(|| provider);
}

fn request_credentials() -> (u32, u32, u32) {
    REQUEST_CREDENTIALS.get().map_or((0, 0, 0), |get| get())
}

fn changes_index(opcode: FuseOpcode) -> bool {
    matches!(
        opcode,
        FuseOpcode::Setattr
            | FuseOpcode::Mknod
            | FuseOpcode::Mkdir
            | FuseOpcode::Unlink
            | FuseOpcode::Rmdir
            | FuseOpcode::Symlink
            | FuseOpcode::Rename
            | FuseOpcode::Rename2
            | FuseOpcode::Link
            | FuseOpcode::Create
            | FuseOpcode::Tmpfile
            | FuseOpcode::Setxattr
            | FuseOpcode::Removexattr
            | FuseOpcode::Open
            | FuseOpcode::Write
            | FuseOpcode::Fallocate
            | FuseOpcode::CopyFileRange
            // Reads can change atime, even when the namespace is unchanged.
            | FuseOpcode::Read
            | FuseOpcode::Readlink
            | FuseOpcode::Opendir
            | FuseOpcode::Readdir
            | FuseOpcode::Readdirplus
    )
}

use crate::{
    device::filesystem::{
        pool,
        pool::{FuseDataBuf, FuseReplyBuf, FuseReplyBufs, FuseRequestBuf, VirtiofsDmaPool},
    },
    transport::DeviceTransport,
};

/// Maximum data pages packed into one FUSE read request as DMA buffers.
///
/// A read request reserves one virtqueue descriptor for its request header and
/// one for its reply header. Batching beyond this limit must be split across
/// multiple requests.
pub const MAX_READ_DATA_PAGES_PER_REQUEST: usize = queue::MAX_DMA_BUFS_PER_REQUEST - 2;

/// Maximum data pages packed into one FUSE write request as DMA buffers.
///
/// A write reserves one descriptor for its request header and one for its
/// complete fixed-size reply.
pub const MAX_WRITE_DATA_PAGES_PER_REQUEST: usize = queue::MAX_DMA_BUFS_PER_REQUEST - 2;

static FILESYSTEM_DEVICES: Once<SpinLock<Vec<Arc<FileSystemDevice>>, LocalIrqDisabled>> =
    Once::new();

/// A virtiofs device that issues FUSE requests to a server.
///
/// # Locking
///
/// FUSE requests may be submitted while the caller holds sleepable VFS
/// guards. The request path takes virtio-fs locks in this order:
/// VFS sleepable guard -> selected request queue `SpinLock`.
pub struct FileSystemDevice {
    transport: SpinLock<DeviceTransport, LocalIrqDisabled>,
    hiprio_queue: Arc<FsRequestQueue>,
    request_queues: Vec<Arc<FsRequestQueue>>,
    to_device_pool: VirtiofsDmaPool<ToDevice>,
    from_device_pool: VirtiofsDmaPool<FromDevice>,
    next_unique: AtomicU64,
    tag: String,
    notify_supported: bool,
    index_epoch: Option<Arc<SpinLock<IndexEpoch, LocalIrqDisabled>>>,
}

impl FileSystemDevice {
    fn new(
        transport: DeviceTransport,
        hiprio_queue: Arc<FsRequestQueue>,
        request_queues: Vec<Arc<FsRequestQueue>>,
        tag: String,
        notify_supported: bool,
        read_index_supported: bool,
    ) -> Self {
        Self {
            transport: SpinLock::new(transport),
            hiprio_queue,
            request_queues,
            to_device_pool: VirtiofsDmaPool::new(&pool::REQUEST_ARENA),
            from_device_pool: VirtiofsDmaPool::new(&pool::REPLY_ARENA),
            // Start request IDs at 1 and keep 0 unused. In FUSE,
            // `unique == 0` is reserved for unsolicited notification messages
            // rather than ordinary request/reply matching.
            next_unique: AtomicU64::new(1),
            tag,
            notify_supported,
            index_epoch: read_index_supported
                .then(|| Arc::new(SpinLock::new(IndexEpoch::default()))),
        }
    }

    /// Submits a FUSE operation and returns a waiter for request completion.
    ///
    /// # Locking
    ///
    /// Builds the request before taking the selected request queue `SpinLock`.
    /// The submit path may sleep waiting for free queue descriptors, but the
    /// queue lock is never held across that wait.
    fn submit_fuse_op<Op: FuseOperation>(
        &self,
        nodeid: FuseNodeId,
        operation: &mut Op,
        data_bufs: Option<FuseDataBuf>,
        complete_fn: Option<FuseCompleteFn>,
    ) -> Result<Arc<FuseWaiter>, FuseError> {
        let opcode = operation.opcode();
        let deferred = complete_fn.is_some();
        let request = self.prepare_request(nodeid, operation, data_bufs, complete_fn)?;
        let waiter = request.waiter().clone();

        let queue = self.select_request_queue(request.nodeid());
        queue.add_request(request, opcode, deferred)?;

        Ok(waiter)
    }

    fn init_completion_taskless(&self) {
        self.hiprio_queue.init_completion_taskless();

        for queue in &self.request_queues {
            queue.init_completion_taskless();
        }
    }

    fn prepare_request<Op: FuseOperation>(
        &self,
        nodeid: FuseNodeId,
        operation: &mut Op,
        data_bufs: Option<FuseDataBuf>,
        complete_fn: Option<FuseCompleteFn>,
    ) -> Result<FuseRequest, FuseError> {
        let unique = self.alloc_unique();
        let reply_expectation = operation.reply_expectation();
        let opcode = operation.opcode();

        let data_buf_len = match data_bufs.as_ref() {
            Some(FuseDataBuf::Write(data_bufs)) => data_bufs.iter().map(FuseRequestBuf::len).sum(),
            _ => 0,
        };

        let request_buf =
            self.alloc_and_fill_request_buf(nodeid, operation, unique, data_buf_len)?;

        let (request_bufs, reply_bufs) = match data_bufs {
            Some(FuseDataBuf::Read(data_bufs)) => {
                let reply_bufs = self.alloc_reply_bufs(reply_expectation, Some(data_bufs))?;
                let mut request_bufs = RequestBufs::new();
                request_bufs.push(request_buf);
                (request_bufs, reply_bufs)
            }
            Some(FuseDataBuf::Write(data_bufs)) => {
                for data_buf in data_bufs.iter() {
                    data_buf.sync_to_device().unwrap();
                }

                let reply_bufs = self.alloc_reply_bufs(reply_expectation, None)?;
                if reply_bufs.is_empty() {
                    return Err(FuseError::MalformedResponse);
                }

                let mut request_bufs = RequestBufs::new();
                request_bufs.push(request_buf);
                request_bufs.extend(data_bufs);
                (request_bufs, reply_bufs)
            }
            None => {
                let reply_bufs = self.alloc_reply_bufs(reply_expectation, None)?;

                let mut request_bufs = RequestBufs::new();
                request_bufs.push(request_buf);
                (request_bufs, reply_bufs)
            }
        };

        Ok(FuseRequest::new(
            unique,
            nodeid,
            reply_expectation,
            request_bufs,
            reply_bufs,
            complete_fn,
            self.index_epoch
                .as_ref()
                .filter(|_| changes_index(opcode))
                .map(IndexChange::start),
        ))
    }

    fn select_request_queue(&self, nodeid: FuseNodeId) -> &FsRequestQueue {
        let request_queue_count = self.request_queues.len();
        let queue_index = if request_queue_count <= 1 {
            0
        } else {
            (nodeid.as_u64() as usize) % request_queue_count
        };

        self.request_queues[queue_index].as_ref()
    }

    fn alloc_unique(&self) -> FuseUnique {
        FuseUnique::new(self.next_unique.fetch_add(1, Ordering::Relaxed))
    }

    fn alloc_and_fill_request_buf(
        &self,
        nodeid: FuseNodeId,
        operation: &mut impl FuseOperation,
        unique: FuseUnique,
        data_buf_len: usize,
    ) -> Result<FuseRequestBuf, FuseError> {
        let request_buf_len = (size_of::<ReqHeader>() as u32)
            .checked_add(operation.body_len() as u32)
            .ok_or(FuseError::LengthOverflow)?;
        let data_buf_len = u32::try_from(data_buf_len).map_err(|_| FuseError::LengthOverflow)?;

        let total_len = request_buf_len
            .checked_add(data_buf_len)
            .ok_or(FuseError::LengthOverflow)?;

        let request_buf = self
            .to_device_pool
            .alloc_request_buf(request_buf_len as usize)
            .map_err(FuseError::ResourceAlloc)?;

        let (uid, gid, pid) = request_credentials();
        let request_header = ReqHeader::new(total_len, operation.opcode() as u32, unique, nodeid)
            .with_credentials(uid, gid, pid);

        let mut writer = request_buf.writer().unwrap();
        writer.write_val(&request_header).unwrap();
        operation.write_body(&mut writer)?;

        request_buf.sync_to_device().unwrap();

        Ok(request_buf)
    }

    fn alloc_reply_bufs(
        &self,
        reply_expectation: ReplyExpectation,
        data_bufs: Option<FuseReplyBufs>,
    ) -> Result<ReplyBufs, FuseError> {
        match (reply_expectation, data_bufs) {
            (ReplyExpectation::None, None) => Ok(ReplyBufs::new(None)),
            (ReplyExpectation::HeaderOnly, None) => self.alloc_control_reply(0),
            (
                ReplyExpectation::FixedPayload(payload_size)
                | ReplyExpectation::VariablePayload(payload_size),
                None,
            ) => self.alloc_control_reply(payload_size.get()),
            (ReplyExpectation::WritePayload { .. }, None) => {
                self.alloc_control_reply(size_of::<WriteReply>())
            }
            (
                ReplyExpectation::FixedPayload(payload_size)
                | ReplyExpectation::VariablePayload(payload_size),
                Some(data_bufs),
            ) => {
                if payload_size.get() > data_bufs.iter().map(FuseReplyBuf::len).sum() {
                    return Err(FuseError::BufferTooSmall);
                }

                let mut reply_bufs = ReplyBufs::new(Some(self.alloc_reply_header_buf()?));
                for data_buf in data_bufs {
                    reply_bufs.push_payload(data_buf);
                }
                Ok(reply_bufs)
            }
            (ReplyExpectation::WritePayload { .. }, Some(_)) => Err(FuseError::MalformedResponse),
            (_, Some(_)) => Err(FuseError::MalformedResponse),
        }
    }

    fn alloc_control_reply(&self, payload_size: usize) -> Result<ReplyBufs, FuseError> {
        let header_size = size_of::<ReplyHeader>();
        if payload_size <= pool::MAX_CLASS_SIZE - header_size {
            let buf = self.alloc_from_device_buf(header_size + payload_size)?;
            return Ok(ReplyBufs::new(Some(buf)));
        }
        let mut bufs = ReplyBufs::new(Some(self.alloc_reply_header_buf()?));
        bufs.push_payload(self.alloc_from_device_buf(payload_size)?);
        Ok(bufs)
    }

    fn alloc_reply_header_buf(&self) -> Result<FuseReplyBuf, FuseError> {
        self.alloc_from_device_buf(size_of::<ReplyHeader>())
    }

    fn alloc_from_device_buf(&self, len: usize) -> Result<FuseReplyBuf, FuseError> {
        self.from_device_pool
            .alloc_reply_buf(len)
            .map_err(FuseError::ResourceAlloc)
    }
}

fn register_device(device: Arc<FileSystemDevice>) {
    FILESYSTEM_DEVICES
        .call_once(|| SpinLock::new(Vec::new()))
        .lock()
        .push(device);
}

/// Finds the virtio-fs device registered with the given `tag`.
pub fn find_device_by_tag(tag: &str) -> Option<Arc<FileSystemDevice>> {
    let devices = FILESYSTEM_DEVICES.get()?;
    let devices = devices.lock();
    devices
        .iter()
        .find(|device| device.tag.as_str() == tag)
        .cloned()
}

/// Virtio-fs reserves queue 0 for high-priority requests such as `FUSE_FORGET`.
const HIPRIO_QUEUE_INDEX: u16 = 0;

/// The default queue size for any queue in virtio-fs.
const DEFAULT_QUEUE_SIZE: u16 = 128;
