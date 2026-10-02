//! File-backed guest RAM. DAX/GPU windows are deliberately not RAM backing.

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
