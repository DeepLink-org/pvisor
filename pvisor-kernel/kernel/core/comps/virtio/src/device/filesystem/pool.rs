// SPDX-License-Identifier: MPL-2.0

//! Size-classed DMA buffer allocation.
//!
//! This module provides `VirtiofsDmaPool`, a size-class allocator backed by
//! [`DmaPool`] segments for small buffers and a shared DMA arena for large ones.

use alloc::sync::Arc;

use aster_util::mem_obj_slice::Slice;
use dma_pool::{DmaArenaPool, DmaBuffer, DmaPool};
use ostd::{
    Result,
    boot::{boot_info, memory_region::MemoryRegionType},
    mm::{
        HasSize, Infallible, PAGE_SIZE, USegment, VmReader, VmWriter,
        dma::{DmaDirection, DmaStream, FromDevice, ToDevice},
        io::util::{HasVmReaderWriter, VmReaderWriterResult},
    },
    sync::SpinLock,
};
use smallvec::SmallVec;
use spin::Once;

use crate::dma_buf::DmaBuf;

/// Pool-backed buffers start at 64 bytes to avoid wasting a page for small
/// fixed-size buffers.
const MIN_SHIFT: usize = 6;

/// Larger buffers use the directional arena or a per-request DMA stream.
const MAX_SHIFT: usize = 12;

/// The number of pool-backed size classes.
const N_CLASSES: usize = MAX_SHIFT - MIN_SHIFT + 1;

/// The largest buffer size served by a pooled DMA segment.
pub(super) const MAX_CLASS_SIZE: usize = 1 << MAX_SHIFT;

/// Retain small-buffer pages for request bursts.
const POOL_HIGH_WATERMARK: usize = 64;

const MAX_DMA_ARENA_PAGES: usize = 8 * 1024 * 1024 / PAGE_SIZE;

type SharedArena<D> = Once<Option<Arc<DmaArenaPool<D>>>>;
pub(super) static REPLY_ARENA: SharedArena<FromDevice> = Once::new();
pub(super) static REQUEST_ARENA: SharedArena<ToDevice> = Once::new();

fn allocate_arena<D: DmaDirection>() -> Option<Arc<DmaArenaPool<D>>> {
    let usable_pages: usize = boot_info()
        .memory_regions
        .iter()
        .filter(|region| region.typ() == MemoryRegionType::Usable)
        .map(|region| region.len() / PAGE_SIZE)
        .sum();
    // Both directions together may reserve at most one eighth of usable RAM.
    let pages = (usable_pages / 16).min(MAX_DMA_ARENA_PAGES);
    DmaArenaPool::new(pages).ok()
}

/// A virtio-fs DMA buffer allocator for pooled segments and large arenas.
#[derive(Debug)]
pub(super) struct VirtiofsDmaPool<D: DmaDirection> {
    // The retained segment must drop before its size-class pool.
    last_control: SpinLock<Option<Arc<Slice<DmaBuffer<D>>>>>,
    classes: [Arc<DmaPool<D>>; N_CLASSES],
    arena: &'static SharedArena<D>,
}

impl<D: DmaDirection> VirtiofsDmaPool<D> {
    pub(super) fn new(arena: &'static SharedArena<D>) -> Self {
        let classes = core::array::from_fn(|i| {
            let segment_size = 1 << (MIN_SHIFT + i);
            DmaPool::<D>::new(segment_size, 0, POOL_HIGH_WATERMARK, false)
        });

        Self {
            classes,
            arena,
            last_control: SpinLock::new(None),
        }
    }

    /// Reuse the DMA mapping, never a request or its contents. An in-flight
    /// descriptor or a reply still being parsed keeps its own strong reference.
    fn alloc_control_buf(&self, len: usize) -> Result<Arc<Slice<DmaBuffer<D>>>> {
        if len > MAX_CLASS_SIZE {
            return self.alloc_buf(len);
        }
        if let Some(mut slot) = self.last_control.try_lock()
            && let Some(buf) = slot.as_mut()
            && buf.size() == len
            && Arc::get_mut(buf).is_some()
        {
            return Ok(buf.clone());
        }

        let buf = self.alloc_buf(len)?;
        if let Some(mut slot) = self.last_control.try_lock() {
            let old = slot.replace(buf.clone());
            drop(slot);
            drop(old);
        }
        Ok(buf)
    }

    /// Allocates a DMA buffer whose visible length is `len`.
    fn alloc_buf(&self, len: usize) -> Result<Arc<Slice<DmaBuffer<D>>>> {
        if len == 0 {
            return Err(ostd::Error::InvalidArgs);
        }

        let storage = if len <= MAX_CLASS_SIZE {
            let shift = MIN_SHIFT.max(len.next_power_of_two().trailing_zeros() as usize);
            DmaBuffer::Pooled(self.classes[shift - MIN_SHIFT].alloc_segment()?)
        } else {
            let pages = len.div_ceil(PAGE_SIZE);
            let arena = self.arena.call_once(allocate_arena);
            match arena.as_ref().and_then(|pool| pool.alloc(pages)) {
                Some(arena) => DmaBuffer::Arena(arena),
                None => DmaBuffer::Direct(DmaStream::alloc_uninit(pages, false)?),
            }
        };

        Ok(Arc::new(Slice::new(storage, 0..len)))
    }
}

impl VirtiofsDmaPool<FromDevice> {
    /// Allocates a DMA buffer for FUSE reply payloads.
    pub(super) fn alloc_reply_buf(&self, len: usize) -> Result<FuseReplyBuf> {
        self.alloc_buf(len).map(FuseReplyBuf)
    }
}

impl VirtiofsDmaPool<ToDevice> {
    /// Allocates a DMA buffer for FUSE requests.
    pub(super) fn alloc_request_buf(&self, len: usize) -> Result<FuseRequestBuf> {
        self.alloc_buf(len).map(FuseRequestBuf)
    }
}

/// Data payload buffers used by FUSE I/O operations.
pub(super) enum FuseDataBuf {
    /// Data filled by the device for read FUSE operations.
    Read(FuseReplyBufs),
    /// Data sent to the device for write FUSE operations.
    Write(FuseRequestBufs),
}

/// A DMA buffer used by FUSE requests.
#[derive(Clone, Debug)]
pub struct FuseRequestBuf(Arc<Slice<DmaBuffer<ToDevice>>>);

/// Request buffers used by a FUSE write operation.
pub type FuseRequestBufs = SmallVec<[FuseRequestBuf; 1]>;

impl FuseRequestBuf {
    pub(super) fn control(pool: &VirtiofsDmaPool<ToDevice>, len: usize) -> Result<Self> {
        pool.alloc_control_buf(len).map(Self)
    }

    /// Returns the length of the DMA buffer.
    pub(crate) fn len(&self) -> usize {
        DmaBuf::len(self.0.as_ref())
    }

    /// Returns the DMA slice used by virtqueue descriptors.
    pub(crate) fn as_dma_slice(&self) -> &Slice<DmaBuffer<ToDevice>> {
        self.0.as_ref()
    }

    /// Synchronizes the whole buffer from memory to the device.
    pub(crate) fn sync_to_device(&self) -> Result<()> {
        self.0.mem_obj().sync_to_device(self.0.offset().clone())
    }
}

impl HasVmReaderWriter for FuseRequestBuf {
    type Types = VmReaderWriterResult;

    fn reader(&self) -> Result<VmReader<'_, Infallible>> {
        self.0.reader()
    }

    fn writer(&self) -> Result<VmWriter<'_, Infallible>> {
        self.0.writer()
    }
}

/// A DMA buffer used by FUSE replies.
#[derive(Clone, Debug)]
pub struct FuseReplyBuf(Arc<Slice<DmaBuffer<FromDevice>>>);

/// Reply buffers used by a FUSE read operation.
pub type FuseReplyBufs = SmallVec<[FuseReplyBuf; 1]>;

impl FuseReplyBuf {
    pub(super) fn control(pool: &VirtiofsDmaPool<FromDevice>, len: usize) -> Result<Self> {
        pool.alloc_control_buf(len).map(Self)
    }

    /// Maps `segment` as a DMA buffer for FUSE reply payloads.
    pub fn new_map(segment: USegment) -> Result<Self> {
        let len = segment.size();
        let stream = DmaStream::map(segment, false)?;

        Ok(FuseReplyBuf(Arc::new(Slice::new(
            DmaBuffer::Direct(stream),
            0..len,
        ))))
    }

    /// Returns the length of the DMA buffer.
    pub(crate) fn len(&self) -> usize {
        DmaBuf::len(self.0.as_ref())
    }

    /// Returns the DMA slice used by virtqueue descriptors.
    pub(crate) fn as_dma_slice(&self) -> &Slice<DmaBuffer<FromDevice>> {
        self.0.as_ref()
    }

    /// Synchronizes the whole buffer from the device into memory.
    pub(crate) fn sync_from_device(&self) -> Result<()> {
        self.0.mem_obj().sync_from_device(self.0.offset().clone())
    }
}

impl HasVmReaderWriter for FuseReplyBuf {
    type Types = VmReaderWriterResult;

    fn reader(&self) -> Result<VmReader<'_, Infallible>> {
        self.0.reader()
    }

    fn writer(&self) -> Result<VmWriter<'_, Infallible>> {
        self.0.writer()
    }
}

#[cfg(ktest)]
mod tests {
    use ostd::{mm::HasDaddr, prelude::ktest};

    use super::*;

    #[ktest]
    fn control_buffers_reuse_only_idle_exact_length_storage() {
        let pool = VirtiofsDmaPool::new(&REPLY_ARENA);
        let outstanding = FuseReplyBuf::control(&pool, 120).unwrap();
        let idle_address = {
            let second = FuseReplyBuf::control(&pool, 120).unwrap();
            assert_ne!(outstanding.0.daddr(), second.0.daddr());
            second.0.daddr()
        };
        let reused = FuseReplyBuf::control(&pool, 120).unwrap();
        assert_eq!(reused.0.daddr(), idle_address);
        drop(reused);
        let shorter = FuseReplyBuf::control(&pool, 96).unwrap();
        assert_eq!(shorter.len(), 96);
        assert_ne!(shorter.0.daddr(), outstanding.0.daddr());
    }
}
