use super::{RamDeltaCapture, RamDeltaSpec, RamMappingSnapshot};
use std::{fs::File, os::unix::fs::FileExt};
use vm_memory::{Address, Bytes, GuestAddress, GuestMemory, GuestMemoryMmap, GuestMemoryRegion};
#[cfg(test)]
use vm_memory::{FileOffset, GuestRegionMmap, mmap::MmapRegionBuilder};

// The caller holds the full-machine freeze. Reading a shared backing file sees
// guest writes without faulting untouched tmpfs holes into the live RAM map.
// Private mappings must instead be read through memory to include COW writes.
pub(super) fn capture_ram(
    memory: &GuestMemoryMmap,
    file: &File,
) -> Result<Vec<RamMappingSnapshot>, String> {
    let mut ram = Vec::new();
    let mut file_offset = 0u64;
    let mut buffer = vec![0; 1024 * 1024];
    for region in memory.iter() {
        let base = region.start_addr().raw_value();
        let len = region.len();
        ram.push(RamMappingSnapshot {
            base,
            len,
            file_offset,
        });
        let mut offset = 0;
        while offset < len {
            let count = (len - offset).min(buffer.len() as u64) as usize;
            if region.flags() & libc::MAP_SHARED != 0
                && region.flags() & libc::MAP_PRIVATE == 0
                && region.file_offset().is_some()
            {
                let backing = region.file_offset().unwrap();
                let start = backing
                    .start()
                    .checked_add(offset)
                    .ok_or("RAM backing offset overflow")?;
                backing
                    .file()
                    .read_exact_at(&mut buffer[..count], start)
                    .map_err(|e| e.to_string())?;
            } else {
                memory
                    .read_slice(&mut buffer[..count], GuestAddress(base + offset))
                    .map_err(|e| e.to_string())?;
            }
            // A fresh destination has zero-filled holes. Preserve logical
            // bytes and length while avoiding a resident copy of zero RAM.
            if buffer[..count].iter().any(|byte| *byte != 0) {
                file.write_all_at(&buffer[..count], file_offset + offset)
                    .map_err(|e| e.to_string())?;
            }
            offset += count as u64;
        }
        file_offset = file_offset
            .checked_add(len)
            .ok_or("RAM snapshot size overflow")?;
    }
    file.set_len(file_offset).map_err(|e| e.to_string())?;
    Ok(ram)
}

/// All CPUs and userspace device writers are frozen. Linux distinguishes
/// file-backed pages from private anonymous COW pages in pagemap. Swapped pages
/// are always copied. This does not use soft-dirty or KVM dirty logs, which alone
/// cannot account for every device write. A missing observation falls back to a
/// full capture before any destination writes have occurred.
pub(super) fn capture_ram_with_delta(
    memory: &GuestMemoryMmap,
    file: &File,
    baseline: Option<&RamDeltaSpec>,
) -> Result<(Vec<RamMappingSnapshot>, Option<RamDeltaCapture>), String> {
    if file.metadata().map_err(|error| error.to_string())?.len() != 0 {
        return Err("RAM snapshot destination must be empty".into());
    }
    let Some(baseline) = baseline else {
        return capture_ram(memory, file).map(|ram| (ram, None));
    };
    baseline.validate()?;
    #[cfg(target_os = "linux")]
    if let Ok(Some((ram, changed))) = cow_blocks(memory, baseline) {
        let mut buffer = vec![0; baseline.block_bytes as usize];
        for mapping in &ram {
            let mut offset = 0;
            while offset < mapping.len {
                let position = mapping.file_offset + offset;
                let index = (position / u64::from(baseline.block_bytes)) as usize;
                let count = (mapping.len - offset).min(
                    u64::from(baseline.block_bytes) - position % u64::from(baseline.block_bytes),
                ) as usize;
                if changed[index] {
                    memory
                        .read_slice(&mut buffer[..count], GuestAddress(mapping.base + offset))
                        .map_err(|e| e.to_string())?;
                    if buffer[..count].iter().any(|byte| *byte != 0) {
                        file.write_all_at(&buffer[..count], position)
                            .map_err(|e| e.to_string())?;
                    }
                }
                offset += count as u64;
            }
        }
        file.set_len(baseline.length).map_err(|e| e.to_string())?;
        let delta = RamDeltaCapture {
            version: 1,
            length: baseline.length,
            block_bytes: baseline.block_bytes,
            base_sha256: baseline.base_sha256.clone(),
            changed_blocks: changed
                .iter()
                .enumerate()
                .filter_map(|(i, changed)| changed.then_some(i as u64))
                .collect(),
        };
        delta.validate()?;
        return Ok((ram, Some(delta)));
    }
    capture_ram(memory, file).map(|ram| (ram, None))
}

#[cfg(target_os = "linux")]
fn cow_blocks(
    memory: &GuestMemoryMmap,
    baseline: &RamDeltaSpec,
) -> std::io::Result<Option<(Vec<RamMappingSnapshot>, Vec<bool>)>> {
    use std::{io, os::unix::fs::MetadataExt};
    use vm_memory::MemoryRegionAddress;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page <= 0 || !(baseline.block_bytes as u64).is_multiple_of(page as u64) {
        return Ok(None);
    }
    let page = page as u64;
    let mut ram = Vec::new();
    let mut position = 0u64;
    // Establish the complete baseline binding before examining page flags.
    for region in memory.iter() {
        if region.flags() & libc::MAP_PRIVATE == 0
            || region.flags() & (libc::MAP_SHARED | libc::MAP_HUGETLB) != 0
        {
            return Ok(None);
        }
        let Some(backing) = region.file_offset() else {
            return Ok(None);
        };
        let metadata = backing.file().metadata()?;
        if !metadata.is_file()
            || metadata.dev() != baseline.device
            || metadata.ino() != baseline.inode
            || metadata.len() != baseline.length
            || backing.start() != position
            || !region.len().is_multiple_of(page)
        {
            return Ok(None);
        }
        ram.push(RamMappingSnapshot {
            base: region.start_addr().raw_value(),
            len: region.len(),
            file_offset: position,
        });
        position = position
            .checked_add(region.len())
            .ok_or_else(|| io::Error::other("RAM size overflow"))?;
    }
    if position != baseline.length {
        return Ok(None);
    }
    let pagemap = File::open("/proc/self/pagemap")?;
    let mut changed =
        vec![false; baseline.length.div_ceil(u64::from(baseline.block_bytes)) as usize];
    let mut entries = vec![0; 4096 * 8];
    for (region, mapping) in memory.iter().zip(&ram) {
        let address = region
            .get_host_address(MemoryRegionAddress(0))
            .map_err(io::Error::other)? as u64;
        if !address.is_multiple_of(page) {
            return Ok(None);
        }
        let mut offset = 0;
        while offset < region.len() {
            let count = ((region.len() - offset) / page).min(4096) as usize;
            let start = (address
                .checked_add(offset)
                .ok_or_else(|| io::Error::other("RAM address overflow"))?
                / page)
                .checked_mul(8)
                .ok_or_else(|| io::Error::other("pagemap offset overflow"))?;
            pagemap.read_exact_at(&mut entries[..count * 8], start)?;
            for (index, bytes) in entries[..count * 8].as_chunks::<8>().0.iter().enumerate() {
                let entry = u64::from_ne_bytes(*bytes);
                if entry & ((1 << 58) | (1 << 59) | (1 << 60)) != 0 {
                    return Ok(None);
                }
                if entry & (1 << 62) != 0 || entry & (1 << 63) != 0 && entry & (1 << 61) == 0 {
                    let position = mapping.file_offset + offset + index as u64 * page;
                    changed[(position / u64::from(baseline.block_bytes)) as usize] = true;
                }
            }
            offset += count as u64 * page;
        }
    }
    Ok(Some((ram, changed)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::OpenOptions, os::unix::fs::MetadataExt, path::PathBuf};

    struct Artifact(PathBuf, File);

    impl Artifact {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let key = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "pvisor-ram-capture-{}-{time}-{key}",
                std::process::id()
            ));
            let file = OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            Self(path, file)
        }
    }

    impl Drop for Artifact {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn capture_shared_ram_preserves_holes_and_file_offsets_without_faulting_source() {
        let source = Artifact::new();
        let output = Artifact::new();
        let size = 8 * 1024 * 1024;
        let start = 1024 * 1024;
        source.1.set_len((size + start) as u64).unwrap();
        source.1.write_all_at(b"outside mapping", 0).unwrap();
        let memory = GuestMemoryMmap::from_ranges_with_files(&[(
            GuestAddress(0x1000),
            size,
            Some(FileOffset::new(source.1.try_clone().unwrap(), start as u64)),
        )])
        .unwrap();
        memory
            .write_slice(b"guest dirty", GuestAddress(0x1000 + 4096))
            .unwrap();
        let before = source.1.metadata().unwrap().blocks();
        let mappings = capture_ram(&memory, &output.1).unwrap();
        assert_eq!(
            source.1.metadata().unwrap().blocks(),
            before,
            "capture must not allocate untouched source holes"
        );
        assert_eq!(mappings[0].base, 0x1000);
        assert_eq!(mappings[0].file_offset, 0);
        assert_eq!(mappings[0].len, size as u64);
        assert_eq!(output.1.metadata().unwrap().len(), size as u64);
        assert!(output.1.metadata().unwrap().blocks() * 512 < size as u64 / 2);
        let mut bytes = vec![0; size];
        output.1.read_exact_at(&mut bytes, 0).unwrap();
        let mut expected = vec![0; size];
        expected[4096..4096 + 11].copy_from_slice(b"guest dirty");
        assert_eq!(bytes, expected);
    }

    #[test]
    fn capture_private_ram_keeps_cow_writes_and_cleared_bytes() {
        let source = Artifact::new();
        let output = Artifact::new();
        let size = 4 * 1024 * 1024;
        source.1.set_len(size as u64).unwrap();
        source.1.write_all_at(b"erase me", 0).unwrap();
        let region = MmapRegionBuilder::new(size)
            .with_file_offset(FileOffset::new(source.1.try_clone().unwrap(), 0))
            .with_mmap_prot(libc::PROT_READ | libc::PROT_WRITE)
            .with_mmap_flags(libc::MAP_PRIVATE)
            .build()
            .unwrap();
        let memory = GuestMemoryMmap::from_regions(vec![
            GuestRegionMmap::new(region, GuestAddress(0)).unwrap(),
        ])
        .unwrap();
        memory.write_slice(&[0; 8], GuestAddress(0)).unwrap();
        memory
            .write_slice(b"private", GuestAddress(2 * 1024 * 1024))
            .unwrap();
        capture_ram(&memory, &output.1).unwrap();
        let mut bytes = vec![0; size];
        output.1.read_exact_at(&mut bytes, 0).unwrap();
        let mut expected = vec![0; size];
        expected[2 * 1024 * 1024..2 * 1024 * 1024 + 7].copy_from_slice(b"private");
        assert_eq!(bytes, expected);
        let mut original = [0; 8];
        source.1.read_exact_at(&mut original, 0).unwrap();
        assert_eq!(&original, b"erase me");
        assert!(output.1.metadata().unwrap().blocks() * 512 < size as u64 / 2);
    }
    #[cfg(target_os = "linux")]
    fn baseline(file: &File) -> RamDeltaSpec {
        let meta = file.metadata().unwrap();
        RamDeltaSpec {
            device: meta.dev(),
            inode: meta.ino(),
            length: meta.len(),
            block_bytes: 64 * 1024,
            base_sha256: "a".repeat(64),
        }
    }

    #[cfg(target_os = "linux")]
    fn private_region(file: &File, start: u64, length: usize, address: u64) -> GuestRegionMmap {
        let region = MmapRegionBuilder::new(length)
            .with_file_offset(FileOffset::new(file.try_clone().unwrap(), start))
            .with_mmap_prot(libc::PROT_READ | libc::PROT_WRITE)
            .with_mmap_flags(libc::MAP_PRIVATE)
            .build()
            .unwrap();
        let result = GuestRegionMmap::new(region, GuestAddress(address)).unwrap();
        let pointer = result
            .get_host_address(vm_memory::MemoryRegionAddress(0))
            .unwrap();
        assert_eq!(
            unsafe { libc::madvise(pointer.cast(), length, libc::MADV_NOHUGEPAGE) },
            0
        );
        result
    }

    #[cfg(target_os = "linux")]
    fn present_pages(memory: &GuestMemoryMmap) -> usize {
        let file = File::open("/proc/self/pagemap").unwrap();
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
        memory
            .iter()
            .map(|region| {
                let address = region
                    .get_host_address(vm_memory::MemoryRegionAddress(0))
                    .unwrap() as u64;
                let mut entries = vec![0; region.len() as usize / page as usize * 8];
                file.read_exact_at(&mut entries, address / page * 8)
                    .unwrap();
                entries
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .filter(|bytes| u64::from_ne_bytes(**bytes) & (1 << 63) != 0)
                    .count()
            })
            .sum()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn delta_capture_keeps_zeroing_and_device_writes_without_faulting_clean_baseline_pages() {
        let source = Artifact::new();
        let output = Artifact::new();
        let size = 16 * 1024 * 1024;
        let original = vec![0x5a; size];
        source.1.write_all_at(&original, 0).unwrap();
        let memory =
            GuestMemoryMmap::from_regions(vec![private_region(&source.1, 0, size, 0x1000)])
                .unwrap();
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        memory
            .write_slice(&vec![0; 64 * 1024], GuestAddress(0x1000))
            .unwrap();
        memory
            .write_slice(
                b"userspace device write",
                GuestAddress(0x1000 + 5 * 64 * 1024 + page as u64),
            )
            .unwrap();
        let before = present_pages(&memory);
        let (mappings, delta) =
            capture_ram_with_delta(&memory, &output.1, Some(&baseline(&source.1))).unwrap();
        let delta = delta.expect("Linux private file mappings must support COW capture");
        assert_eq!(delta.changed_blocks, [0, 5]);
        assert_eq!(mappings[0].file_offset, 0);
        assert_eq!(output.1.metadata().unwrap().len(), size as u64);
        let after = present_pages(&memory);
        assert!(
            after <= before + 2 * 64 * 1024 / page,
            "capture faulted clean baseline: {before} -> {after}"
        );
        assert!(after < size / page / 8);
        assert!(output.1.metadata().unwrap().blocks() * 512 <= 128 * 1024);
        let mut reconstructed = original.clone();
        for block in delta.changed_blocks {
            let offset = block as usize * 64 * 1024;
            output
                .1
                .read_exact_at(
                    &mut reconstructed[offset..offset + 64 * 1024],
                    offset as u64,
                )
                .unwrap();
        }
        let mut live = vec![0; size];
        memory.read_slice(&mut live, GuestAddress(0x1000)).unwrap();
        assert_eq!(reconstructed, live);
        let mut unchanged = vec![0; size];
        source.1.read_exact_at(&mut unchanged, 0).unwrap();
        assert_eq!(unchanged, original);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn delta_capture_spans_split_guest_regions_and_partial_tail_blocks() {
        let source = Artifact::new();
        let output = Artifact::new();
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let original = vec![0x23; page * 5];
        source.1.write_all_at(&original, 0).unwrap();
        let memory = GuestMemoryMmap::from_regions(vec![
            private_region(&source.1, 0, page * 3, 0x1000),
            private_region(&source.1, (page * 3) as u64, page * 2, 0x100000),
        ])
        .unwrap();
        memory
            .write_slice(b"second region", GuestAddress(0x100000 + page as u64))
            .unwrap();
        let (mappings, delta) =
            capture_ram_with_delta(&memory, &output.1, Some(&baseline(&source.1))).unwrap();
        assert_eq!(delta.unwrap().changed_blocks, [0]);
        assert_eq!(mappings[1].file_offset, (page * 3) as u64);
        let mut actual = vec![0; page * 5];
        output.1.read_exact_at(&mut actual, 0).unwrap();
        let mut expected = original;
        expected[page * 4..page * 4 + 13].copy_from_slice(b"second region");
        assert_eq!(actual, expected);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unavailable_baseline_falls_back_to_full_capture_and_invalid_geometry_never_writes() {
        let source = Artifact::new();
        source.1.set_len(64 * 1024).unwrap();
        let memory =
            GuestMemoryMmap::from_regions(vec![private_region(&source.1, 0, 64 * 1024, 0)])
                .unwrap();
        memory.write_slice(b"private", GuestAddress(0)).unwrap();
        let mut binding = baseline(&source.1);
        binding.inode += 1;
        let output = Artifact::new();
        let (_, delta) = capture_ram_with_delta(&memory, &output.1, Some(&binding)).unwrap();
        assert!(delta.is_none());
        let mut bytes = vec![0; 64 * 1024];
        output.1.read_exact_at(&mut bytes, 0).unwrap();
        assert_eq!(&bytes[..7], b"private");
        binding.block_bytes = 3000;
        let rejected = Artifact::new();
        assert!(capture_ram_with_delta(&memory, &rejected.1, Some(&binding)).is_err());
        assert_eq!(rejected.1.metadata().unwrap().len(), 0);
        assert!(capture_ram_with_delta(&memory, &output.1, None).is_err());
    }
}
