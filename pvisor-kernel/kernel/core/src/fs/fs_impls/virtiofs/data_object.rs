// SPDX-License-Identifier: MPL-2.0

//! Mount-local clean data pages, separate from the mutable namespace inode.

use aster_fuse::ReadReq;
use aster_virtio::device::filesystem::{
    device::{FuseSession, MAX_READ_DATA_PAGES_PER_REQUEST},
    pool::{FuseReplyBuf, FuseReplyBufs},
};
use io_util::batch::IoBatch;
use lru::LruCache;
use ostd::mm::{Segment, io::util::HasVmReaderWriter};
use smallvec::SmallVec;

use super::open_handle::{OpenHandles, VirtioFsOpenHandle};
use crate::{
    prelude::*,
    vm::page_cache::{LockedCachePage, PageCache, PageCacheBackend, PageRun},
};

pub(super) struct LowerObjects {
    active: BTreeMap<u32, Weak<LowerObject>>,
    idle: LruCache<u32, (Arc<LowerObject>, usize)>,
    charged: usize,
    budget: usize,
}

impl LowerObjects {
    pub(super) fn new(ram: usize) -> Self {
        Self {
            active: BTreeMap::new(),
            idle: LruCache::unbounded(),
            charged: 0,
            budget: (ram / 32).min(16 << 20),
        }
    }

    pub(super) fn bind(
        &mut self,
        (id, size): (u32, usize),
        session: &Arc<FuseSession>,
        handle: &Arc<VirtioFsOpenHandle>,
    ) -> Result<Arc<LowerObject>> {
        let object = if let Some(open) = self.active.get(&id).and_then(Weak::upgrade) {
            open
        } else {
            let object = if let Some((idle, charge)) = self.idle.pop(&id) {
                self.charged -= charge;
                idle
            } else {
                LowerObject::new(session, id, size)?
            };
            self.active.insert(id, Arc::downgrade(&object));
            object
        };
        if object.size() != size {
            return_errno_with_message!(Errno::EIO, "inconsistent immutable object size");
        }
        object.reader.handles.insert(handle);
        Ok(object)
    }

    pub(super) fn release(&mut self, object: Arc<LowerObject>) {
        // A mapping keeps its open file alive, including across fork and close(fd).
        if Arc::strong_count(&object) != 1 {
            return;
        }
        let id = object.id;
        self.active.remove(&id);
        let limit = self.budget / PAGE_SIZE;
        let pages = object.pages.as_vmo().resident_pages_up_to(limit + 1);
        if pages == 0 || pages > limit {
            return;
        }
        let charge = pages * PAGE_SIZE;
        while self.charged + charge > self.budget || self.idle.len() >= 1024 {
            let (_, (_, evicted_charge)) = self.idle.pop_lru().unwrap();
            self.charged -= evicted_charge;
        }
        self.charged += charge;
        self.idle.put(id, (object, charge));
    }
}

pub(super) struct LowerObject {
    id: u32,
    pub(super) pages: PageCache,
    reader: Arc<LowerReader>,
}

impl LowerObject {
    fn new(session: &Arc<FuseSession>, id: u32, size: usize) -> Result<Arc<Self>> {
        let reader = Arc::new(LowerReader {
            session: session.clone(),
            size,
            handles: OpenHandles::new(),
        });
        let backend = Arc::downgrade(&reader);
        let pages = PageCache::new_with_backend(size, backend)?;
        Ok(Arc::new(Self { id, pages, reader }))
    }

    pub(super) fn read_at(&self, offset: usize, writers: &mut [VmWriter]) -> Result<usize> {
        super::inode::read_cached(&self.pages, self.size(), offset, writers)
    }

    pub(super) fn size(&self) -> usize {
        self.reader.size
    }
}

struct LowerReader {
    session: Arc<FuseSession>,
    size: usize,
    // Only handles whose successful OPEN returned this object's receipt.
    handles: OpenHandles,
}

impl LowerReader {
    fn fill(&self, index: usize, pages: &[LockedCachePage]) -> Result<()> {
        let offset = index.checked_mul(PAGE_SIZE).ok_or(Errno::EOVERFLOW)?;
        if offset >= self.size {
            return_errno!(Errno::EINVAL);
        }
        let len = (pages.len() * PAGE_SIZE).min(self.size - offset);
        let handle = self.handles.find_readable_handle().ok_or(Errno::EBADF)?;
        let buffers = pages
            .iter()
            .map(|p| FuseReplyBuf::new_map(Segment::from((**p).clone()).into()))
            .collect::<core::result::Result<FuseReplyBufs, _>>()?;
        let req = ReadReq::new(handle.fh(), offset as u64, len as u32, handle.file_flags());
        if self.session.read_buffers(handle.nodeid(), req, buffers)? != len {
            return_errno_with_message!(Errno::EIO, "short immutable object read");
        }
        if len % PAGE_SIZE != 0 {
            let mut tail = pages.last().unwrap().writer();
            tail.skip(len % PAGE_SIZE);
            tail.fill_zeros(PAGE_SIZE - len % PAGE_SIZE);
        }
        for page in pages {
            page.set_up_to_date();
        }
        Ok(())
    }
}

impl PageCacheBackend for LowerReader {
    fn read_page_async(&self, idx: usize, page: LockedCachePage, _: &mut IoBatch) -> Result<()> {
        self.fill(idx, core::slice::from_ref(&page))
    }

    fn read_pages_async(&self, mut pages: PageRun<'_>, _: &mut IoBatch) -> Result<()> {
        while pages.len() != 0 {
            let start = pages.start_idx();
            let chunk: SmallVec<[_; 1]> =
                (&mut pages).take(MAX_READ_DATA_PAGES_PER_REQUEST).collect();
            self.fill(start, &chunk)?;
        }
        Ok(())
    }

    fn write_page_async(&self, _: usize, _: LockedCachePage, _: &mut IoBatch) -> Result<()> {
        return_errno!(Errno::EROFS);
    }
}
