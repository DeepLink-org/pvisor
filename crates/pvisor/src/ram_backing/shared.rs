//! Linux physical-page owner. References pin immutable slots in a size-sealed
//! memfd; only the owner writes free slots. VMs receive a read-only descriptor.
use super::{ImageId, invalid, resident::identity};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{fs::FileExt, net::UnixStream},
    },
    sync::Arc,
};

const PAGE: usize = 4096;
pub struct SharedObject {
    id: ImageId,
    offset: u64,
    file: Arc<File>,
}
impl SharedObject {
    pub fn id(&self) -> ImageId {
        self.id
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }
    pub fn restore(&self, output: &mut [u8]) -> io::Result<()> {
        if output.len() != PAGE {
            return Err(invalid("shared page length mismatch"));
        }
        self.file.read_exact_at(output, self.offset)?;
        if identity(output) != self.id {
            return Err(invalid("shared page checksum mismatch"));
        }
        Ok(())
    }
}
// mmap is private to this owner and accessed only while SharedPool is locked.
struct ResidentMapping {
    address: *mut libc::c_void,
    len: usize,
}
unsafe impl Send for ResidentMapping {}
impl Drop for ResidentMapping {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.address, self.len);
        }
    }
}
pub struct SharedPool {
    file: File,
    readonly: Arc<File>,
    mapping: ResidentMapping,
    objects: BTreeMap<ImageId, Arc<SharedObject>>,
    candidates: BTreeMap<ImageId, [u8; 16]>,
    candidate_order: VecDeque<ImageId>,
    free: Vec<u64>,
    next: u64,
    slots: usize,
}
impl SharedPool {
    pub fn new(max_bytes: usize, max_objects: usize) -> io::Result<Self> {
        let slots = (max_bytes / PAGE).min(max_objects);
        let size = slots
            .checked_mul(PAGE)
            .filter(|v| *v != 0 && *v <= isize::MAX as usize)
            .ok_or_else(|| invalid("invalid shared pool budget"))?;
        let fd = unsafe {
            libc::memfd_create(
                c"pvisor-shared-ram".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        file.set_len(size as u64)?;
        // Seal size, not writes: the owner still publishes only unreferenced slots.
        if unsafe {
            libc::fcntl(
                fd,
                libc::F_ADD_SEALS,
                libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        let readonly = Arc::new(File::open(format!("/proc/self/fd/{fd}"))?);
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            file,
            readonly,
            mapping: ResidentMapping { address, len: size },
            objects: BTreeMap::new(),
            candidates: BTreeMap::new(),
            candidate_order: VecDeque::new(),
            free: Vec::new(),
            next: 0,
            slots,
        })
    }
    pub fn readonly_file(&self) -> Arc<File> {
        self.readonly.clone()
    }
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }
    pub fn payload_bytes(&self) -> usize {
        self.objects.len() * PAGE
    }
    /// Admit only after equal hashes occur in two VM sessions. Candidates hold
    /// no payload; full bytes are still compared before reusing a live slot.
    pub fn intern_duplicate(
        &mut self,
        bytes: &[u8],
        session: [u8; 16],
    ) -> io::Result<Arc<SharedObject>> {
        if bytes.len() != PAGE {
            return Err(invalid("physical pool requires a 4 KiB page"));
        }
        let id = identity(bytes);
        if self.objects.contains_key(&id) {
            return self.intern(bytes);
        }
        if self
            .candidates
            .get(&id)
            .is_some_and(|first| *first != session)
        {
            return self.intern(bytes);
        }
        if !self.candidates.contains_key(&id) {
            if self.candidates.len() >= self.slots.saturating_mul(4)
                && let Some(old) = self.candidate_order.pop_front()
            {
                self.candidates.remove(&old);
            }
            self.candidates.insert(id, session);
            self.candidate_order.push_back(id);
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "page has no cross-session duplicate yet",
        ))
    }
    pub fn forget_candidates(&mut self, session: [u8; 16]) {
        self.candidates.retain(|_, first| *first != session);
        self.candidate_order
            .retain(|id| self.candidates.contains_key(id));
    }
    pub fn intern(&mut self, bytes: &[u8]) -> io::Result<Arc<SharedObject>> {
        if bytes.len() != PAGE {
            return Err(invalid("physical pool requires a 4 KiB page"));
        }
        let id = identity(bytes);
        if let Some(object) = self.objects.get(&id) {
            let mut decoded = [0; PAGE];
            object.restore(&mut decoded)?;
            if decoded != bytes {
                return Err(invalid("shared page content identity collision"));
            }
            return Ok(object.clone());
        }
        let offset = if let Some(offset) = self.free.pop() {
            offset
        } else {
            if self.next / PAGE as u64 >= self.slots as u64 {
                return Err(io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "shared page pool budget exhausted",
                ));
            }
            let offset = self.next;
            self.next += PAGE as u64;
            offset
        };
        if let Err(error) = self.file.write_all_at(bytes, offset) {
            self.free.push(offset);
            return Err(error);
        }
        // Fault this owner's PTE too: complete-group PSS must include all pool
        // pages, even while a published page is not yet mapped by a VM.
        let mapped = unsafe {
            std::slice::from_raw_parts(
                (self.mapping.address as *const u8).add(offset as usize),
                PAGE,
            )
        };
        if mapped != bytes {
            return Err(invalid("shared page publication mismatch"));
        }
        let object = Arc::new(SharedObject {
            id,
            offset,
            file: self.readonly.clone(),
        });
        self.objects.insert(id, object.clone());
        Ok(object)
    }
    pub fn collect(&mut self) -> io::Result<()> {
        let dead: Vec<_> = self
            .objects
            .iter()
            .filter(|(_, obj)| Arc::strong_count(obj) == 1)
            .map(|(id, _)| *id)
            .collect();
        for id in dead {
            self.collect_one(id)?;
        }
        Ok(())
    }
    pub fn collect_one(&mut self, id: ImageId) -> io::Result<()> {
        if !self
            .objects
            .get(&id)
            .is_some_and(|obj| Arc::strong_count(obj) == 1)
        {
            return Ok(());
        }
        let offset = self.objects[&id].offset;
        // No VM mapping may survive its last explicit reference. Disconnected
        // sessions keep references until pidfd confirms all mappings are gone.
        if unsafe {
            libc::fallocate(
                self.file.as_raw_fd(),
                libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
                offset as i64,
                PAGE as i64,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        self.objects.remove(&id);
        self.free.push(offset);
        Ok(())
    }
}
pub(super) fn peer_pidfd(stream: &UnixStream) -> io::Result<OwnedFd> {
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut peer as *mut libc::ucred).cast(),
            &mut size,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if peer.uid != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "shared pool peer belongs to another user",
        ));
    }
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, peer.pid, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}
pub(super) fn wait_peer_exit(pidfd: &OwnedFd) -> io::Result<()> {
    let mut pfd = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let result = unsafe { libc::poll(&mut pfd, 1, -1) };
        if result > 0 && pfd.revents & libc::POLLIN != 0 {
            return Ok(());
        }
        if result < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(io::Error::other(
            "shared pool owner exit could not be verified",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn immutable_pages_are_reused_and_last_reference_releases_physical_storage() {
        let mut pool = SharedPool::new(2 * PAGE, 2).unwrap();
        let a = pool.intern(&[7; PAGE]).unwrap();
        let b = pool.intern(&[7; PAGE]).unwrap();
        assert_eq!(a.offset(), b.offset());
        assert_eq!(pool.payload_bytes(), PAGE);
        let id = a.id();
        drop(a);
        pool.collect_one(id).unwrap();
        let mut restored = [0; PAGE];
        b.restore(&mut restored).unwrap();
        assert_eq!(restored, [7; PAGE]);
        let offset = b.offset();
        drop(b);
        pool.collect_one(id).unwrap();
        assert_eq!(pool.object_count(), 0);
        let c = pool.intern(&[9; PAGE]).unwrap();
        assert_eq!(c.offset(), offset);
        c.restore(&mut restored).unwrap();
        assert_eq!(restored, [9; PAGE]);
        let readonly = pool.readonly_file();
        assert_eq!(
            unsafe { libc::fcntl(readonly.as_raw_fd(), libc::F_GETFL) } & libc::O_ACCMODE,
            libc::O_RDONLY
        );
        assert!(readonly.write_all_at(&[0; PAGE], 0).is_err());
        assert!(pool.file.set_len(PAGE as u64).is_err());
    }
    #[test]
    fn unique_candidates_use_no_page_slots_and_need_another_session() {
        let mut pool = SharedPool::new(PAGE, 1).unwrap();
        for _ in 0..3 {
            assert!(pool.intern_duplicate(&[7; PAGE], [1; 16]).is_err());
        }
        assert_eq!(pool.payload_bytes(), 0);
        let object = pool.intern_duplicate(&[7; PAGE], [2; 16]).unwrap();
        assert_eq!(pool.payload_bytes(), PAGE);
        assert!(pool.intern_duplicate(&[9; PAGE], [1; 16]).is_err());
        let mut restored = [0; PAGE];
        object.restore(&mut restored).unwrap();
        assert_eq!(restored, [7; PAGE]);
        pool.forget_candidates([1; 16]);
        assert!(pool.candidates.is_empty());
        for n in 10..20 {
            assert!(pool.intern_duplicate(&[n; PAGE], [3; 16]).is_err());
        }
        assert_eq!(pool.candidates.len(), 4);
        assert!(pool.candidates.contains_key(&identity(&[19; PAGE])));
    }
    #[test]
    fn rejected_new_pages_do_not_damage_existing_shared_pages() {
        let mut pool = SharedPool::new(PAGE, 1).unwrap();
        let a = pool.intern(&[3; PAGE]).unwrap();
        assert!(pool.intern(&[4; PAGE]).is_err());
        let b = pool.intern(&[3; PAGE]).unwrap();
        let mut bytes = [0; PAGE];
        b.restore(&mut bytes).unwrap();
        assert_eq!(bytes, [3; PAGE]);
        assert_eq!(a.offset(), b.offset());
    }
}
