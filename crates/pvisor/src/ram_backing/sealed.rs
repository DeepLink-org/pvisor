//! Linux immutable compressed objects. Each live object owns one sealed memfd;
//! clients retain the same inode, so restore never depends on the publisher.
use super::{
    BLOCK_BYTES, ImageId, invalid,
    resident::{CompressedObject, identity},
};
use std::{
    collections::BTreeMap,
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::FileExt,
    },
    sync::Arc,
};

const SEALS: i32 = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;

/// Immutable encoded RAM, independently owned by its file descriptor.
/// Decoding verifies the original content identity on every restore.
pub(super) struct SealedObject {
    file: File,
    id: ImageId,
    length: usize,
    frame_bytes: usize,
}
impl SealedObject {
    pub(super) fn receive(file: File, id: ImageId, bytes: &[u8]) -> io::Result<Self> {
        if unsafe { libc::sysconf(libc::_SC_PAGESIZE) } != 4096 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "sealed RAM pool requires 4 KiB host pages",
            ));
        }
        let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        let size = file.metadata()?.len();
        if seals < 0
            || seals & SEALS != SEALS
            || flags < 0
            || flags & libc::O_ACCMODE != libc::O_RDONLY
            || !(46..=(45 + BLOCK_BYTES) as u64).contains(&size)
            || bytes.len() != BLOCK_BYTES
            || (size as usize).div_ceil(4096) * 4096 + 256 > bytes.len() * 7 / 8
        {
            return Err(invalid(
                "encoded pool descriptor must be read-only, fully sealed and bounded",
            ));
        }
        let object = Self {
            file,
            id,
            length: bytes.len(),
            frame_bytes: size as usize,
        };
        let mut decoded = vec![0; bytes.len()];
        object.restore(&mut decoded)?;
        if decoded != bytes {
            return Err(invalid("encoded pool publication content mismatch"));
        }
        Ok(object)
    }
    pub(super) fn file(&self) -> &File {
        &self.file
    }
    pub(super) fn id(&self) -> ImageId {
        self.id
    }
    pub(super) fn length(&self) -> usize {
        self.length
    }
    // memfd pages, including frame header and page rounding. Index/FD overhead
    // is separately bounded by object and connection-reference limits.
    pub(super) fn charged_bytes(&self) -> usize {
        self.frame_bytes.div_ceil(4096) * 4096
    }
    pub(super) fn restore(&self, output: &mut [u8]) -> io::Result<()> {
        if output.len() != self.length {
            return Err(invalid("sealed object output length mismatch"));
        }
        let mut frame = vec![0; self.frame_bytes];
        self.file.read_exact_at(&mut frame, 0)?;
        let object = CompressedObject::from_frame(&frame)?;
        if object.id() != self.id || object.length() != self.length {
            return Err(invalid("sealed object identity mismatch"));
        }
        object.restore(output)
    }
}

/// Bounded daemon pool for 64 KiB cold blocks. Equal decoded bytes share one
/// immutable file across sessions. Unique blocks must save at least 1/8 after
/// page rounding and a conservative 256-byte metadata allowance. No raw fallback.
/// The caller serializes access and collects after releasing references.
pub struct SealedPool {
    objects: BTreeMap<ImageId, Arc<SealedObject>>,
    max_bytes: usize,
    max_objects: usize,
    bytes: usize,
}
impl SealedPool {
    /// Budgets bound resident file pages and object count independently; zero
    /// admits no objects. Publication requires 4 KiB host pages. Does not change
    /// host memory policy or resource limits.
    pub fn new(max_bytes: usize, max_objects: usize) -> Self {
        Self {
            objects: BTreeMap::new(),
            max_bytes,
            max_objects,
            bytes: 0,
        }
    }
    pub(super) fn intern(&mut self, bytes: &[u8]) -> io::Result<Arc<SealedObject>> {
        if unsafe { libc::sysconf(libc::_SC_PAGESIZE) } != 4096 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "sealed RAM pool requires 4 KiB host pages",
            ));
        }
        if bytes.len() != BLOCK_BYTES {
            return Err(invalid("sealed pool requires 64 KiB blocks"));
        }
        let id = identity(bytes);
        if let Some(object) = self.objects.get(&id) {
            let mut decoded = vec![0; bytes.len()];
            object.restore(&mut decoded)?;
            if decoded != bytes {
                return Err(invalid("sealed pool content identity collision"));
            }
            return Ok(object.clone());
        }
        let frame = CompressedObject::from_bytes(bytes)?.frame();
        let charged = frame.len().div_ceil(4096) * 4096;
        if charged + 256 > bytes.len() * 7 / 8 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "insufficient compression savings",
            ));
        }
        if self.objects.len() >= self.max_objects
            || charged > self.max_bytes.saturating_sub(self.bytes)
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "sealed pool budget exhausted",
            ));
        }
        let fd = unsafe {
            libc::memfd_create(
                c"pvisor-encoded-ram".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        file.write_all_at(&frame, 0)?;
        if unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, SEALS) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let readonly = File::open(format!("/proc/self/fd/{fd}"))?;
        let object = Arc::new(SealedObject::receive(readonly, id, bytes)?);
        self.bytes += charged;
        self.objects.insert(id, object.clone());
        Ok(object)
    }
    pub(super) fn collect_one(&mut self, id: ImageId) {
        if self
            .objects
            .get(&id)
            .is_some_and(|o| Arc::strong_count(o) == 1)
        {
            self.bytes -= self.objects.remove(&id).unwrap().charged_bytes();
        }
    }
    pub(super) fn collect(&mut self) {
        let dead: Vec<_> = self.objects.keys().copied().collect();
        for id in dead {
            self.collect_one(id);
        }
    }
    pub(super) fn stats(&self) -> (u64, u64) {
        (self.bytes as u64, self.objects.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn noise() -> Vec<u8> {
        let mut state = 0x71a582e3u32;
        (0..BLOCK_BYTES)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect()
    }

    #[test]
    fn sealed_pool_deduplicates_compressed_blocks_and_accounts_file_pages() {
        let mut pool = SealedPool::new(8192, 2);
        for bytes in [
            vec![7; BLOCK_BYTES],
            (0..BLOCK_BYTES).map(|i| (i % 251) as u8).collect(),
        ] {
            let a = pool.intern(&bytes).unwrap();
            let b = pool.intern(&bytes).unwrap();
            assert!(Arc::ptr_eq(&a, &b));
            assert_eq!(
                a.file.metadata().unwrap().ino(),
                b.file.metadata().unwrap().ino()
            );
            assert_eq!(a.charged_bytes(), 4096);
            let mut output = vec![0; BLOCK_BYTES];
            b.restore(&mut output).unwrap();
            assert_eq!(output, bytes);
            assert!(a.file.write_all_at(b"!", 0).is_err());
            let writable = std::fs::OpenOptions::new()
                .write(true)
                .open(format!("/proc/self/fd/{}", a.file.as_raw_fd()))
                .unwrap();
            assert!(writable.write_all_at(b"!", 0).is_err());
            assert!(writable.set_len(0).is_err());
            pool.collect();
            assert_eq!(pool.stats(), (4096, 1));
            drop(a);
            drop(b);
            pool.collect();
            assert_eq!(pool.stats(), (0, 0));
        }
    }

    #[test]
    fn sealed_pool_rejects_raw_short_and_over_budget_without_mutation() {
        let mut pool = SealedPool::new(4096, 1);
        assert!(pool.intern(&[1; 4096]).is_err());
        assert_eq!(
            pool.intern(&noise()).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(pool.stats(), (0, 0));
        let first = pool.intern(&vec![1; BLOCK_BYTES]).unwrap();
        assert_eq!(
            pool.intern(&vec![2; BLOCK_BYTES]).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(pool.stats(), (4096, 1));
        let id = first.id();
        drop(first);
        pool.collect_one(id);
        assert_eq!(pool.stats(), (0, 0));
        assert!(pool.intern(&vec![2; BLOCK_BYTES]).is_ok());
        for (bytes, objects) in [(4095, 1), (4096, 0)] {
            assert!(
                SealedPool::new(bytes, objects)
                    .intern(&vec![1; BLOCK_BYTES])
                    .is_err()
            );
        }
    }

    #[test]
    fn sealed_pool_compares_bytes_after_identity_hit_and_handles_outlive_owner() {
        let mut pool = SealedPool::new(8192, 2);
        let bytes = vec![3; BLOCK_BYTES];
        let object = pool.intern(&bytes).unwrap();
        let other = vec![4; BLOCK_BYTES];
        // Force a digest collision in the private index to exercise full comparison.
        pool.objects.insert(identity(&other), object.clone());
        assert_eq!(
            pool.intern(&other).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        drop(pool);
        let mut output = vec![0; BLOCK_BYTES];
        object.restore(&mut output).unwrap();
        assert_eq!(output, bytes);
        assert!(object.restore(&mut output[..4096]).is_err());
    }

    #[test]
    fn sealed_delivery_rejects_mutable_writable_forged_and_oversized_backing() {
        fn file(frame: &[u8], seals: i32, writable: bool) -> File {
            let fd = unsafe {
                libc::memfd_create(
                    c"pvisor-sealed-invalid".as_ptr(),
                    libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
                )
            };
            assert!(fd >= 0);
            let file = unsafe { File::from_raw_fd(fd) };
            file.write_all_at(frame, 0).unwrap();
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, seals) }, 0);
            if writable {
                file
            } else {
                File::open(format!("/proc/self/fd/{fd}")).unwrap()
            }
        }
        let bytes = vec![9; BLOCK_BYTES];
        let frame = CompressedObject::from_bytes(&bytes).unwrap().frame();
        let id = identity(&bytes);
        for (seals, writable) in [
            (0, false),
            (libc::F_SEAL_GROW | libc::F_SEAL_SHRINK, false),
            (SEALS, true),
        ] {
            assert!(SealedObject::receive(file(&frame, seals, writable), id, &bytes).is_err());
        }
        let mut bad = frame.clone();
        bad[45] ^= 1;
        assert!(SealedObject::receive(file(&bad, SEALS, false), id, &bytes).is_err());
        let mut bad = frame.clone();
        bad[8] ^= 1;
        assert!(SealedObject::receive(file(&bad, SEALS, false), id, &bytes).is_err());
        assert!(
            SealedObject::receive(file(&vec![0; BLOCK_BYTES + 46], SEALS, false), id, &bytes)
                .is_err()
        );
        let mut raw = b"PVBLK1\0\0".to_vec();
        raw.extend_from_slice(&id);
        raw.extend_from_slice(&(BLOCK_BYTES as u32).to_le_bytes());
        raw.push(1);
        raw.extend_from_slice(&bytes);
        assert!(SealedObject::receive(file(&raw, SEALS, false), id, &bytes).is_err());
    }
}
