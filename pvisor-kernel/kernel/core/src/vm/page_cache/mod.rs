// SPDX-License-Identifier: MPL-2.0

//! Page-cache infrastructure shared by filesystems, memory mappings, and
//! writeback.
//!
//! This module is the filesystem-facing entry point to the page-cache
//! subsystem. A filesystem typically keeps one [`PageCache`] per file (or per
//! inode data stream) and uses it for buffered I/O, truncate/extend, and cache
//! invalidation.
//!
//! # Overview
//!
//! The subsystem is split into four layers with different responsibilities:
//!
//! - [`PageCache`] is the per-file façade used by filesystems. It exposes
//!   buffered I/O, resize, flush, and invalidation operations in filesystem
//!   terms.
//! - [`Vmo`] is the lower-level memory object underneath a page cache. It owns
//!   the page array, commits pages on demand, and is the abstraction shared
//!   with page-fault and mapping code.
//! - [`CachePage`] stores cached page contents, while
//!   [`cache_page::PageState`] records whether those contents are uninitialized,
//!   clean, or dirty.
//! - [`PageCacheBackend`] is the storage-facing contract used
//!   to make a page readable or durable.
//!
//! For a file with a backend, the steady-state flow is
//! `filesystem metadata -> PageCache -> Vmo -> PageCacheBackend`. Anonymous
//! page caches use the same `PageCache` / `Vmo` layers without a backend.
//!
//! # Responsibility Boundary
//!
//! `PageCache` manages page-aligned cache capacity and cached contents. The
//! filesystem still owns file size, extent or block mapping metadata, and the
//! higher-level locking that keeps those decisions stable while the page cache
//! is accessed.
//!
//! Stay at [`PageCache`] when the caller is operating in filesystem terms. Drop
//! to [`Vmo`] only when code needs the lower-level memory-object interface
//! directly, such as `mmap` setup or page-fault handling.
//!
//! # Synchronization Model
//!
//! The page-cache subsystem serializes per-page state transitions, including
//! the backend page states in [`cache_page::PageState`] and the auxiliary
//! writeback tracking bit in [`CachePageMeta`].
//!
//! [`PageCache`] is stored inside a filesystem-level lock or inode lock. It
//! prevents conflicting buffered I/O and resizing. However, it cannot prevent
//! concurrent page faults. They need to be synchronized with page table locks,
//! the reverse-mapping lock, and per-page locks. This module handles the
//! complexity in cooperation with [`super::vmar`].
//!
//! A complete overview of how cached file I/O ("read"/"write" below),
//! `O_DIRECT` file I/O ("read (direct)"/"write (direct)"), page cache
//! management operations ("resize"/"flush"/"evict"), and page faults
//! ("fault (load)"/"fault (store)") interact with each other when they are
//! invoked concurrently is shown below.
//!
//! |                | read            | write           | read (direct) | write (direct)  | resize      | flush       | evict       | fault (load) | fault (store) |
//! |----------------|-----------------|-----------------|---------------|-----------------|-------------|-------------|-------------|--------------|---------------|
//! | read           | N/A             |                 |               |                 |             |             |             |              |               |
//! | write          | inode*          | inode*          |               |                 |             |             |             |              |               |
//! | read (direct)  | N/A             | (flush)         | N/A           |                 |             |             |             |              |               |
//! | write (direct) | (flush + evict) | (flush + evict) | inode*        | inode*          |             |             |             |              |               |
//! | resize         | inode           | inode           | inode         | inode           | inode       |             |             |              |               |
//! | flush          | N/A             | page            | (flush)       | (flush + evict) | rmap + page | rmap + page |             |              |               |
//! | evict          | N/A             | page            | (flush)       | (flush + evict) | rmap + page | rmap + page | rmap + page |              |               |
//! | fault (load)   | N/A             | N/A             | N/A           | (flush + evict) | PT + xarray | N/A         | PT + page   | N/A          |               |
//! | fault (store)  | N/A             | N/A             | (flush)       | (flush + evict) | PT + xarray | PT + page   | PT + page   | N/A          | N/A           |
//!
//! The annotations in the table are explained below.
//!
//!  - If two operations have no semantic conflicts, even if they are called
//!    concurrently (e.g., if both operations try to read something), the
//!    corresponding entry is noted as `N/A`.
//!
//!  - Userspace programs should not mix cached and direct file I/O. Otherwise,
//!    data corruption caused by the program will be the user's fault. The
//!    corresponding entry is noted as `(OP)` where `OP` is the operation
//!    inserted by the filesystem before starting direct file I/O, which
//!    ensures the correctness when the two operations are called
//!    **sequentially**.
//!
//!  - For two operations protected by the inode lock, the entry is noted as
//!    `inode` or `inode*`. The latter, `inode*`, means that lock usage is not
//!    yet enforced at the page cache layer or the Rust type level (i.e.,
//!    [`PageCache`] operations do not yet take `&mut self`).
//!
//!  - In this table, `flush` and `evict` are assumed to be able to be called
//!    without holding the inode lock (e.g., under memory pressure). But as we
//!    have not implemented any related mechanisms, they can currently only be
//!    called from the filesystem while holding at least the inode read lock.
//!
//!  - For interactions between page faults and page cache management, the
//!    correct order of multiple locks is important to ensure that the cached
//!    page is always in a consistent state, even when operations are called
//!    concurrently. The corresponding entry is noted as `LockA + LockB`. We
//!    use `rmap` to denote the reverse-mapping lock, `PT` to denote the page
//!    table lock, `page` to denote the page lock, and `xarray` to denote the
//!    lock of VMO pages. For details, see the comments in the implementation.

use core::{ops::Range, sync::atomic::Ordering};

use align_ext::AlignExt;
use io_util::batch::IoBatch;
use ostd::mm::VmIo;

use crate::prelude::*;

mod cache_page;
#[cfg(ktest)]
mod tests;
mod vmo;

pub(crate) use cache_page::{CachePage, CachePageExt, CachePageMeta, LockedCachePage};
pub use vmo::Vmo;
pub(crate) use vmo::{VmoCommitError, VmoFlags, VmoMapMode, VmoOptions};

/// The page cache for a file-like object.
///
/// This is the abstraction a filesystem usually stores in an inode: it handles
/// buffered reads and writes, writeback, invalidation, and page-cache resizing,
/// while delegating per-page population and writeback to the underlying [`Vmo`].
///
/// A `PageCache` owns cached page contents and a page-aligned capacity. It does
/// not own:
///
/// - the filesystem's file size (EOF);
/// - extent or block-mapping metadata; or
/// - the higher-level synchronization that keeps metadata, buffered I/O,
///   invalidation, and VM mappings coherent.
///
/// Filesystems backed by persistent or remote storage create it with
/// [`PageCache::new_with_backend`] and a [`PageCacheBackend`]. Purely
/// in-memory filesystems can use [`PageCache::new_anon`] to get the same
/// buffered-I/O interface without a backend.
///
/// A `PageCache` is more than just an `Arc<Vmo>` wrapper. It acts as a unique
/// handle to the underlying [`Vmo`], enabling additional operations and
/// enforcing the correct concurrency contract (see the module-level
/// documentation of [`crate::vm::page_cache`]). Typically, a user (such as a
/// file system) should place a `PageCache` inside a [`Mutex`] or [`RwMutex`].
/// Operations requiring exclusive access to the page cache handle require
/// `&mut self`, so a proper inode or file system lock must be obtained in
/// advance.
///
/// Reach for [`PageCache::as_vmo`] only when a lower-level consumer such as
/// memory mapping or page-fault code must operate on the underlying [`Vmo`]
/// directly.
#[derive(Debug)]
pub(crate) struct PageCache(Arc<Vmo>);

impl PageCache {
    /// Creates a page cache with a backend and the specified initial capacity
    /// in bytes.
    pub(crate) fn new_with_backend(
        size: usize,
        backend: Weak<dyn PageCacheBackend>,
    ) -> Result<Self> {
        Ok(Self(VmoOptions::new_page_cache(size, backend).alloc()?))
    }

    /// Creates an anonymous page cache with the specified initial capacity in
    /// bytes.
    pub(crate) fn new_anon(size: usize) -> Result<Self> {
        Ok(Self(VmoOptions::new_anon(size).alloc()?))
    }

    /// Returns the wrapped [`Vmo`].
    pub(crate) fn as_vmo(&self) -> &Arc<Vmo> {
        &self.0
    }

    /// Returns the current page-cache capacity in bytes.
    ///
    /// This size is page-aligned and may exceed the file size. The filesystem
    /// remains responsible for tracking EOF separately.
    pub(crate) fn size(&self) -> usize {
        self.0.size()
    }

    /// Resizes the page-cache capacity to cover a new file size.
    ///
    /// `new_file_size` is the post-resize file size requested by the filesystem.
    /// The underlying cache capacity is rounded up to page boundaries. If the
    /// new size is smaller than the current size, pages that fall entirely
    /// within the truncated range will be decommitted (freed). For the page
    /// that is only partially truncated (i.e., the page containing the new
    /// boundary), the truncated portion will be filled with zeros instead.
    ///
    /// The `old_file_size` must be the file length before this resize. It is
    /// used to determine the boundary of previously valid data so that only the
    /// discarded range within a partially truncated tail page is zero-filled.
    ///
    /// Extending the page cache does not eagerly allocate pages and therefore
    /// cannot return an error. Shrinking may fail because it can zero-fill or
    /// decommit existing pages.
    ///
    /// # Size Synchronization
    ///
    /// `PageCache::resize` only updates the page-aligned [`Vmo`] capacity. The
    /// filesystem must keep that capacity synchronized with its own file size
    /// under the same inode- or file-level lock that excludes conflicting
    /// buffered I/O and resizing.
    ///
    /// The required ordering is:
    ///
    /// - When extending a file, update the file size before increasing
    ///   [`Vmo::size`] so subsequent reads can observe the new range.
    /// - When truncating a file, shrink [`Vmo::size`] before decreasing the
    ///   file size so reads beyond the new EOF cannot observe stale cached
    ///   pages.
    ///
    /// Accordingly, `old_file_size` must be the pre-resize file size captured
    /// inside that resize critical section.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use ostd::mm::PAGE_SIZE;
    ///
    /// use crate::vm::page_cache::PageCache;
    ///
    /// let mut page_cache = PageCache::new_anon(0).unwrap();
    ///
    /// // Extend: publish the new file size first, then grow the page cache
    /// // with the previous file size.
    /// let mut file_size = PAGE_SIZE + 123;
    /// page_cache.resize(file_size, 0).unwrap();
    /// assert_eq!(page_cache.size(), 2 * PAGE_SIZE);
    ///
    /// // Truncate: shrink the page cache first, while passing the old file
    /// // size captured in the same critical section.
    /// let old_file_size = file_size;
    /// file_size = 512;
    /// page_cache.resize(file_size, old_file_size).unwrap();
    /// assert_eq!(page_cache.size(), PAGE_SIZE);
    /// ```
    pub(crate) fn resize(&mut self, new_file_size: usize, old_file_size: usize) -> Result<()> {
        self.resize_impl(new_file_size, old_file_size, true)
    }

    /// Reflects a size change already applied by the backing filesystem.
    /// The caller must invalidate cached pages affected by the change.
    pub(crate) fn resize_from_backing(&mut self, new_size: usize, old_size: usize) -> Result<()> {
        self.resize_impl(new_size, old_size, false)
    }

    fn resize_impl(
        &mut self,
        new_file_size: usize,
        old_file_size: usize,
        zero_tail: bool,
    ) -> Result<()> {
        let vmo = &self.0;
        assert!(vmo.flags.contains(VmoFlags::RESIZABLE));

        if zero_tail && new_file_size < old_file_size && !new_file_size.is_multiple_of(PAGE_SIZE) {
            let fill_zero_end = old_file_size.min(new_file_size.align_up(PAGE_SIZE));
            self.fill_zeros(new_file_size..fill_zero_end)?;
        } else if zero_tail
            && new_file_size > old_file_size
            && !old_file_size.is_multiple_of(PAGE_SIZE)
        {
            let fill_zero_end = new_file_size.min(old_file_size.align_up(PAGE_SIZE));
            self.fill_zeros(old_file_size..fill_zero_end)?;
        }

        let rmap = if new_file_size < old_file_size {
            Some(vmo.rmap.lock())
        } else {
            None
        };

        let new_cache_size = new_file_size.align_up(PAGE_SIZE);
        let locked_pages = vmo.pages.lock();
        let old_cache_size = vmo.size();
        if new_cache_size == old_cache_size {
            return Ok(());
        }

        vmo.size.store(new_cache_size, Ordering::Release);

        if let Some(locked_rmap) = rmap {
            let decommit_range = new_cache_size..old_cache_size;
            if let Some(backed_vmo) = vmo.as_backed_vmo() {
                backed_vmo.decommit_pages(locked_rmap, locked_pages, &decommit_range)?;
            } else {
                vmo.decommit_anon_pages(locked_rmap, locked_pages, decommit_range)?;
            }
        }

        Ok(())
    }

    /// Flushes dirty pages in the specified range to the backend storage.
    ///
    /// This walks the current cache contents, submits writeback for pages that
    /// are dirty when this pass reaches them, and waits for the submitted I/O
    /// to complete.
    ///
    /// Filesystems that need `fsync`-like guarantees must still exclude
    /// concurrent writers or repeat the operation until their own ordering
    /// requirements are met.
    ///
    /// If the given range exceeds the current size of the page cache, only the pages within
    /// the valid range will be flushed.
    pub(crate) fn flush_range(&self, range: Range<usize>) -> Result<()> {
        let Some(vmo) = self.0.as_backed_vmo() else {
            return Ok(());
        };

        vmo.flush_dirty_pages(&range)
    }

    /// Evicts clean pages within the specified range from the page cache.
    ///
    /// Only pages in the `UpToDate` state are removed. Dirty and uninitialized
    /// pages are left in place. This is useful for invalidating cached data that
    /// is no longer needed and can be re-read from the backend if necessary
    /// (e.g., before direct I/O operations).
    pub(crate) fn evict_range(&self, range: Range<usize>) -> Result<()> {
        let Some(vmo) = self.0.as_backed_vmo() else {
            return Ok(());
        };

        vmo.evict_up_to_date_pages(&range)
    }

    /// Flushes dirty pages and then evicts clean pages in the specified range.
    ///
    /// This is the standard preparation step before issuing direct I/O that must
    /// bypass the page cache. It uses the same locking requirements as
    /// [`PageCache::flush_range`] and [`PageCache::evict_range`].
    pub(crate) fn invalidate_range(&self, range: Range<usize>) -> Result<()> {
        let Some(vmo) = self.0.as_backed_vmo() else {
            return Ok(());
        };

        vmo.flush_dirty_pages(&range)?;
        vmo.evict_up_to_date_pages(&range)
    }

    /// Fills the specified range of the page cache with zeros.
    ///
    /// Callers must hold the filesystem-level lock that serializes operations in
    /// the target range.
    pub(crate) fn fill_zeros(&self, range: Range<usize>) -> Result<()> {
        self.0.fill_zeros(range)
    }
}

impl VmIo for PageCache {
    fn read(&self, offset: usize, writer: &mut VmWriter) -> ostd::Result<()> {
        self.0.read(offset, writer)?;
        Ok(())
    }

    fn write(&self, offset: usize, reader: &mut VmReader) -> ostd::Result<()> {
        self.0.write(offset, reader)?;
        Ok(())
    }
}

/// A storage backend for a backed [`PageCache`].
///
/// This trait is the high-level contract used by the page-cache layer to load
/// page contents from storage and write dirty contents back. A successful read
/// makes the requested page usable by readers; a successful write makes the
/// page's current contents durable in the backend.
pub(crate) trait PageCacheBackend: Sync + Send {
    /// Reads a page from the backend asynchronously.
    ///
    /// The caller may try to pass an index that exceeds the size of the
    /// underlying backend (e.g., the file size). This can occur if file
    /// truncation and a page fault occur at the same time. If this happens,
    /// this method should fail with `EINVAL`.
    fn read_page_async(
        &self,
        idx: usize,
        locked_page: LockedCachePage,
        io_batch: &mut IoBatch,
    ) -> Result<()>;

    /// Reads multiple pages from the backend asynchronously.
    ///
    /// Backends that can process several pages in one request should override
    /// this method. The default implementation submits each page separately.
    fn read_pages_async(&self, pages: PageRun<'_>, io_batch: &mut IoBatch) -> Result<()> {
        for (idx, locked_page) in pages.into_indexed() {
            self.read_page_async(idx, locked_page, io_batch)?;
        }
        Ok(())
    }

    /// Writes a page to the backend asynchronously.
    ///
    /// If the caller tries to pass an index that exceeds the size of the
    /// underlying backend (e.g., the file size), this method should fail with
    /// `EINVAL`. Note that this cannot happen until we support concurrent file
    /// truncation and page writeback.
    //
    // TODO: Revise this behavior once file truncation and page writeback can
    // happen concurrently.
    fn write_page_async(
        &self,
        idx: usize,
        locked_page: LockedCachePage,
        io_batch: &mut IoBatch,
    ) -> Result<()>;

    /// Writes multiple pages to the backend asynchronously.
    ///
    /// Backends that can process several pages in one request should override
    /// this method. The default implementation submits each page separately.
    fn write_pages_async(&self, pages: PageRun<'_>, io_batch: &mut IoBatch) -> Result<()> {
        for (idx, locked_page) in pages.into_indexed() {
            self.write_page_async(idx, locked_page, io_batch)?;
        }
        Ok(())
    }
}

impl dyn PageCacheBackend {
    /// Reads a page from the backend synchronously.
    pub(crate) fn read_page(&self, idx: usize, page: LockedCachePage) -> Result<()> {
        let mut io_batch = IoBatch::with_capacity(1);
        self.read_page_async(idx, page, &mut io_batch)?;
        io_batch.wait_all()?;
        Ok(())
    }
}

/// A non-empty run of consecutive logical page-cache indices.
///
/// The first item is at [`Self::start_idx`], and every following item is at the
/// next index.
pub(crate) struct PageRun<'a> {
    start_idx: usize,
    pages: &'a mut dyn ExactSizeIterator<Item = LockedCachePage>,
}

impl Iterator for PageRun<'_> {
    type Item = LockedCachePage;

    fn next(&mut self) -> Option<Self::Item> {
        let page = self.pages.next()?;
        self.start_idx += 1;
        Some(page)
    }
}

impl<'a> PageRun<'a> {
    pub(super) fn new<I>(start_idx: usize, pages: &'a mut I) -> Self
    where
        I: ExactSizeIterator<Item = LockedCachePage>,
    {
        debug_assert!(pages.len() > 0);
        Self { start_idx, pages }
    }

    /// Returns the first logical page index in the run.
    pub(crate) fn start_idx(&self) -> usize {
        self.start_idx
    }

    /// Returns the number of pages in the run.
    pub(crate) fn len(&self) -> usize {
        self.pages.len()
    }

    /// Returns the ordered pages paired with their logical indices.
    fn into_indexed(self) -> impl ExactSizeIterator<Item = (usize, LockedCachePage)> {
        let start_idx = self.start_idx;
        self.pages
            .enumerate()
            .map(move |(offset, page)| (start_idx + offset, page))
    }
}
