//! Linux missing-fault pager. No userspace-only UFFD, signal handler, or
//! accessed-bit heuristic: kernel faults must cover KVM and host device I/O.
//! Publication runs outside the barrier; byte comparison at commit rejects
//! writes made during publication. Read heat is learned only from refaults.
use crate::api::{ColdRamActivity, ColdRamOptions, ColdRamStore, VmmHandle};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use vm_memory::{GuestMemory, GuestMemoryMmap, GuestMemoryRegion};

const BLOCK: usize = 64 * 1024;
const BATCH_BYTES: usize = 4 * 1024 * 1024;
static PENDING: AtomicU64 = AtomicU64::new(0);
const API: u64 = 0xaa;
// Linux x86_64 _IOWR/_IOR ABI; this module is compiled only on that target.
const fn iowr(number: u64, size: u64) -> libc::c_ulong {
    (0xc000_0000 | (size << 16) | (0xaa << 8) | number) as libc::c_ulong
}
#[repr(C)]
struct Api {
    api: u64,
    features: u64,
    ioctls: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Range {
    start: u64,
    len: u64,
}
#[repr(C)]
struct Register {
    range: Range,
    mode: u64,
    ioctls: u64,
}
#[repr(C)]
struct Copy {
    dst: u64,
    src: u64,
    len: u64,
    mode: u64,
    copied: i64,
}

struct Uffd(OwnedFd);
impl Uffd {
    fn open() -> io::Result<Self> {
        let flags = libc::O_CLOEXEC | libc::O_NONBLOCK;
        // Deliberately omit UFFD_USER_MODE_ONLY: KVM get_user_pages and kernel
        // reads/writes must block rather than fail or bypass restoration.
        let raw = unsafe { libc::syscall(libc::SYS_userfaultfd, flags) };
        let fd = if raw >= 0 {
            unsafe { OwnedFd::from_raw_fd(raw as i32) }
        } else {
            let syscall_error = io::Error::last_os_error();
            let device = OpenOptions::new().read(true).write(true).open("/dev/userfaultfd")
                .map_err(|error| io::Error::new(error.kind(), format!(
                    "Linux cold RAM requires kernel-fault userfaultfd access: syscall: {syscall_error}; /dev/userfaultfd: {error}. Grant device access or CAP_SYS_PTRACE; no sysctl is changed")))?;
            // USERFAULTFD_IOC_NEW = _IO(0xaa, 0). The device permission is the
            // authority for kernel-fault handling; no process-wide policy change.
            let raw = unsafe { libc::ioctl(device.as_raw_fd(), 0xaa00 as _, flags) };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            unsafe { OwnedFd::from_raw_fd(raw) }
        };
        let result = Self(fd);
        let mut api = Api {
            api: API,
            features: 0,
            ioctls: 0,
        };
        result.ioctl(iowr(0x3f, 24), &mut api)?;
        if api.api != API || api.ioctls & 3 != 3 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "UFFD API lacks register/unregister",
            ));
        }
        Ok(result)
    }
    fn ioctl<T>(&self, request: libc::c_ulong, value: &mut T) -> io::Result<()> {
        if unsafe { libc::ioctl(self.0.as_raw_fd(), request as _, value) } < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    fn register(&self, range: Range) -> io::Result<()> {
        let mut registration = Register {
            range,
            mode: 1,
            ioctls: 0,
        };
        self.ioctl(iowr(0, 32), &mut registration)?;
        if registration.ioctls & ((1 << 2) | (1 << 3)) != (1 << 2) | (1 << 3) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "UFFD range lacks COPY/WAKE",
            ));
        }
        Ok(())
    }
    fn restore(&self, range: Range, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() as u64 != range.len {
            return Err(io::Error::other("UFFD restore length mismatch"));
        }
        let mut copy = Copy {
            dst: range.start,
            src: bytes.as_ptr() as u64,
            len: range.len,
            mode: 1,
            copied: 0,
        }; // DONTWAKE until the entire block exists.
        self.ioctl(iowr(3, 40), &mut copy)?;
        if copy.copied != range.len as i64 {
            return Err(io::Error::other(format!(
                "incomplete UFFD COPY: {} of {}",
                copy.copied, range.len
            )));
        }
        self.wake(range)
    }
    // A missing page with no published cold object is an untouched anonymous
    // zero page. Materialize only the faulting 4 KiB, preserving sparse RAM and
    // every neighboring resident page. Queued faults may race an earlier copy.
    fn zero_page(&self, address: u64) -> io::Result<()> {
        let range = Range {
            start: address & !4095,
            len: 4096,
        };
        let zero = [0u8; 4096];
        match self.restore(range, &zero) {
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => self.wake(range),
            result => result,
        }
    }

    fn wake(&self, mut range: Range) -> io::Result<()> {
        self.ioctl(0x8010_aa02, &mut range)
    }
    fn event(&self) -> io::Result<Option<u64>> {
        let mut poll = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut poll, 1, 100) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(error);
        }
        if ready == 0 {
            return Ok(None);
        }
        if poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(io::Error::other("UFFD poll failed"));
        }
        // uffd_msg is a packed 32-byte record; parse bytes without unaligned refs.
        let mut message = [0u8; 32];
        let read = unsafe {
            libc::read(
                self.0.as_raw_fd(),
                message.as_mut_ptr().cast(),
                message.len(),
            )
        };
        if read < 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Ok(None);
            }
            return Err(error);
        }
        if read != 32 || message[0] != 0x12 {
            return Err(io::Error::other("unexpected UFFD event"));
        }
        let flags = u64::from_ne_bytes(message[8..16].try_into().unwrap());
        if flags & !1 != 0 {
            return Err(io::Error::other("non-missing UFFD fault"));
        }
        Ok(Some(u64::from_ne_bytes(
            message[16..24].try_into().unwrap(),
        )))
    }
}

struct Page<O> {
    range: Range,
    // Most configured guest pages are not cold objects. Reserve only a pointer
    // for those pages; allocate authority/checksum when reclamation succeeds.
    cold: Option<Box<(O, [u8; 32])>>,
    eligible_at: Instant,
}
struct Pager<S: ColdRamStore> {
    // Pins every pointer, even while VM/device owners are shutting down.
    memory: GuestMemoryMmap,
    pages: Vec<Page<S::Object>>,
    store: Arc<Mutex<S>>,
    uffd: Arc<Uffd>,
    cursor: usize,
    restored: u64,
    discarded: u64,
    rejected: u64,
    put_rejections: u64,
    restore_count: u64,
    restore_total_us: u64,
    restore_max_us: u64,
}
fn host_sorted_pages<O>(mappings: &[Range], eligible_at: Instant, block_bytes: usize) -> Vec<Page<O>> {
    let mut pages: Vec<_> = mappings
        .iter()
        .flat_map(|mapping| {
            (0..mapping.len).step_by(block_bytes).map(move |offset| Page {
                range: Range {
                    start: mapping.start + offset,
                    len: (mapping.len - offset).min(block_bytes as u64),
                },
                cold: None,
                eligible_at,
            })
        })
        .collect();
    pages.sort_unstable_by_key(|page| page.range.start);
    pages
}

struct Snapshot {
    index: usize,
    bytes: Vec<u8>,
}
struct Batch(Vec<Snapshot>);
impl Drop for Batch {
    fn drop(&mut self) {
        let bytes: usize = self.0.iter().map(|snapshot| snapshot.bytes.len()).sum();
        PENDING.fetch_sub(bytes as u64, Ordering::SeqCst);
    }
}
// Inspect residency without reading RAM or creating missing-fault events.
// The sample barrier rechecks this predicate before copying under the pager lock.
fn fully_resident(range: Range) -> io::Result<bool> {
    let mut pages = [0u8; BLOCK / 4096];
    let count = (range.len as usize).div_ceil(4096);
    if count > pages.len() {
        return Err(io::Error::other("cold RAM block exceeds residency buffer"));
    }
    if unsafe {
        libc::mincore(
            range.start as *mut libc::c_void,
            range.len as usize,
            pages.as_mut_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(pages[..count].iter().all(|byte| byte & 1 != 0))
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn fatal(error: impl std::fmt::Display) -> ! {
    eprintln!("Linux cold RAM failure; terminate runner: {error}");
    std::process::exit(1)
}

#[cfg(test)]
fn ranges(memory: &GuestMemoryMmap, device_start: u64) -> io::Result<Vec<Range>> {
    ranges_excluding_kernel(memory, device_start, None)
}

fn ranges_excluding_kernel(
    memory: &GuestMemoryMmap,
    device_start: u64,
    kernel: Option<crate::vmm::RawKernelMapping>,
) -> io::Result<Vec<Range>> {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page != 4096 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Linux cold RAM requires 4 KiB host pages",
        ));
    }
    let mut result = Vec::new();
    let mut kernel_seen = kernel.is_none();
    for region in memory.iter() {
        // build_raw has no mmap flags/protection metadata. Exclude exactly the
        // builder-authorized firmware pointer and topology, never arbitrary raw
        // or unsuitable mappings. File-backed/restored kernel RAM is not exempt.
        if kernel.is_some_and(|kernel| {
            region.start_addr().0 == kernel.guest_address
                && region.as_ptr() as usize == kernel.host_address
                && region.len() == kernel.length as u64
                && region.flags() == 0
                && region.prot() == 0
                && region.file_offset().is_none()
        }) {
            kernel_seen = true;
            continue;
        }
        if region
            .start_addr()
            .0
            .checked_add(region.len())
            .is_none_or(|end| end > device_start)
        {
            continue; // Device/DAX windows are never pager candidates.
        }
        if region.file_offset().is_some()
            || region.flags() & (libc::MAP_SHARED | libc::MAP_HUGETLB) != 0
            || region.flags() & (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS)
                != libc::MAP_PRIVATE | libc::MAP_ANONYMOUS
            || region.prot() & (libc::PROT_READ | libc::PROT_WRITE)
                != libc::PROT_READ | libc::PROT_WRITE
            || !(region.as_ptr() as usize).is_multiple_of(page as usize)
            || !region.len().is_multiple_of(page as u64)
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Linux cold RAM requires private anonymous writable ordinary RAM; file-backed/COW/shared/hugetlb RAM is unsupported",
            ));
        }
        result.push(Range {
            start: region.as_ptr() as u64,
            len: region.len(),
        });
    }
    if !kernel_seen {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Linux cold RAM trusted raw kernel mapping identity/topology mismatch",
        ));
    }
    if result.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no eligible Linux cold RAM mappings",
        ));
    }
    Ok(result)
}
// The callback owns the actual VM barrier; empty work must never enter it.
fn quiesce_nonempty<T, R>(
    work: &mut [T],
    action: impl FnOnce(&mut [T]) -> Result<Option<R>, String>,
) -> Result<Option<R>, String> {
    if work.is_empty() {
        Ok(None)
    } else {
        action(work)
    }
}

fn publish_snapshot<S: ColdRamStore>(
    pager: &Mutex<Pager<S>>,
    pool: &Mutex<S>,
    snapshot: &Snapshot,
) -> io::Result<Option<S::Object>> {
    let object = {
        let mut store = pool
            .lock()
            .map_err(|_| io::Error::other("store poisoned"))?;
        match store.put(&snapshot.bytes) {
            Ok(object) => Some(object),
            Err(error) => {
                // A healthy capacity rejection preserves RAM; a broken store
                // must not silently strand references to existing cold objects.
                store.stats().map_err(|_| error)?;
                None
            }
        }
    };
    // Fault recovery locks pager before store. Release store before taking
    // pager here, including the incompressible/capacity rejection path.
    if object.is_none() {
        pager
            .lock()
            .map_err(|_| io::Error::other("pager poisoned"))?
            .pages[snapshot.index]
            .eligible_at = Instant::now() + Duration::from_secs(30);
    }
    Ok(object)
}

impl<S: ColdRamStore> Pager<S> {
    fn candidates(&mut self) -> Vec<usize> {
        let mut candidates = Vec::new();
        let mut candidate_bytes = 0;
        let now = Instant::now();
        for _ in 0..self.pages.len() {
            if candidate_bytes >= BATCH_BYTES || now.elapsed() >= Duration::from_millis(8) {
                break;
            }
            let index = self.cursor;
            self.cursor = (self.cursor + 1) % self.pages.len();
            let page = &self.pages[index];
            if page.cold.is_some() || now < page.eligible_at {
                continue;
            }
            if fully_resident(page.range).unwrap_or_else(|error| fatal(error)) {
                candidates.push(index);
                candidate_bytes += page.range.len as usize;
            }
        }
        candidates
    }

    #[cfg(test)]
    fn sample(&mut self) -> Batch {
        let candidates = self.candidates();
        self.sample_candidates(&candidates)
    }

    fn sample_candidates(&mut self, candidates: &[usize]) -> Batch {
        let mut batch = Batch(Vec::new());
        let now = Instant::now();
        for &index in candidates {
            if now.elapsed() >= Duration::from_millis(8) {
                break;
            }
            let page = &self.pages[index];
            if page.cold.is_some() || now < page.eligible_at {
                continue;
            }
            // Never copy a sparse block while holding pager state: that would
            // fault and wait for the resolver, which needs this same lock.
            if !fully_resident(page.range).unwrap_or_else(|error| fatal(error)) {
                continue;
            }
            // Metadata selection needs no barrier, but copying live RAM does:
            // CPUs must be parked and device leases drained to avoid concurrent
            // writes. This block was verified resident under the barrier.
            let bytes = unsafe {
                std::slice::from_raw_parts(page.range.start as *const u8, page.range.len as usize)
            }
            .to_vec();
            PENDING.fetch_add(bytes.len() as u64, Ordering::SeqCst);
            batch.0.push(Snapshot { index, bytes });
        }
        batch
    }
    fn commit(&mut self, snapshot: &Snapshot, object: S::Object) -> io::Result<Option<S::Object>> {
        let page = &mut self.pages[snapshot.index];
        if page.cold.is_some() {
            return Ok(Some(object));
        }
        let live = unsafe {
            std::slice::from_raw_parts(page.range.start as *const u8, page.range.len as usize)
        };
        if live != snapshot.bytes {
            page.eligible_at = Instant::now() + Duration::from_secs(30);
            self.rejected += 1;
            return Ok(Some(object));
        }
        // Store the immutable reference and checksum before any destructive syscall.
        page.cold = Some(Box::new((object, digest(&snapshot.bytes))));
        if unsafe {
            libc::madvise(
                page.range.start as *mut libc::c_void,
                page.range.len as usize,
                libc::MADV_DONTNEED,
            )
        } != 0
        {
            // A partially successful discard cannot be rolled back by CPU memcpy.
            // Keep authority intact and terminate the runner on any mapping error.
            return Err(io::Error::last_os_error());
        }
        self.discarded += page.range.len;
        Ok(None)
    }
    fn restore(&mut self, index: usize) -> io::Result<()> {
        let page = &mut self.pages[index];
        let Some(cold) = &page.cold else {
            return self.uffd.wake(page.range);
        };
        let (object, expected) = cold.as_ref();
        // Service time includes store lock/restore, validation, COPY and release,
        // but excludes kernel/UFFD queue wait before entering this method.
        let started = Instant::now();
        let mut bytes = vec![0; page.range.len as usize];
        let mut store = self
            .store
            .lock()
            .map_err(|_| io::Error::other("cold store poisoned"))?;
        store.restore(object, &mut bytes)?;
        if digest(&bytes) != *expected {
            return Err(io::Error::other("cold RAM restore checksum mismatch"));
        }
        self.uffd.restore(page.range, &bytes)?;
        let (object, _) = *page.cold.take().unwrap();
        page.eligible_at = Instant::now() + Duration::from_secs(30);
        self.restored += page.range.len;
        store.release(object)?;
        let elapsed = started.elapsed().as_micros() as u64;
        self.restore_count += 1;
        self.restore_total_us += elapsed;
        self.restore_max_us = self.restore_max_us.max(elapsed);
        Ok(())
    }
    fn fault_index(&self, address: u64) -> Option<usize> {
        // Pages are sorted by host address, not GuestMemory's guest order.
        let index = self
            .pages
            .partition_point(|page| page.range.start <= address);
        let index = index.checked_sub(1)?;
        let range = self.pages[index].range;
        (address - range.start < range.len).then_some(index)
    }

    fn fault(&mut self, address: u64) -> io::Result<()> {
        let index = self
            .fault_index(address)
            .ok_or_else(|| io::Error::other("UFFD fault outside owned RAM"))?;
        if self.pages[index].cold.is_none() {
            self.uffd.zero_page(address)
        } else {
            self.restore(index)
        }
    }
    fn shutdown(&mut self) -> io::Result<()> {
        for index in 0..self.pages.len() {
            if self.pages[index].cold.is_some() {
                self.restore(index)?;
            }
        }
        // Never close UFFD over discarded data, even when VM destruction has
        // begun: outstanding device owners may still hold GuestMemory clones.
        for region in self.memory.iter() {
            if self
                .pages
                .iter()
                .any(|page| page.range.start == region.as_ptr() as u64)
            {
                let mut range = Range {
                    start: region.as_ptr() as u64,
                    len: region.len(),
                };
                self.uffd.ioctl(0x8010_aa01, &mut range)?;
            }
        }
        Ok(())
    }
}

pub(crate) fn activity() -> ColdRamActivity {
    ColdRamActivity {
        pending_file_bytes: 0,
        pending_snapshot_bytes: PENDING.load(Ordering::SeqCst),
    }
}
pub(crate) fn start<S: ColdRamStore + 'static>(
    handle: VmmHandle,
    store: S,
    options: ColdRamOptions,
) -> io::Result<()> {
    let block_bytes = if options.page_granular { 4096 } else { BLOCK };
    if cfg!(any(
        feature = "tee",
        feature = "aws-nitro",
        feature = "gpu",
        feature = "snd",
        feature = "input"
    )) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Linux cold RAM unavailable with unguarded devices or confidential RAM",
        ));
    }
    let (memory, mappings, kernel) = {
        let _transition = handle
            .transition
            .lock()
            .map_err(|_| io::Error::other("VM transition lock poisoned"))?;
        let vmm = handle
            .vmm
            .upgrade()
            .ok_or_else(|| io::Error::other("VMM has stopped"))?;
        let locked = vmm
            .lock()
            .map_err(|_| io::Error::other("VMM lock poisoned"))?;
        if locked.is_paused()
            || locked.device_memory_gate().has_prepare()
            || locked.device_memory_gate().has_dedup_advice()
        {
            return Err(io::Error::other(
                "Linux cold RAM requires running VM without device preparation or RAM dedup advice",
            ));
        }
        let memory = locked.guest_memory().clone();
        let kernel = locked.raw_kernel_mapping();
        let mappings = ranges_excluding_kernel(&memory, locked.ram_device_window_start(), kernel)?;
        handle
            .cold_pager_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "cold RAM pager already started",
                )
            })?;
        (memory, mappings, kernel)
    };
    let initialized = (|| {
        let uffd = Arc::new(Uffd::open()?);
        let faults = Arc::new(crate::devices::virtio::memory_gate::ColdFaultActivity::default());
        let pages = handle
            .ram_quiesced(|vmm| {
                // Disable the other destructive RAM owner before registration.
                // A closed idle gate guarantees every old balloon lease drained.
                vmm.device_memory_gate()
                    .install_cold_faults(faults.clone())?;
                for mapping in &mappings {
                    // Existing bytes remain mapped; untouched/previously ballooned
                    // holes remain zero. No new destructive owner can create holes
                    // after the fault gate is installed. Missing faults without a
                    // cold object are therefore safe lazy-zero allocations.
                    uffd.register(*mapping).map_err(|error| error.to_string())?;
                }
                Ok(host_sorted_pages(
                    &mappings,
                    Instant::now() + Duration::from_secs(1),
                    block_bytes,
                ))
            })
            .map_err(io::Error::other)?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "VM busy during cold RAM registration",
                )
            })?;
        Ok::<_, io::Error>(Arc::new(Mutex::new(Pager {
            memory,
            pages,
            store: Arc::new(Mutex::new(store)),
            uffd,
            cursor: 0,
            restored: 0,
            discarded: 0,
            rejected: 0,
            put_rejections: 0,
            restore_count: 0,
            restore_total_us: 0,
            restore_max_us: 0,
        })))
    })();
    let pager = match initialized {
        Ok(pager) => pager,
        Err(error) => {
            handle.cold_pager_started.store(false, Ordering::Release);
            return Err(error);
        }
    };
    if options.metrics {
        let pager = pager.lock().unwrap();
        let pid = std::process::id();
        let eligible_bytes: u64 = mappings.iter().map(|range| range.len).sum();
        // Geometry only: a consumer may attribute smaps PSS to this union only
        // if complete VMA intervals exactly cover it. A larger coalesced VMA is
        // an envelope, not isolated RAM; its per-RAM PSS must be unavailable.
        eprintln!(
            "pvisor-cold-linux-layout host_page_bytes=4096 block_bytes={block_bytes} eligible_mappings={} eligible_bytes_total={eligible_bytes} kernel_excluded={} pss_attribution=requires_exact_vma_union pid={pid}",
            mappings.len(),
            kernel.is_some()
        );
        for region in pager.memory.iter() {
            if mappings
                .iter()
                .any(|range| range.start == region.as_ptr() as u64 && range.len == region.len())
            {
                eprintln!(
                    "pvisor-cold-linux-region host_start=0x{:x} length={} guest_start=0x{:x} pid={pid}",
                    region.as_ptr() as usize,
                    region.len(),
                    region.start_addr().0
                );
            }
        }
        if let Some(kernel) = kernel {
            eprintln!(
                "pvisor-cold-linux-kernel-excluded host_start=0x{:x} length={} guest_start=0x{:x} reason=trusted_raw_firmware pid={pid}",
                kernel.host_address, kernel.length, kernel.guest_address
            );
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let resolver_pager = pager.clone();
    let resolver_stop = stop.clone();
    let uffd = pager.lock().unwrap().uffd.clone();
    let faults = {
        let vmm = handle
            .vmm
            .upgrade()
            .ok_or_else(|| io::Error::other("VMM has stopped"))?;
        let locked = vmm
            .lock()
            .map_err(|_| io::Error::other("VMM lock poisoned"))?;
        locked
            .device_memory_gate()
            .cold_faults()
            .ok_or_else(|| io::Error::other("missing cold fault monitor"))?
    };
    let resolver = match std::thread::Builder::new()
        .name("pvisor-uffd".into())
        .spawn(move || {
            while !resolver_stop.load(Ordering::Acquire) {
                if let Some(address) = uffd.event().unwrap_or_else(|error| fatal(error)) {
                    let _fault = faults.begin();
                    resolver_pager
                        .lock()
                        .unwrap_or_else(|_| fatal("pager poisoned"))
                        .fault(address)
                        .unwrap_or_else(|error| fatal(error));
                }
            }
        }) {
        Ok(thread) => thread,
        Err(error) => {
            pager.lock().unwrap().shutdown()?;
            handle.cold_pager_started.store(false, Ordering::Release);
            return Err(error);
        }
    };
    let reservation = handle.cold_pager_started.clone();
    let worker_pager = pager.clone();
    let worker_stop = stop.clone();
    let worker = std::thread::Builder::new().name("pvisor-cold-pager".into()).spawn(move || {
        let pool = worker_pager.lock().unwrap().store.clone();
        loop {
            std::thread::sleep(Duration::from_millis(250));
            // Empty scans still need to observe VM teardown without a barrier.
            if handle.vmm.upgrade().is_none() {
                break;
            }
            let mut candidates = worker_pager.lock().unwrap_or_else(|_| fatal("pager poisoned")).candidates();
            let sampled = quiesce_nonempty(&mut candidates, |candidates| handle.ram_quiesced(|_| {
                Ok(worker_pager.lock().map_err(|_| "pager poisoned")?.sample_candidates(candidates))
            }));
            let batch = match sampled {
                Ok(Some(batch)) => batch,
                Ok(None) => continue,
                Err(error) if error == "VMM has stopped" => break,
                Err(error) => fatal(error),
            };
            let mut objects = Vec::new();
            let mut put_rejections = 0;
            for snapshot in &batch.0 {
                // Neither VMM/barrier nor pager state is held during publication.
                let object = publish_snapshot(&worker_pager, &pool, snapshot)
                    .unwrap_or_else(|error| fatal(error));
                if let Some(object) = object {
                    objects.push((snapshot, Some(object)));
                } else {
                    put_rejections += 1;
                }
            }
            worker_pager.lock().unwrap_or_else(|_| fatal("pager poisoned")).put_rejections += put_rejections;
            let committed = quiesce_nonempty(&mut objects, |objects| handle.ram_quiesced(|_| {
                let mut pager = worker_pager.lock().map_err(|_| "pager poisoned")?;
                for (snapshot, object) in objects {
                    // Successful publication still needs the live-byte recheck
                    // under the second barrier before destructive discard.
                    if let Some(published) = object.take() {
                        *object = pager.commit(snapshot, published).map_err(|error| error.to_string())?;
                    }
                }
                Ok(())
            }));
            for object in objects.into_iter().filter_map(|(_, object)| object) {
                pool.lock().unwrap_or_else(|_| fatal("store poisoned")).release(object).unwrap_or_else(|error| fatal(error));
            }
            match committed {
                Ok(_) => {},
                Err(error) if error == "VMM has stopped" => break,
                Err(error) => fatal(error),
            }
            if options.metrics {
                // Pool RPCs never run with the pager or VM barrier held. Pool
                // counters are current pool-wide values, not VM physical savings.
                let stats = pool.lock().unwrap_or_else(|_| fatal("store poisoned")).stats().unwrap_or_else(|error| fatal(error));
                let pager = worker_pager.lock().unwrap_or_else(|_| fatal("pager poisoned"));
                let cold: u64 = pager.pages.iter().filter(|page| page.cold.is_some()).map(|page| page.range.len).sum();
                eprintln!("pvisor-cold-linux cold_bytes_current={cold} discarded_bytes_total={} restored_bytes_total={} invalidated_total={} put_rejections_total={} restore_count_total={} restore_time_us_total={} restore_time_us_max={} snapshots_batch={} pool_encoded_bytes_current={} pool_objects_current={} pool_session_references_current={} pool_cross_session_objects_current={} pid={}",
                    pager.discarded, pager.restored, pager.rejected, pager.put_rejections,
                    pager.restore_count, pager.restore_total_us, pager.restore_max_us,
                    batch.0.len(), stats.encoded_bytes, stats.objects, stats.session_references,
                    stats.cross_session_objects, std::process::id());
            }
        }
        worker_pager.lock().unwrap_or_else(|_| fatal("pager poisoned")).shutdown().unwrap_or_else(|error| fatal(error));
        worker_stop.store(true, Ordering::Release);
    });
    if let Err(error) = worker {
        stop.store(true, Ordering::Release);
        resolver
            .join()
            .map_err(|_| io::Error::other("UFFD resolver panicked"))?;
        pager.lock().unwrap().shutdown()?;
        reservation.store(false, Ordering::Release);
        return Err(error);
    }
    // Workers own pinned memory and store through shutdown. The VM handle is weak.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ColdRamPoolStats;
    use std::io::{Read, Write};
    use vm_memory::{Bytes, GuestAddress};

    #[derive(Default)]
    struct CompressedStore {
        puts: usize,
        restores: usize,
        releases: usize,
        corrupt: bool,
        reject_barrier: Option<Arc<std::sync::Barrier>>,
    }
    impl ColdRamStore for CompressedStore {
        type Object = Vec<u8>;
        fn put(&mut self, bytes: &[u8]) -> io::Result<Vec<u8>> {
            if let Some(barrier) = &self.reject_barrier {
                barrier.wait();
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "incompressible"));
            }
            let mut encoder =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(bytes)?;
            self.puts += 1;
            encoder.finish()
        }
        fn restore(&mut self, object: &Vec<u8>, output: &mut [u8]) -> io::Result<()> {
            self.restores += 1;
            flate2::read::DeflateDecoder::new(object.as_slice()).read_exact(output)?;
            if self.corrupt {
                output[0] ^= 1;
            }
            Ok(())
        }
        fn release(&mut self, _: Vec<u8>) -> io::Result<()> {
            self.releases += 1;
            Ok(())
        }
        fn stats(&mut self) -> io::Result<ColdRamPoolStats> {
            Ok(ColdRamPoolStats {
                encoded_bytes: 0,
                objects: 0,
                session_references: 0,
                cross_session_objects: 0,
            })
        }
    }
    fn pager(blocks: usize, real: bool) -> Pager<CompressedStore> {
        let memory = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), blocks * BLOCK)]).unwrap();
        let bytes: Vec<_> = (0..blocks * BLOCK)
            .map(|index| (index % 251) as u8)
            .collect();
        memory.write_slice(&bytes, GuestAddress(0)).unwrap();
        let mappings = ranges(&memory, u64::MAX).unwrap();
        let uffd = if real {
            Uffd::open().expect("kernel-fault UFFD access is required")
        } else {
            Uffd(std::fs::File::open("/dev/null").unwrap().into())
        };
        if real {
            for range in &mappings {
                uffd.register(*range).unwrap();
            }
        }
        let pages = mappings
            .iter()
            .flat_map(|mapping| {
                (0..mapping.len).step_by(BLOCK).map(move |offset| Page {
                    range: Range {
                        start: mapping.start + offset,
                        len: BLOCK as u64,
                    },
                    cold: None,
                    eligible_at: Instant::now(),
                })
            })
            .collect();
        Pager {
            memory,
            pages,
            store: Arc::new(Mutex::new(CompressedStore::default())),
            uffd: Arc::new(uffd),
            cursor: 0,
            restored: 0,
            discarded: 0,
            rejected: 0,
            put_rejections: 0,
            restore_count: 0,
            restore_total_us: 0,
            restore_max_us: 0,
        }
    }
    fn publish(pager: &mut Pager<CompressedStore>) -> Vec<u8> {
        pager.pages[0].eligible_at = Instant::now();
        let batch = pager.sample();
        assert_eq!(batch.0.len(), 1);
        let snapshot = &batch.0[0];
        let object = pager.store.lock().unwrap().put(&snapshot.bytes).unwrap();
        assert!(
            object.len() < snapshot.bytes.len() / 4,
            "test payload must actually compress"
        );
        let expected = snapshot.bytes.clone();
        assert!(pager.commit(snapshot, object).unwrap().is_none());
        expected
    }
    #[test]
    fn host_sorted_fault_lookup_handles_gaps_and_partial_tails() {
        let mut pager = pager(1, false);
        // Guest order can be the reverse of host mmap order.
        pager.pages = host_sorted_pages(
            &[
                Range {
                    start: 0x80000,
                    len: BLOCK as u64 + 4096,
                },
                Range {
                    start: 0x10000,
                    len: BLOCK as u64 + 8192,
                },
            ],
            Instant::now(),
            BLOCK,
        );
        assert_eq!(
            pager
                .pages
                .iter()
                .map(|page| page.range.start)
                .collect::<Vec<_>>(),
            vec![0x10000, 0x20000, 0x80000, 0x90000]
        );
        for (address, expected) in [
            (0, None),
            (0xffff, None),
            (0x10000, Some(0)),
            (0x1ffff, Some(0)),
            (0x20000, Some(1)),
            (0x21fff, Some(1)),
            (0x22000, None),
            (0x7ffff, None),
            (0x80000, Some(2)),
            (0x8ffff, Some(2)),
            (0x90000, Some(3)),
            (0x90fff, Some(3)),
            (0x91000, None),
            (u64::MAX, None),
        ] {
            assert_eq!(pager.fault_index(address), expected, "address {address:#x}");
            if expected.is_none() {
                assert_eq!(
                    pager.fault(address).unwrap_err().to_string(),
                    "UFFD fault outside owned RAM"
                );
            }
        }
    }

    #[test]
    fn rejected_publication_does_not_deadlock_fault_recovery() {
        let mut state = pager(1, false);
        let batch = state.sample();
        let pool = state.store.clone();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        pool.lock().unwrap().reject_barrier = Some(barrier.clone());
        let state = Arc::new(Mutex::new(state));
        let resolver_state = state.clone();
        let resolver_pool = pool.clone();
        let resolver = std::thread::spawn(move || {
            let _pager = resolver_state.lock().unwrap();
            barrier.wait();
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                if resolver_pool.try_lock().is_ok() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "publication holds store while waiting for fault recovery's pager lock"
                );
                std::thread::yield_now();
            }
        });
        assert!(
            publish_snapshot(&state, &pool, &batch.0[0])
                .unwrap()
                .is_none()
        );
        resolver.join().unwrap();
        let state = state.lock().unwrap();
        assert!(state.pages[0].cold.is_none());
        assert!(state.pages[0].eligible_at > Instant::now());
        assert_eq!(state.discarded, 0);
    }

    #[test]
    fn empty_or_ineligible_work_never_enters_barriers() {
        let mut pager = pager(2, false);
        pager.pages[0].cold = Some(Box::new((Vec::new(), [0; 32])));
        pager.pages[1].eligible_at = Instant::now() + Duration::from_secs(30);
        let mut candidates = pager.candidates();
        assert!(candidates.is_empty());
        let sampled = quiesce_nonempty(&mut candidates, |_| -> Result<Option<Batch>, String> {
            panic!("empty scan entered sampling barrier")
        })
        .unwrap();
        assert!(sampled.is_none());

        // An empty batch or an all-rejected publication has no commit work.
        let mut published: Vec<(usize, Vec<u8>)> = Vec::new();
        let committed = quiesce_nonempty(&mut published, |_| -> Result<Option<()>, String> {
            panic!("empty publication entered commit barrier")
        })
        .unwrap();
        assert!(committed.is_none());

        pager.pages[1].eligible_at = Instant::now();
        let mut candidates = pager.candidates();
        assert_eq!(candidates, vec![1]);
        let sampled = quiesce_nonempty(&mut candidates, |candidates| {
            Ok(Some(pager.sample_candidates(candidates)))
        })
        .unwrap()
        .unwrap();
        assert_eq!(sampled.0.len(), 1);
        assert_eq!(sampled.0[0].index, 1);

        // A fault restore between selection and the barrier can start cooldown.
        pager.pages[1].eligible_at = Instant::now() + Duration::from_secs(30);
        assert!(pager.sample_candidates(&candidates).0.is_empty());
    }

    #[test]
    fn publication_write_race_preserves_live_bytes_and_reference() {
        let mut pager = pager(1, false);
        let batch = pager.sample();
        let object = pager.store.lock().unwrap().put(&batch.0[0].bytes).unwrap();
        pager.memory.write_slice(&[0xab], GuestAddress(42)).unwrap();
        let unused = pager.commit(&batch.0[0], object).unwrap().unwrap();
        pager.store.lock().unwrap().release(unused).unwrap();
        assert!(pager.pages[0].cold.is_none());
        assert_eq!(pager.memory.read_obj::<u8>(GuestAddress(42)).unwrap(), 0xab);
        assert_eq!(pager.discarded, 0);
        assert_eq!(pager.rejected, 1);
        assert_eq!(pager.store.lock().unwrap().releases, 1);
        assert!(
            pager.sample().0.is_empty(),
            "invalidated blocks need a cooldown"
        );
    }
    #[test]
    fn corrupt_restore_rejected_before_copy_or_reference_release() {
        let mut pager = pager(1, false);
        let bytes = vec![0x5a; BLOCK];
        let object = pager.store.lock().unwrap().put(&bytes).unwrap();
        pager.pages[0].cold = Some(Box::new((object, digest(&bytes))));
        pager.store.lock().unwrap().corrupt = true;
        assert_eq!(
            pager.restore(0).unwrap_err().to_string(),
            "cold RAM restore checksum mismatch"
        );
        assert!(pager.pages[0].cold.is_some());
        assert_eq!(pager.store.lock().unwrap().releases, 0);
        assert_eq!(pager.restored, 0);
    }
    #[test]
    fn batch_is_bounded_and_excludes_cold_pages() {
        let mut pager = pager(BATCH_BYTES / BLOCK + 2, false);
        pager.pages[0].cold = Some(Box::new((Vec::new(), [0; 32])));
        let batch = pager.sample();
        assert!(batch.0.len() <= BATCH_BYTES / BLOCK);
        assert!(!batch.0.is_empty());
        assert!(batch.0.iter().all(|snapshot| snapshot.index != 0));
        assert!(
            batch
                .0
                .iter()
                .map(|snapshot| snapshot.bytes.len())
                .sum::<usize>()
                <= 4 * 1024 * 1024
        );
    }
    #[test]
    fn trusted_raw_kernel_is_excluded_but_unknown_raw_and_file_ram_are_rejected() {
        use crate::vmm::RawKernelMapping;
        use vm_memory::{GuestRegionMmap, MmapRegion};
        // Keep the firmware allocation alive: build_raw borrows, rather than
        // owns, the bundle's address exactly as the fresh builder does.
        let firmware: GuestMemoryMmap =
            GuestMemoryMmap::from_ranges(&[(GuestAddress(0), BLOCK)]).unwrap();
        let host = firmware.iter().next().unwrap().as_ptr();
        let raw = unsafe { MmapRegion::build_raw(host, BLOCK, 0, 0) }.unwrap();
        let raw = Arc::new(GuestRegionMmap::new(raw, GuestAddress(BLOCK as u64)).unwrap());
        let memory = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), BLOCK)])
            .unwrap()
            .insert_region(raw)
            .unwrap();
        let identity = RawKernelMapping {
            guest_address: BLOCK as u64,
            host_address: host as usize,
            length: BLOCK,
        };
        let eligible = ranges_excluding_kernel(&memory, u64::MAX, Some(identity)).unwrap();
        assert_eq!(eligible.len(), 1);
        assert_eq!(
            eligible[0].start,
            memory.iter().next().unwrap().as_ptr() as u64
        );
        assert_eq!(eligible[0].len, BLOCK as u64);
        assert_eq!(
            ranges(&memory, u64::MAX).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        for wrong in [
            RawKernelMapping {
                host_address: identity.host_address + 4096,
                ..identity
            },
            RawKernelMapping {
                guest_address: identity.guest_address + 4096,
                ..identity
            },
            RawKernelMapping {
                length: identity.length - 4096,
                ..identity
            },
        ] {
            assert_eq!(
                ranges_excluding_kernel(&memory, u64::MAX, Some(wrong))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::Unsupported
            );
        }
        // An otherwise matching guest/host topology is not trusted firmware
        // without the specific raw-region metadata used by load_payload.
        let writable_raw = unsafe {
            MmapRegion::build_raw(
                host,
                BLOCK,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            )
        }
        .unwrap();
        let writable = GuestMemoryMmap::from_regions(vec![
            GuestRegionMmap::new(writable_raw, GuestAddress(BLOCK as u64)).unwrap(),
        ])
        .unwrap();
        assert!(ranges_excluding_kernel(&writable, u64::MAX, Some(identity)).is_err());
        let file = Arc::new(tempfile::tempfile().unwrap());
        file.set_len(BLOCK as u64).unwrap();
        let extra = vm_memory::mmap::MmapRegionBuilder::new(BLOCK)
            .with_file_offset(vm_memory::FileOffset::from_arc(file, 0))
            .with_mmap_prot(libc::PROT_READ | libc::PROT_WRITE)
            .with_mmap_flags(libc::MAP_PRIVATE)
            .build()
            .unwrap();
        let with_file = memory
            .insert_region(Arc::new(
                GuestRegionMmap::new(extra, GuestAddress((BLOCK * 2) as u64)).unwrap(),
            ))
            .unwrap();
        assert_eq!(
            ranges_excluding_kernel(&with_file, u64::MAX, Some(identity))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }
    #[test]
    fn file_and_device_mappings_are_not_eligible() {
        let file = Arc::new(tempfile::tempfile().unwrap());
        file.set_len(BLOCK as u64).unwrap();
        let memory = GuestMemoryMmap::from_ranges_with_files(&[(
            GuestAddress(0),
            BLOCK,
            Some(vm_memory::FileOffset::from_arc(file, 0)),
        )])
        .unwrap();
        assert_eq!(
            ranges(&memory, u64::MAX).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        let anon = pager(1, false);
        assert_eq!(
            ranges(&anon.memory, 0).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }
    #[cfg(not(feature = "tee"))]
    fn reporting_balloon(
        memory: &GuestMemoryMmap,
    ) -> (
        crate::devices::virtio::Balloon,
        crate::devices::virtio::VirtQueue<'_>,
    ) {
        use crate::devices::{
            legacy::DummyIrqChip,
            virtio::{Balloon, DeviceQueue, InterruptTransport, VirtioDevice},
        };
        use crate::utils::eventfd::{EFD_NONBLOCK, EventFd};
        let report = crate::devices::virtio::VirtQueue::new(GuestAddress(0x1000), memory, 8);
        report.dtable[0].addr.set(BLOCK as u64);
        report.dtable[0].len.set(BLOCK as u32);
        report.dtable[0].flags.set(0x2); // Virtio writable descriptor.
        let mut balloon = Balloon::new().unwrap();
        let queues = (0..balloon.queue_config().len())
            .map(|index| {
                DeviceQueue::new(
                    if index == 4 {
                        report.create_queue()
                    } else {
                        crate::devices::virtio::Queue::new(8)
                    },
                    Arc::new(EventFd::new(EFD_NONBLOCK).unwrap()),
                )
            })
            .collect();
        balloon
            .activate(
                memory.clone(),
                InterruptTransport::new(DummyIrqChip::new().into(), "balloon-test".into()).unwrap(),
                queues,
            )
            .unwrap();
        (balloon, report)
    }
    #[test]
    #[cfg(not(feature = "tee"))]
    fn ordinary_balloon_discard_is_unchanged_and_cold_policy_acknowledges_without_discard() {
        use crate::devices::virtio::memory_gate::{ColdFaultActivity, register};
        let pager = pager(2, false);
        let memory = &pager.memory;
        let gate = register(memory);
        let (mut balloon, report) = reporting_balloon(memory);
        report.avail.ring[0].set(0);
        report.avail.idx.set(1);
        assert!(balloon.process_frq());
        assert_eq!(report.used.idx.get(), 1);
        let mut bytes = vec![1; BLOCK];
        memory
            .read_slice(&mut bytes, GuestAddress(BLOCK as u64))
            .unwrap();
        assert!(
            bytes.iter().all(|byte| *byte == 0),
            "ordinary balloon must still discard"
        );
        gate.try_close().unwrap();
        gate.install_cold_faults(Arc::new(ColdFaultActivity::default()))
            .unwrap();
        gate.open();
        memory
            .write_slice(&vec![0x6b; BLOCK], GuestAddress(BLOCK as u64))
            .unwrap();
        report.avail.ring[1].set(0);
        report.avail.idx.set(2);
        assert!(balloon.process_frq());
        assert_eq!(report.used.idx.get(), 2);
        memory
            .read_slice(&mut bytes, GuestAddress(BLOCK as u64))
            .unwrap();
        assert!(bytes.iter().all(|byte| *byte == 0x6b));
    }
    #[test]
    #[cfg(not(feature = "tee"))]
    #[ignore = "requires permitted kernel-fault userfaultfd access; real balloon free-page queue"]
    fn actual_balloon_reports_cannot_discard_resident_or_cold_pager_ram() {
        use crate::devices::virtio::memory_gate::{ColdFaultActivity, register};
        let mut state = pager(2, false);
        let memory = state.memory.clone();
        let gate = register(&memory);
        let (mut balloon, report) = reporting_balloon(&memory);
        // A prior ordinary balloon discard is legal. Startup must prefault that
        // hole before registering UFFD, not assume all Resident bytes exist.
        report.avail.ring[0].set(0);
        report.avail.idx.set(1);
        assert!(balloon.process_frq());
        assert!(gate.try_close().unwrap());
        gate.install_cold_faults(Arc::new(ColdFaultActivity::default()))
            .unwrap();
        let uffd = Arc::new(Uffd::open().expect("kernel-fault UFFD access is required"));
        let mappings = ranges(&memory, u64::MAX).unwrap();
        for mapping in mappings {
            for offset in (0..mapping.len as usize).step_by(4096) {
                let ptr = (mapping.start as *mut u8).wrapping_add(offset);
                unsafe {
                    ptr.write_volatile(ptr.read_volatile());
                }
            }
            uffd.register(mapping).unwrap();
        }
        state.uffd = uffd;
        gate.open();
        let range = state.pages[1].range;
        let expected = vec![0x5c; BLOCK];
        memory
            .write_slice(&expected, GuestAddress(BLOCK as u64))
            .unwrap();
        report.avail.ring[1].set(0);
        report.avail.idx.set(2);
        assert!(balloon.process_frq());
        assert_eq!(
            resident(range),
            BLOCK,
            "resident reporting must not create unowned missing pages"
        );
        let object = state.store.lock().unwrap().put(&expected).unwrap();
        assert!(
            state
                .commit(
                    &Snapshot {
                        index: 1,
                        bytes: expected.clone()
                    },
                    object
                )
                .unwrap()
                .is_none()
        );
        let pager = Arc::new(Mutex::new(state));
        let stop = Arc::new(AtomicBool::new(false));
        let resolver = resolver(pager.clone(), stop.clone());
        report.avail.ring[2].set(0);
        report.avail.idx.set(3);
        assert!(balloon.process_frq());
        assert_eq!(report.used.idx.get(), 3);
        assert_eq!(
            resident(range),
            0,
            "disabled reporting must not read/restore or discard cold payload"
        );
        let mut bytes = vec![0; BLOCK];
        memory
            .read_slice(&mut bytes, GuestAddress(BLOCK as u64))
            .unwrap();
        assert_eq!(bytes, expected);
        pager.lock().unwrap().shutdown().unwrap();
        stop.store(true, Ordering::Release);
        resolver.join().unwrap();
    }
    fn sparse_state() -> Pager<CompressedStore> {
        let state = pager(4, false);
        let range = Range {
            start: state.memory.iter().next().unwrap().as_ptr() as u64,
            len: (4 * BLOCK) as u64,
        };
        assert_eq!(
            unsafe {
                libc::madvise(
                    range.start as *mut libc::c_void,
                    range.len as usize,
                    libc::MADV_DONTNEED,
                )
            },
            0
        );
        state
            .memory
            .write_slice(&[0x31; 4096], GuestAddress(0))
            .unwrap();
        state
            .memory
            .write_slice(&[0x93; 4096], GuestAddress((4 * BLOCK - 4096) as u64))
            .unwrap();
        state
    }

    #[test]
    fn sparse_selection_does_not_materialize_or_snapshot_holes() {
        let mut state = sparse_state();
        assert!(state.sample().0.is_empty());
        let range = Range {
            start: state.memory.iter().next().unwrap().as_ptr() as u64,
            len: (4 * BLOCK) as u64,
        };
        assert_eq!(resident(range), 8192);
    }

    #[test]
    fn page_granular_selection_reclaims_resident_pages_in_sparse_blocks() {
        let mut state = sparse_state();
        let mappings = ranges(&state.memory, u64::MAX).unwrap();
        state.pages = host_sorted_pages(&mappings, Instant::now(), 4096);
        let batch = state.sample();
        assert_eq!(batch.0.len(), 2);
        assert_eq!(batch.0[0].bytes, vec![0x31; 4096]);
        assert_eq!(batch.0[1].bytes, vec![0x93; 4096]);
        assert_eq!(resident(mappings[0]), 8192);
    }

    #[test]
    fn unoccupied_page_metadata_does_not_reserve_full_store_objects() {
        // Store-specific object sizes must not multiply by all configured pages.
        assert!(std::mem::size_of::<Page<[u8; 4096]>>() <= 48);
    }

    #[test]
    #[ignore = "requires permitted kernel-fault userfaultfd; never changes host policy"]
    fn page_granular_fault_restores_only_requested_page() {
        let mut state = pager(1, true);
        let memory = state.memory.clone();
        let mappings = ranges(&memory, u64::MAX).unwrap();
        state.pages = host_sorted_pages(&mappings, Instant::now(), 4096);
        let batch = state.sample();
        let expected: Vec<_> = batch.0.iter().map(|snapshot| snapshot.bytes.clone()).collect();
        for snapshot in &batch.0 {
            let object = state.store.lock().unwrap().put(&snapshot.bytes).unwrap();
            assert!(state.commit(snapshot, object).unwrap().is_none());
        }
        assert_eq!(resident(mappings[0]), 0);
        let pager = Arc::new(Mutex::new(state));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = resolver(pager.clone(), stop.clone());
        let mut bytes = [0; 4096];
        memory.read_slice(&mut bytes, GuestAddress(4096)).unwrap();
        assert_eq!(bytes.as_slice(), expected[1]);
        {
            let state = pager.lock().unwrap();
            assert!(state.pages[0].cold.is_some());
            assert!(state.pages[1].cold.is_none());
            assert!(state.pages[2].cold.is_some());
            assert_eq!(state.restored, 4096);
        }
        assert_eq!(resident(mappings[0]), 4096);
        pager.lock().unwrap().shutdown().unwrap();
        stop.store(true, Ordering::Release);
        thread.join().unwrap();
        for (index, expected) in expected.iter().enumerate() {
            memory.read_slice(&mut bytes, GuestAddress((index * 4096) as u64)).unwrap();
            assert_eq!(bytes.as_slice(), expected);
        }
    }

    #[test]
    #[ignore = "requires permitted kernel-fault userfaultfd; never changes host policy"]
    fn sparse_missing_faults_preserve_lazy_zeroes_and_existing_bytes() {
        let mut state = sparse_state();
        let memory = state.memory.clone();
        let mappings = ranges(&memory, u64::MAX).unwrap();
        let uffd = Arc::new(Uffd::open().unwrap());
        for range in &mappings {
            uffd.register(*range).unwrap();
        }
        state.uffd = uffd;
        assert_eq!(
            resident(mappings[0]),
            8192,
            "registration must not prefault RAM"
        );
        let pager = Arc::new(Mutex::new(state));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = resolver(pager.clone(), stop.clone());
        let mut zero = [1; 4096];
        memory
            .read_slice(&mut zero, GuestAddress(BLOCK as u64))
            .unwrap();
        assert_eq!(zero, [0; 4096]);
        memory
            .write_slice(&[0x72; 4096], GuestAddress((BLOCK + 4096) as u64))
            .unwrap();
        // A stale queued missing event must not overwrite an existing page.
        pager.lock().unwrap().fault(mappings[0].start).unwrap();
        let mut first = [0; 4096];
        memory.read_slice(&mut first, GuestAddress(0)).unwrap();
        assert_eq!(first, [0x31; 4096]);
        memory
            .read_slice(&mut first, GuestAddress((4 * BLOCK - 4096) as u64))
            .unwrap();
        assert_eq!(first, [0x93; 4096]);
        memory
            .read_slice(&mut first, GuestAddress((BLOCK + 4096) as u64))
            .unwrap();
        assert_eq!(first, [0x72; 4096]);
        assert!(
            resident(mappings[0]) <= 4 * 4096,
            "faults must not populate entire cold blocks"
        );
        pager.lock().unwrap().shutdown().unwrap();
        stop.store(true, Ordering::Release);
        thread.join().unwrap();
    }

    fn resolver(
        pager: Arc<Mutex<Pager<CompressedStore>>>,
        stop: Arc<AtomicBool>,
    ) -> std::thread::JoinHandle<()> {
        let uffd = pager.lock().unwrap().uffd.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                if let Some(address) = uffd.event().unwrap() {
                    pager.lock().unwrap().fault(address).unwrap();
                }
            }
        })
    }
    fn resident(range: Range) -> usize {
        let mut status = vec![0; range.len as usize / 4096];
        assert_eq!(
            unsafe {
                libc::mincore(
                    range.start as *mut libc::c_void,
                    range.len as usize,
                    status.as_mut_ptr(),
                )
            },
            0
        );
        status.iter().filter(|byte| **byte & 1 != 0).count() * 4096
    }
    #[test]
    #[ignore = "requires permitted kernel-fault userfaultfd access; never changes host policy"]
    fn actual_missing_reads_writes_and_kernel_io() {
        let pager = Arc::new(Mutex::new(pager(1, true)));
        let memory = pager.lock().unwrap().memory.clone();
        let range = pager.lock().unwrap().pages[0].range;
        assert_eq!(resident(range), BLOCK);
        let expected = publish(&mut pager.lock().unwrap());
        assert_eq!(
            resident(range),
            0,
            "discard must actually remove anonymous pages"
        );
        let stop = Arc::new(AtomicBool::new(false));
        let resolver = resolver(pager.clone(), stop.clone());
        // Concurrent device-like host readers all see the complete original bytes.
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let memory = memory.clone();
                let expected = &expected;
                scope.spawn(move || {
                    let mut output = vec![0; BLOCK];
                    memory.read_slice(&mut output, GuestAddress(0)).unwrap();
                    assert_eq!(&output, expected);
                });
            }
        });
        assert_eq!(resident(range), BLOCK);
        publish(&mut pager.lock().unwrap());
        memory
            .write_slice(&[0xe7; 4096], GuestAddress(4096))
            .unwrap();
        let mut output = vec![0; BLOCK];
        memory.read_slice(&mut output, GuestAddress(0)).unwrap();
        assert_eq!(&output[..4096], &expected[..4096]);
        assert_eq!(&output[4096..8192], &[0xe7; 4096]);
        assert_eq!(&output[8192..], &expected[8192..]);
        let expected = publish(&mut pager.lock().unwrap());
        let file = tempfile::tempfile().unwrap();
        // pwrite reads the cold buffer from kernel context, not userspace.
        assert_eq!(
            unsafe {
                libc::pwrite(
                    file.as_raw_fd(),
                    range.start as *const libc::c_void,
                    BLOCK,
                    0,
                )
            },
            BLOCK as isize
        );
        let mut persisted = vec![0; BLOCK];
        use std::os::unix::fs::FileExt;
        file.read_exact_at(&mut persisted, 0).unwrap();
        assert_eq!(persisted, expected);
        publish(&mut pager.lock().unwrap());
        // pread writes cold RAM from kernel context. Entire-block contents and
        // untouched bytes are verified after the kernel-origin missing fault.
        file.write_all_at(&[0x34; 4096], 0).unwrap();
        assert_eq!(
            unsafe {
                libc::pread(
                    file.as_raw_fd(),
                    (range.start + 8192) as *mut libc::c_void,
                    4096,
                    0,
                )
            },
            4096
        );
        memory.read_slice(&mut output, GuestAddress(0)).unwrap();
        assert_eq!(&output[..8192], &expected[..8192]);
        assert_eq!(&output[8192..12288], &[0x34; 4096]);
        assert_eq!(&output[12288..], &expected[12288..]);
        pager.lock().unwrap().shutdown().unwrap();
        stop.store(true, Ordering::Release);
        resolver.join().unwrap();
        let locked = pager.lock().unwrap();
        let store = locked.store.lock().unwrap();
        assert_eq!(store.puts, store.releases);
    }
    #[test]
    #[ignore = "requires /dev/kvm and permitted kernel-fault userfaultfd access; one VM"]
    fn actual_kvm_reads_and_writes() {
        use kvm_ioctls::{Kvm, VcpuExit};
        let pager = Arc::new(Mutex::new(pager(1, true)));
        let memory = pager.lock().unwrap().memory.clone();
        // Real-mode load, increment, store, then port exit.
        memory
            .write_slice(
                &[0xa1, 0x00, 0x20, 0x40, 0xa3, 0x00, 0x20, 0xe6, 0x80, 0xf4],
                GuestAddress(0x1000),
            )
            .unwrap();
        memory.write_obj(41u16, GuestAddress(0x2000)).unwrap();
        let kvm = Kvm::new().expect("/dev/kvm access is required");
        let vm = kvm.create_vm().unwrap();
        let range = pager.lock().unwrap().pages[0].range;
        unsafe {
            vm.set_user_memory_region(kvm_bindings::kvm_userspace_memory_region {
                slot: 0,
                flags: 0,
                guest_phys_addr: 0,
                memory_size: BLOCK as u64,
                userspace_addr: range.start,
            })
            .unwrap();
        }
        let mut cpu = vm.create_vcpu(0).unwrap();
        let mut sregs = cpu.get_sregs().unwrap();
        sregs.cs.base = 0;
        sregs.cs.selector = 0;
        cpu.set_sregs(&sregs).unwrap();
        cpu.set_regs(&kvm_bindings::kvm_regs {
            rip: 0x1000,
            rflags: 2,
            ..Default::default()
        })
        .unwrap();
        let expected = publish(&mut pager.lock().unwrap());
        assert_eq!(resident(range), 0);
        let stop = Arc::new(AtomicBool::new(false));
        let resolver = resolver(pager.clone(), stop.clone());
        match cpu.run().unwrap() {
            VcpuExit::IoOut(port, data) => {
                assert_eq!(port, 0x80);
                assert_eq!(data, &[42]);
            }
            exit => panic!("unexpected KVM exit: {exit:?}"),
        }
        assert_eq!(memory.read_obj::<u16>(GuestAddress(0x2000)).unwrap(), 42);
        let mut output = vec![0; BLOCK];
        memory.read_slice(&mut output, GuestAddress(0)).unwrap();
        let mut expected = expected;
        expected[0x2000..0x2002].copy_from_slice(&42u16.to_ne_bytes());
        assert_eq!(output, expected);
        drop(cpu);
        drop(vm);
        pager.lock().unwrap().shutdown().unwrap();
        stop.store(true, Ordering::Release);
        resolver.join().unwrap();
    }
}
