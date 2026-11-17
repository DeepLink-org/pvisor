//! Pack allocated extents of a completed, private sparse capture after thaw.
//! The manifest binds both the restore layout and the packed file identity.
#[cfg(target_os = "linux")]
use anyhow::ensure;
use pvisor_vm::api::RamRestoreExtent;
use std::{fs::File, path::Path};

#[cfg(target_os = "linux")]
pub(super) fn pack(path: &Path, captured: File) -> anyhow::Result<(File, Vec<RamRestoreExtent>)> {
    use std::{
        io::{self, Read, Seek, SeekFrom},
        os::fd::AsRawFd,
    };
    let len = captured.metadata()?.len();
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    ensure!(
        page > 0 && len <= i64::MAX as u64,
        "invalid branch RAM geometry"
    );
    let fd = captured.as_raw_fd();
    let mut pos = 0;
    let mut packed = 0;
    let mut extents = Vec::new();
    while pos < len {
        let next = unsafe { libc::lseek(fd, pos as i64, libc::SEEK_DATA) };
        let data = if next < 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::ENXIO) => len,
                Some(libc::EINVAL | libc::EOPNOTSUPP | libc::ENOTTY | libc::ENOSYS) => {
                    return Ok((captured, Vec::new()));
                }
                _ => return Err(error.into()),
            }
        } else {
            next as u64
        };
        ensure!(pos <= data && data <= len, "invalid captured data extent");
        if data > pos {
            extents.push(RamRestoreExtent {
                offset: pos,
                length: data - pos,
                backing_offset: None,
            });
        }
        if data == len {
            break;
        }
        let hole = unsafe { libc::lseek(fd, data as i64, libc::SEEK_HOLE) };
        if hole < 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::EINVAL | libc::EOPNOTSUPP | libc::ENOTTY | libc::ENOSYS) => {
                    Ok((captured, Vec::new()))
                }
                _ => Err(error.into()),
            };
        }
        pos = hole as u64;
        ensure!(data < pos && pos <= len, "invalid captured hole extent");
        extents.push(RamRestoreExtent {
            offset: data,
            length: pos - data,
            backing_offset: Some(packed),
        });
        packed += pos - data;
        if extents.len() > 8192 {
            return Ok((captured, Vec::new()));
        }
    }
    if packed == len
        || extents.len() > 8192
        || extents
            .iter()
            .any(|e| !e.length.is_multiple_of(page as u64))
    {
        return Ok((captured, Vec::new()));
    }

    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().expect("RAM file has a parent"))?;
    let mut reader = &captured;
    unsafe { libc::posix_fadvise(fd, 0, 0, libc::POSIX_FADV_RANDOM) };
    for extent in extents.iter().filter(|e| e.backing_offset.is_some()) {
        reader.seek(SeekFrom::Start(extent.offset))?;
        let n = io::copy(&mut reader.by_ref().take(extent.length), tmp.as_file_mut())?;
        ensure!(n == extent.length, "short read of captured RAM extent");
    }
    tmp.as_file().sync_all()?;
    Ok((tmp.persist(path)?, extents))
}

#[cfg(not(target_os = "linux"))]
pub(super) fn pack(_path: &Path, captured: File) -> anyhow::Result<(File, Vec<RamRestoreExtent>)> {
    Ok((captured, Vec::new()))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::FileExt;

    #[test]
    fn packed_bytes_and_zero_ranges_reconstruct_the_entire_capture() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ram");
        let file = File::create_new(&path).unwrap();
        let length = 8 * 1024 * 1024;
        file.set_len(length).unwrap();
        file.write_all_at(b"first", 4093).unwrap();
        file.write_all_at(b"last", length - 9).unwrap();
        file.sync_all().unwrap();
        let original = std::fs::read(&path).unwrap();
        let (packed, extents) = pack(&path, file).unwrap();
        if extents.is_empty() {
            // Some filesystems legitimately report the whole file as data.
            assert_eq!(std::fs::read(path).unwrap(), original);
            return;
        }
        let mut restored = vec![0; length as usize];
        let mut logical = 0;
        let mut offset = 0;
        for e in extents {
            assert_eq!(e.offset, logical);
            logical += e.length;
            if let Some(start) = e.backing_offset {
                assert_eq!(start, offset);
                packed
                    .read_exact_at(&mut restored[e.offset as usize..logical as usize], start)
                    .unwrap();
                offset += e.length;
            }
        }
        assert_eq!(logical, length);
        assert_eq!(offset, packed.metadata().unwrap().len());
        assert!(offset < length);
        assert_eq!(restored, original);
    }
}
