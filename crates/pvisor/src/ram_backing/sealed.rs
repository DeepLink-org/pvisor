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
pub struct SealedObject {
    file: File,
    id: ImageId,
    length: usize,
    frame_bytes: usize,
}
impl SealedObject {
    pub(super) fn receive(file: File, id: ImageId, bytes: &[u8]) -> io::Result<Self> {
        let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        let size = file.metadata()?.len();
        if seals < 0
            || seals & SEALS != SEALS
            || flags < 0
            || flags & libc::O_ACCMODE != libc::O_RDONLY
            || !(46..=(45 + BLOCK_BYTES) as u64).contains(&size)
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
    /// admits no objects. Does not change host memory policy or resource limits.
    pub fn new(max_bytes: usize, max_objects: usize) -> Self {
        Self {
            objects: BTreeMap::new(),
            max_bytes,
            max_objects,
            bytes: 0,
        }
    }
    pub(super) fn intern(&mut self, bytes: &[u8]) -> io::Result<Arc<SealedObject>> {
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
