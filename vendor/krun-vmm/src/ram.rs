//! File-backed guest RAM. DAX/GPU windows are deliberately not RAM backing.

#[cfg(target_os = "macos")]
pub use hvf::{MemoryFault, MemoryFaultHandler};

use std::fs::File;
use std::sync::Arc;
use vm_memory::{FileOffset, GuestAddress, GuestMemory, GuestMemoryMmap, GuestMemoryRegion};

#[derive(Debug, Clone, Copy)]
pub struct RamReclaim {
    pub backed_bytes: u64,
    pub resident_before_bytes: Option<u64>,
    pub resident_after_bytes: Option<u64>,
}

pub(crate) fn map(
    ram: &[(GuestAddress, usize)],
    shared: &[(GuestAddress, usize)],
    file: Arc<File>,
) -> Result<GuestMemoryMmap, String> {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page <= 0 {
        return Err("cannot determine host page size".into());
    }
    let page = page as u64;
    let mut offset = 0u64;
    let mut ranges = Vec::new();
    for &(address, size) in ram {
        ranges.push((
            address,
            size,
            Some(FileOffset::from_arc(file.clone(), offset)),
        ));
        offset = offset
            .checked_add(size as u64)
            .and_then(|size| size.checked_add(page - 1))
            .map(|size| size / page * page)
            .ok_or("RAM backing size overflow")?;
    }
    if offset > i64::MAX as u64 {
        return Err("RAM backing exceeds file offset range".into());
    }
    file.set_len(offset).map_err(|e| e.to_string())?;
    ranges.extend(shared.iter().map(|&(address, size)| (address, size, None)));
    GuestMemoryMmap::from_ranges_with_files(ranges).map_err(|e| format!("{e:?}"))
}

pub(crate) fn residency(memory: &GuestMemoryMmap) -> Option<u64> {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page <= 0 {
        return None;
    }
    let mut bytes = 0;
    let mut vector = [0u8; 4096];
    for region in memory
        .iter()
        .filter(|region| region.file_offset().is_some())
    {
        let mut offset = 0;
        while offset < region.len() as usize {
            let length = (region.len() as usize - offset).min(vector.len() * page as usize);
            let pages = length.div_ceil(page as usize);
            if unsafe {
                libc::mincore(
                    region.as_ptr().add(offset).cast(),
                    length,
                    vector.as_mut_ptr().cast(),
                )
            } != 0
            {
                return None;
            }
            bytes += vector[..pages]
                .iter()
                .filter(|byte| **byte & 1 != 0)
                .count() as u64
                * page as u64;
            offset += length;
        }
    }
    Some(bytes)
}

/// Diagnostic object/offset identities, not physical page numbers. Caller must
/// quiesce CPU and device mapping users. This does not fault pages into RAM.
#[cfg(target_os = "macos")]
pub(crate) fn page_inventory(memory: &GuestMemoryMmap) -> Result<(u64, Vec<[u64; 4]>), String> {
    // Native <mach/vm_region.h> VM_PAGE_INFO_BASIC ABI (8 integer_t words).
    #[repr(C)]
    #[derive(Default)]
    struct PageInfo {
        disposition: i32,
        ref_count: i32,
        object_id: u64,
        offset: u64,
        depth: i32,
        pad: i32,
    }
    unsafe extern "C" {
        static mach_task_self_: u32;
        fn mach_vm_page_info(task: u32, address: u64, flavor: i32,
            info: *mut PageInfo, count: *mut u32) -> i32;
    }
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page <= 0 { return Err("cannot determine host page size".into()); }
    let page = page as usize;
    let mut rows = Vec::new();
    for region in memory.iter().filter(|region| region.file_offset().is_some()) {
        if region.len() as usize % page != 0 || region.as_ptr() as usize % page != 0 {
            return Err("RAM inventory requires host-page-aligned ranges".into());
        }
        for offset in (0..region.len() as usize).step_by(page) {
            let mut info = PageInfo::default();
            let mut count = 8;
            let result = unsafe { mach_vm_page_info(mach_task_self_,
                region.as_ptr().add(offset) as u64, 1, &mut info, &mut count) };
            if result != 0 || count != 8 {
                return Err(format!("RAM page query failed: kernel={result} count={count}"));
            }
            rows.push([region.start_addr().0 + offset as u64, info.object_id,
                info.offset, info.disposition as u32 as u64]);
        }
    }
    Ok((page as u64, rows))
}

pub(crate) fn reclaim(memory: &GuestMemoryMmap) -> Result<u64, String> {
    let mut bytes = 0;
    for region in memory
        .iter()
        .filter(|region| region.file_offset().is_some())
    {
        let address = region.as_ptr().cast();
        let size = region.len() as usize;
        // Keep the MAP_SHARED address valid for device workers. No copying,
        // MAP_FIXED replacement, MADV_FREE, or anonymous-page discard.
        let flags = libc::MS_SYNC;
        #[cfg(target_os = "macos")]
        let flags = flags | libc::MS_INVALIDATE;
        if unsafe { libc::msync(address, size, flags) } != 0 {
            return Err(format!(
                "RAM writeback: {}",
                std::io::Error::last_os_error()
            ));
        }
        if unsafe { libc::madvise(address, size, libc::MADV_DONTNEED) } != 0 {
            return Err(format!("RAM reclaim: {}", std::io::Error::last_os_error()));
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let backing = region.file_offset().unwrap();
            let error = unsafe {
                libc::posix_fadvise(
                    backing.file().as_raw_fd(),
                    backing.start() as libc::off_t,
                    size as libc::off_t,
                    libc::POSIX_FADV_DONTNEED,
                )
            };
            if error != 0 {
                return Err(format!(
                    "RAM cache reclaim: {}",
                    std::io::Error::from_raw_os_error(error)
                ));
            }
        }
        bytes += region.len();
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vm_memory::Bytes;

    #[test]
    #[cfg(target_os = "macos")]
    fn detached_file_reclaim_preserves_private_ram_and_neighboring_blocks() {
        use std::io::{Read, Seek, SeekFrom};
        let temporary = vmm_sys_util::tempfile::TempFile::new().unwrap();
        let file = Arc::new(temporary.as_file().try_clone().unwrap());
        let chunk = 64 * 1024;
        let memory = map(&[(GuestAddress(0), chunk * 2)], &[], file.clone()).unwrap();
        memory.write_slice(b"file", GuestAddress(0)).unwrap();
        memory.write_slice(b"neighbor", GuestAddress(chunk as u64)).unwrap();
        let block = blocks(&memory, chunk).unwrap().remove(0);
        // No HVF mapping exists in this test. Emulate the completed host mapping
        // replacement, then a CPU restore into a private anonymous mapping.
        let replaced = unsafe { libc::mmap(block.host as *mut _, chunk,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_FIXED | libc::MAP_PRIVATE | libc::MAP_ANON, -1, 0) };
        assert_ne!(replaced, libc::MAP_FAILED);
        memory.write_slice(b"private", GuestAddress(0)).unwrap();
        for _ in 0..2 {
            unsafe { block.reclaim_file().unwrap(); }
            let mut private = [0; 7];
            memory.read_slice(&mut private, GuestAddress(0)).unwrap();
            assert_eq!(&private, b"private");
            let mut persisted = file.try_clone().unwrap();
            persisted.seek(SeekFrom::Start(0)).unwrap();
            let mut hole = [1; 4];
            persisted.read_exact(&mut hole).unwrap();
            assert_eq!(hole, [0; 4]);
            persisted.seek(SeekFrom::Start(chunk as u64)).unwrap();
            let mut neighbor = [0; 8];
            persisted.read_exact(&mut neighbor).unwrap();
            assert_eq!(&neighbor, b"neighbor");
            assert_eq!(file.metadata().unwrap().len(), (chunk * 2) as u64);
        }
    }

    #[test]
    fn reclaim_preserves_shared_ram_and_excludes_device_windows() {
        let temporary = vmm_sys_util::tempfile::TempFile::new().unwrap();
        let file = Arc::new(temporary.as_file().try_clone().unwrap());
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let memory = map(
            &[
                (GuestAddress(0), page),
                (GuestAddress((page * 2) as u64), page),
            ],
            &[(GuestAddress((page * 4) as u64), page)],
            file.clone(),
        )
        .unwrap();
        memory.write_slice(b"first", GuestAddress(0)).unwrap();
        memory
            .write_slice(b"second", GuestAddress((page * 2) as u64))
            .unwrap();
        memory
            .write_slice(b"device", GuestAddress((page * 4) as u64))
            .unwrap();
        assert_eq!(reclaim(&memory).unwrap(), (page * 2) as u64);
        for (address, expected) in [
            (0, b"first".as_slice()),
            (page * 2, b"second".as_slice()),
            (page * 4, b"device".as_slice()),
        ] {
            let mut actual = vec![0; expected.len()];
            memory
                .read_slice(&mut actual, GuestAddress(address as u64))
                .unwrap();
            assert_eq!(actual, expected);
        }
        assert_eq!(file.metadata().unwrap().len(), (page * 2) as u64);
        // Repeated offloads retain writes made after the first reclaim.
        memory.write_slice(b"again", GuestAddress(0)).unwrap();
        assert_eq!(reclaim(&memory).unwrap(), (page * 2) as u64);
        let mut actual = [0; 5];
        memory.read_slice(&mut actual, GuestAddress(0)).unwrap();
        assert_eq!(&actual, b"again");
        use std::io::{Read, Seek, SeekFrom};
        let mut persisted = file.try_clone().unwrap();
        persisted.seek(SeekFrom::Start(0)).unwrap();
        persisted.read_exact(&mut actual).unwrap();
        assert_eq!(&actual, b"again");
        persisted.seek(SeekFrom::Start(page as u64)).unwrap();
        let mut actual = [0; 6];
        persisted.read_exact(&mut actual).unwrap();
        assert_eq!(&actual, b"second");
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn mapping_rejects_overflow_and_nonwritable_backing_before_allocation() {
        let temporary = vmm_sys_util::tempfile::TempFile::new().unwrap();
        let file = Arc::new(temporary.as_file().try_clone().unwrap());
        assert!(map(&[(GuestAddress(0), usize::MAX)], &[], file).is_err());
        assert_eq!(temporary.as_file().metadata().unwrap().len(), 0);
        let readonly = Arc::new(File::open(temporary.as_path()).unwrap());
        assert!(map(&[(GuestAddress(0), 4096)], &[], readonly).is_err());
    }
}

/// Experimental HVF block. The retained mapping keeps its host address alive.
/// Mapping mutations require the owning VM's quiescent epoch or a serialized
/// fault for an already inaccessible block. Never use after the VM has stopped.
#[cfg(target_os = "macos")]
#[derive(Clone)]
pub struct RamBlock {
    _memory: Arc<GuestMemoryMmap>,
    guest: u64,
    host: usize,
    bytes: usize,
    file: Arc<File>,
    offset: u64,
}
#[cfg(target_os = "macos")]
impl RamBlock {
    pub fn guest_address(&self) -> u64 { self.guest }
    pub fn length(&self) -> usize { self.bytes }
    /// # Safety
    /// All vCPUs must be paused and device references drained. Block must be resident.
    pub unsafe fn snapshot(&self, output: &mut [u8]) -> std::io::Result<()> {
        if output.len() != self.bytes { return Err(std::io::Error::other("RAM block length mismatch")); }
        unsafe { std::ptr::copy_nonoverlapping(self.host as *const u8, output.as_mut_ptr(), self.bytes); }
        Ok(())
    }
    /// # Safety
    /// Synchronize sampling with all vCPUs and device users; inaccessible blocks
    /// may be reenabled only after their content is restored.
    pub unsafe fn observe(&self, enabled: bool) -> Result<(), String> {
        hvf::HvfVm {}.protect_memory(self.guest, self.bytes as u64, if enabled { 0 } else { 7 })
            .map_err(|error| format!("RAM block permissions: {error:?}"))
    }
    /// Replace the mapping only after a verified compressed reference exists.
    /// Original file bytes remain until reclaim_file; this avoids disk I/O while paused.
    /// # Safety
    /// Requires paused CPUs and drained devices, exclusive ownership of the file
    /// range, and no concurrent snapshot/offload users. Failure is fail-stop.
    pub unsafe fn discard(&self) -> Result<(), String> {
        hvf::HvfVm {}.unmap_memory(self.guest, self.bytes as u64)
            .map_err(|error| format!("cold RAM unmap: {error:?}"))?;
        let result = unsafe { libc::mmap(self.host as *mut _, self.bytes, libc::PROT_NONE,
            libc::MAP_FIXED | libc::MAP_PRIVATE | libc::MAP_ANON, -1, 0) };
        if result == libc::MAP_FAILED { return Err(format!("cold RAM discard: {}", std::io::Error::last_os_error())); }
        Ok(())
    }
    /// Reclaim the detached original file range, without holding CPU/device barriers.
    /// # Safety
    /// The original mapping must have been replaced, and this file range must never be mapped again
    /// or used by a snapshot/offload reader. Restores must use private anonymous RAM.
    pub unsafe fn reclaim_file(&self) -> Result<(), String> {
        use std::os::fd::AsRawFd;
        let hole = libc::fpunchhole_t { fp_flags: 0, reserved: 0,
            fp_offset: self.offset as i64, fp_length: self.bytes as i64 };
        if unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_PUNCHHOLE, &hole) } != 0 {
            return Err(format!("cold RAM punch: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }
    /// # Safety
    /// Block must be inaccessible to CPUs and devices, with same-block restores
    /// serialized. Input must already pass the compressed storage checksum.
    pub unsafe fn restore(&self, input: &[u8]) -> Result<(), String> {
        if input.len() != self.bytes { return Err("RAM restore length mismatch".into()); }
        let result = unsafe { libc::mmap(self.host as *mut _, self.bytes,
            libc::PROT_READ | libc::PROT_WRITE, libc::MAP_FIXED | libc::MAP_PRIVATE | libc::MAP_ANON, -1, 0) };
        if result == libc::MAP_FAILED { return Err(format!("cold RAM allocate: {}", std::io::Error::last_os_error())); }
        // Anonymous mappings already read as zero; avoid dirtying private
        // physical pages just to rewrite verified all-zero contents.
        if input.iter().any(|byte| *byte != 0) {
            unsafe { std::ptr::copy_nonoverlapping(input.as_ptr(), self.host as *mut u8, self.bytes); }
        }
        extern "C" { fn sys_icache_invalidate(start: *mut libc::c_void, bytes: usize); }
        // Restored blocks can contain guest instructions. Publish code bytes
        // before executable stage-2 mappings become visible to other vCPUs.
        unsafe { sys_icache_invalidate(self.host as *mut _, self.bytes); }
        hvf::HvfVm {}.map_memory(self.host as u64, self.guest, self.bytes as u64)
            .map_err(|error| format!("cold RAM remap: {error:?}"))
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn blocks(memory: &GuestMemoryMmap, chunk: usize) -> Result<Vec<RamBlock>, String> {
    use vm_memory::Address;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page <= 0 || chunk == 0 || !chunk.is_multiple_of(page as usize) {
        return Err("invalid RAM sampling chunk".into());
    }
    let mut blocks = Vec::new();
    let owner = Arc::new(memory.clone());
    for region in memory.iter().filter(|region| region.file_offset().is_some()) {
        let backing = region.file_offset().unwrap();
        let file = Arc::new(backing.file().try_clone().map_err(|error| error.to_string())?);
        if !(region.len() as usize).is_multiple_of(chunk) { return Err("RAM sampling requires complete chunks".into()); }
        for offset in (0..region.len() as usize).step_by(chunk) {
            blocks.push(RamBlock { _memory: owner.clone(),
                guest: region.start_addr().raw_value() + offset as u64,
                host: region.as_ptr() as usize + offset, bytes: chunk,
                file: file.clone(), offset: backing.start() + offset as u64 });
        }
    }
    if blocks.is_empty() { return Err("no file-backed RAM".into()); }
    Ok(blocks)
}
