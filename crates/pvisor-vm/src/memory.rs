//! Private implementations for the uniform RAM token API.
use crate::api::{RamAccess, RamBlock};
#[cfg(target_os = "macos")]
pub(crate) type Block = crate::vmm::ram::RamBlock;
#[cfg(not(target_os = "macos"))]
#[derive(Clone)]
pub(crate) enum Block {}
impl RamAccess for RamBlock {
    fn guest_address(&self) -> u64 {
        #[cfg(target_os = "macos")]
        {
            self.inner.guest_address()
        }
        #[cfg(not(target_os = "macos"))]
        {
            match self.inner {}
        }
    }
    fn length(&self) -> usize {
        #[cfg(target_os = "macos")]
        {
            self.inner.length()
        }
        #[cfg(not(target_os = "macos"))]
        {
            match self.inner {}
        }
    }
    /// # Safety
    /// All CPUs and devices must be quiescent; the block must be resident.
    unsafe fn snapshot(&self, output: &mut [u8]) -> std::io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            unsafe { self.inner.snapshot(output) }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = output;
            match self.inner {}
        }
    }
    /// # Safety
    /// Requires exclusive CPU/device quiescence and restored content before reenable.
    unsafe fn observe(&self, enabled: bool) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            unsafe { self.inner.observe(enabled) }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = enabled;
            match self.inner {}
        }
    }
    /// # Safety
    /// Requires verified persisted content, exclusive ownership and drained devices.
    unsafe fn discard(&self) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            unsafe { self.inner.discard() }
        }
        #[cfg(not(target_os = "macos"))]
        {
            match self.inner {}
        }
    }
    /// # Safety
    /// The original file range must be detached and must never be reused by a mapping.
    unsafe fn reclaim_file(&self) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            unsafe { self.inner.reclaim_file() }
        }
        #[cfg(not(target_os = "macos"))]
        {
            match self.inner {}
        }
    }
    /// # Safety
    /// Block must be inaccessible, with serialized restores and verified input bytes.
    unsafe fn restore(&self, input: &[u8]) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            unsafe { self.inner.restore(input) }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = input;
            match self.inner {}
        }
    }
}

/// Platform-independent sealed RAM mapper, shared by the API and native restore.
pub(crate) fn map_snapshot_ram(
    ram: &[crate::api::RamMappingSnapshot],
    file: std::sync::Arc<std::fs::File>,
    ranges: &[(vm_memory::GuestAddress, usize)],
) -> Result<vm_memory::GuestMemoryMmap, String> {
    use vm_memory::{
        mmap::MmapRegionBuilder, Address, FileOffset, GuestMemoryMmap, GuestRegionMmap,
    };
    if ram.is_empty() {
        return Err("empty RAM snapshot inventory".into());
    }
    let mut end = 0u64;
    let mut address_end = 0u64;
    for mapping in ram {
        if mapping.len == 0 || mapping.file_offset != end || mapping.base < address_end {
            return Err("invalid RAM mapping inventory".into());
        }
        end = end
            .checked_add(mapping.len)
            .ok_or("RAM file size overflow")?;
        address_end = mapping
            .base
            .checked_add(mapping.len)
            .ok_or("RAM address overflow")?;
    }
    if file.metadata().map_err(|e| e.to_string())?.len() != end {
        return Err("RAM snapshot file size mismatch".into());
    }
    if ranges.len() != ram.len()
        || ranges
            .iter()
            .zip(ram)
            .any(|((base, len), saved)| base.raw_value() != saved.base || *len as u64 != saved.len)
    {
        return Err("RAM snapshot topology mismatch".into());
    }
    // SAFETY: sysconf takes a constant selector and no pointers.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page <= 0 {
        return Err("cannot determine host page size".into());
    }
    let mut regions = Vec::with_capacity(ranges.len());
    for mapping in ram {
        if mapping.file_offset % page as u64 != 0 || mapping.len % page as u64 != 0 {
            return Err("RAM snapshot mappings must be host-page aligned".into());
        }
        let size = usize::try_from(mapping.len).map_err(|e| e.to_string())?;
        let region = MmapRegionBuilder::new(size)
            .with_file_offset(FileOffset::from_arc(file.clone(), mapping.file_offset))
            .with_mmap_prot(libc::PROT_READ | libc::PROT_WRITE)
            .with_mmap_flags(libc::MAP_PRIVATE)
            .build()
            .map_err(|e| e.to_string())?;
        regions.push(std::sync::Arc::new(
            GuestRegionMmap::new(region, vm_memory::GuestAddress(mapping.base))
                .ok_or("invalid RAM snapshot mapping")?,
        ));
    }
    GuestMemoryMmap::from_arc_regions(regions).map_err(|e| format!("{e:?}"))
}
