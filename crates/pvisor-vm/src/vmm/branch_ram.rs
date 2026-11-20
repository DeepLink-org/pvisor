//! Linux snapshot-branch RAM and private delta ownership.
//!
//! The immutable restore mapping is the base. A page becomes branch-owned only
//! after its COW bytes have been saved to the exclusive delta file. Subsequent
//! writes use MAP_SHARED delta mappings; clean base pages remain MAP_PRIVATE.
//! The VMM must park every CPU and drain device writers for the entire operation.
use std::{
    collections::BTreeSet,
    fs::File,
    io,
    os::{
        fd::AsRawFd,
        unix::fs::{FileExt, MetadataExt},
    },
    sync::Arc,
};
use vm_memory::{GuestMemory, GuestMemoryMmap, GuestMemoryRegion};

const PRESENT: u64 = 1 << 63;
const SWAPPED: u64 = 1 << 62;
const FILE_PAGE: u64 = 1 << 61;
// Bound fragmentation before replacing any mappings. Do not silently turn a
// fragmented branch into a full private copy to obtain a successful result.
const MAX_DELTA_RUNS: usize = 4096;

pub(crate) fn validate_delta(delta: &File, base: &File) -> io::Result<()> {
    let m = delta.metadata()?;
    let b = base.metadata()?;
    let flags = unsafe { libc::fcntl(delta.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if !m.is_file()
        || m.len() != 0
        || (m.dev(), m.ino()) == (b.dev(), b.ino())
        || flags & libc::O_ACCMODE != libc::O_RDWR
        || flags & libc::O_APPEND != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "branch delta must be a separate empty read/write non-append file",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{RamMappingSnapshot, RamRestoreExtent};
    use vm_memory::{Bytes, GuestAddress};

    fn restore(compact: bool) -> super::super::snapshot::MachineRestore {
        let path = tempfile::NamedTempFile::new().unwrap();
        path.as_file()
            .set_len(if compact { 8192 } else { 16384 })
            .unwrap();
        path.as_file().write_all_at(&vec![0x37; 4096], 0).unwrap();
        path.as_file()
            .write_all_at(&vec![0x59; 4096], 4096)
            .unwrap();
        super::super::snapshot::MachineRestore {
            state: super::super::snapshot::MachineSnapshot {
                version: 1,
                cpus: vec![],
                devices: vec![],
                pio_devices: vec![],
                kvm: None,
                kernel_layout: None,
                ram: vec![RamMappingSnapshot {
                    base: 0,
                    len: 16384,
                    file_offset: 0,
                }],
            },
            ram_file: Arc::new(File::open(path.path()).unwrap()),
            ram_extents: if compact {
                vec![
                    RamRestoreExtent {
                        offset: 0,
                        length: 8192,
                        backing_offset: Some(0),
                    },
                    RamRestoreExtent {
                        offset: 8192,
                        length: 8192,
                        backing_offset: None,
                    },
                ]
            } else {
                vec![]
            },
        }
    }
    fn branch(input: &super::super::snapshot::MachineRestore) -> (GuestMemoryMmap, BranchRam) {
        let memory = input.map_ram(&[(GuestAddress(0), 16384)]).unwrap();
        let owner =
            BranchRam::new(&memory, input, Arc::new(tempfile::tempfile().unwrap())).unwrap();
        (memory, owner)
    }
    fn bytes(memory: &GuestMemoryMmap) -> Vec<u8> {
        let mut bytes = vec![0; 16384];
        memory.read_slice(&mut bytes, GuestAddress(0)).unwrap();
        bytes
    }

    #[test]
    fn four_branches_keep_cow_changes_and_shared_base_across_repeated_offload() {
        let input = restore(false);
        let mut branches = (0..4).map(|_| branch(&input)).collect::<Vec<_>>();
        let original = bytes(&branches[0].0);
        for (id, (memory, owner)) in branches.iter_mut().enumerate() {
            memory
                .write_slice(&[id as u8 + 1], GuestAddress(23))
                .unwrap();
            let expected = bytes(memory);
            let first = unsafe { owner.offload() }.unwrap();
            assert_eq!(first.newly_private_bytes, 4096);
            assert_eq!(first.retained_base_bytes, 12288);
            assert_eq!(bytes(memory), expected);
            // Existing delta page changes must be flushed on every offload;
            // clean base and old delta pages must not be recopied as new COW.
            memory
                .write_slice(&[id as u8 + 9], GuestAddress(24))
                .unwrap();
            let expected = bytes(memory);
            let again = unsafe { owner.offload() }.unwrap();
            assert_eq!(again.newly_private_bytes, 0);
            assert_eq!(again.private_backing_bytes, 4096);
            assert_eq!(bytes(memory), expected);
        }
        let mut baseline = vec![0; original.len()];
        input.ram_file.read_exact_at(&mut baseline, 0).unwrap();
        assert_eq!(baseline, original);
        // Releasing one branch does not release/rewrite another branch's base.
        branches.remove(0);
        drop(input);
        for (id, (memory, owner)) in branches.iter_mut().enumerate() {
            assert_eq!(bytes(memory)[23], id as u8 + 2);
            unsafe { owner.offload() }.unwrap();
            assert_eq!(bytes(memory)[24], id as u8 + 10);
        }
    }

    #[test]
    fn shared_postfork_page_survives_discard_and_one_siblings_rewrite() {
        let input = restore(true);
        let pool = tempfile::tempfile().unwrap();
        pool.write_all_at(&[0x91; 4096], 0).unwrap();
        let mut branches = (0..4).map(|_| branch(&input)).collect::<Vec<_>>();
        for (memory, owner) in &mut branches {
            let host = memory.get_host_address(GuestAddress(0)).unwrap();
            assert!(!owner.private_candidate(host as u64).unwrap());
            memory.write_slice(&[0x91; 4096], GuestAddress(0)).unwrap();
            assert!(owner.private_candidate(host as u64).unwrap());
            // Reproduce the physical pool's private mapping at its verified commit point.
            let mapped = unsafe { libc::mmap(host.cast(), 4096, libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_FIXED | libc::MAP_PRIVATE, pool.as_raw_fd(), 0) };
            assert_eq!(mapped, host.cast());
            assert!(!owner.private_candidate(host as u64).unwrap());
        }
        drop(input);
        let (first, siblings) = branches.split_first_mut().unwrap();
        first.0.write_slice(&[0x37], GuestAddress(0)).unwrap();
        let host = first.0.get_host_address(GuestAddress(0)).unwrap();
        assert!(first.1.private_candidate(host as u64).unwrap());
        assert_eq!(unsafe { first.1.offload() }.unwrap().newly_private_bytes, 4096);
        assert!(!first.1.private_candidate(host as u64).unwrap());
        assert_eq!(bytes(&first.0)[0], 0x37);
        for (memory, owner) in siblings {
            assert_eq!(unsafe { owner.offload() }.unwrap().newly_private_bytes, 0);
            assert_eq!(&bytes(memory)[..4096], &[0x91; 4096]);
        }
    }

    #[test]
    fn modified_to_zero_overrides_nonzero_base_and_new_cow_is_recorded() {
        let input = restore(false);
        let (memory, mut owner) = branch(&input);
        memory.write_slice(&vec![0; 4096], GuestAddress(0)).unwrap();
        unsafe { owner.offload() }.unwrap();
        assert_eq!(&bytes(&memory)[..4096], &[0; 4096]);
        memory.write_slice(b"later", GuestAddress(4100)).unwrap();
        let report = unsafe { owner.offload() }.unwrap();
        assert_eq!(report.newly_private_bytes, 4096);
        assert_eq!(report.private_backing_bytes, 8192);
        assert_eq!(&bytes(&memory)[4100..4105], b"later");
        let mut baseline = [0];
        input.ram_file.read_exact_at(&mut baseline, 0).unwrap();
        assert_eq!(baseline, [0x37]);
    }

    #[test]
    fn compact_zero_extents_keep_anonymous_private_changes() {
        let input = restore(true);
        let (memory, mut owner) = branch(&input);
        memory
            .write_slice(b"zero branch", GuestAddress(8192))
            .unwrap();
        let before = bytes(&memory);
        unsafe { owner.offload() }.unwrap();
        assert_eq!(bytes(&memory), before);
        let (_, other) = branch(&input);
        assert!(other.private.is_empty());
    }

    #[test]
    fn fragmented_delta_preserves_padding_and_isolation_on_repeated_offload() {
        let mut input = restore(false);
        let size = (2 * MAX_DELTA_RUNS + 1) * 4096;
        let base = tempfile::tempfile().unwrap();
        base.set_len(size as u64).unwrap();
        base.write_all_at(&[0x93; 4096], 4096).unwrap();
        input.ram_file = Arc::new(base);
        input.state.ram[0].len = size as u64;
        let layout = [(GuestAddress(0), size)];
        let memory = input.map_ram(&layout).unwrap();
        let sibling = input.map_ram(&layout).unwrap();
        let mut owner = BranchRam::new(&memory, &input, Arc::new(tempfile::tempfile().unwrap())).unwrap();
        for offset in (0..size).step_by(8192) {
            memory.write_slice(&[0x35], GuestAddress(offset as u64)).unwrap();
        }
        let mut expected = vec![0; size];
        memory.read_slice(&mut expected, GuestAddress(0)).unwrap();
        let report = unsafe { owner.offload() }.unwrap();
        assert_eq!(report.delta_runs, MAX_DELTA_RUNS);
        assert_eq!(owner.padding_pages, 1);
        let mut actual = vec![0; size];
        memory.read_slice(&mut actual, GuestAddress(0)).unwrap();
        assert_eq!(actual, expected);
        memory.write_slice(&[0x44], GuestAddress(4096)).unwrap();
        assert_eq!(unsafe { owner.offload() }.unwrap().newly_private_bytes, 0);
        assert_eq!(memory.read_obj::<u8>(GuestAddress(4096)).unwrap(), 0x44);
        assert_eq!(sibling.read_obj::<u8>(GuestAddress(4096)).unwrap(), 0x93);
    }

    #[test]
    fn compaction_respects_regions_and_cumulative_padding_budget() {
        let input = restore(false);
        let (_memory, mut owner) = branch(&input);
        let page = owner.page;
        owner.ranges = vec![
            Range { host: page, length: 16 * page, offset: 0 },
            Range { host: 100 * page, length: 16 * page, offset: (16 * page) as u64 },
        ];
        let pages = |indices: &[usize]| indices.iter().map(|&i| (i * page) as u64).collect::<BTreeSet<_>>();
        // A two-page gap at the region boundary must remain unmerged.
        let mut next = pages(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 17, 18, 21, 22, 24, 25]);
        let mut added = BTreeSet::new();
        let (_, cost) = owner.compact_delta_runs(&mut next, &mut added, 3).unwrap();
        assert_eq!(cost, 1);
        assert_eq!(added, pages(&[23]));
        owner.padding_pages = cost;
        added.clear();
        let before = next.clone();
        assert!(owner.compact_delta_runs(&mut next, &mut added, 2).is_err());
        assert_eq!(next, before);
        assert!(added.is_empty());
        owner.padding_pages = 0;
        let (runs, cost) = owner.compact_delta_runs(&mut next, &mut added, 2).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(cost, 2);
        assert_eq!(added, pages(&[19, 20]));
    }

    #[test]
    fn failed_write_never_discards_cow_or_allows_retry() {
        let input = restore(false);
        let (memory, mut owner) = branch(&input);
        memory
            .write_slice(b"must survive", GuestAddress(0))
            .unwrap();
        let before = bytes(&memory);
        // Simulate a failing backing service after successful installation.
        owner.delta = input.ram_file.clone(); // read-only, pwrite fails
        assert!(unsafe { owner.offload() }.is_err());
        assert_eq!(bytes(&memory), before);
        assert!(owner.private.is_empty());
        assert!(unsafe { owner.offload() }
            .unwrap_err()
            .contains("terminate"));
    }

    #[test]
    fn unreadable_page_inventory_fails_closed_before_discard() {
        let input = restore(false);
        let (memory, mut owner) = branch(&input);
        memory.write_slice(b"keep", GuestAddress(0)).unwrap();
        owner.pagemap = tempfile::tempfile().unwrap();
        assert!(unsafe { owner.offload() }.is_err());
        assert_eq!(&bytes(&memory)[..4], b"keep");
        assert!(owner.private.is_empty());
    }

    #[test]
    fn swapped_pages_are_never_mistaken_for_clean_base() {
        assert!(needs_save(SWAPPED));
        assert!(needs_save(SWAPPED | FILE_PAGE));
        assert!(needs_save(PRESENT));
        assert!(!needs_save(PRESENT | FILE_PAGE));
        assert!(!needs_save(0));
    }

    #[test]
    fn delta_validation_rejects_alias_readonly_append_and_nonempty_files() {
        let base = tempfile::NamedTempFile::new().unwrap();
        assert!(validate_delta(base.as_file(), base.as_file()).is_err());
        let separate = tempfile::NamedTempFile::new().unwrap();
        assert!(validate_delta(&File::open(separate.path()).unwrap(), base.as_file()).is_err());
        let append = std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .open(separate.path())
            .unwrap();
        assert!(validate_delta(&append, base.as_file()).is_err());
        validate_delta(separate.as_file(), base.as_file()).unwrap();
        separate.as_file().set_len(1).unwrap();
        assert!(validate_delta(separate.as_file(), base.as_file()).is_err());
    }
}

#[derive(Clone, Copy)]
struct Range {
    host: usize,
    length: usize,
    offset: u64,
}

pub(super) struct BranchRam {
    delta: Arc<File>,
    pagemap: File,
    ranges: Vec<Range>,
    page: usize,
    // Logical snapshot offsets, not PFNs or guest addresses.
    private: BTreeSet<u64>,
    padding_pages: usize,
    poisoned: bool,
}

#[derive(Debug, Default, serde::Serialize)]
pub(super) struct Report {
    pub logical_bytes: u64,
    pub newly_private_bytes: u64,
    pub private_backing_bytes: u64,
    pub retained_base_bytes: u64,
    pub delta_runs: usize,
}

fn needs_save(entry: u64) -> bool {
    // A swapped COW page must be faulted back and saved. PFNs are not used;
    // reading one's own pagemap flags does not require CAP_SYS_ADMIN.
    entry & SWAPPED != 0 || entry & PRESENT != 0 && entry & FILE_PAGE == 0
}

impl BranchRam {
    /// Called before the memory is exposed to CPUs/devices. Mapping topology is
    /// already authenticated by MachineRestore; retain that owner's exact layout.
    pub(super) fn new(
        memory: &GuestMemoryMmap,
        restore: &super::snapshot::MachineRestore,
        delta: Arc<File>,
    ) -> Result<Self, String> {
        validate_delta(&delta, &restore.ram_file).map_err(|e| e.to_string())?;
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page <= 0 {
            return Err("cannot determine branch page size".into());
        }
        let page = page as usize;
        let mut ranges = Vec::new();
        if memory.num_regions() != restore.state.ram.len() {
            return Err("branch RAM topology changed".into());
        }
        for (region, saved) in memory.iter().zip(&restore.state.ram) {
            if !region.owned()
                || region.flags() & libc::MAP_PRIVATE == 0
                || region.start_addr().0 != saved.base
                || region.len() != saved.len
                || !(region.as_ptr() as usize).is_multiple_of(page)
                || !(region.len() as usize).is_multiple_of(page)
            {
                return Err("branch requires owned private snapshot RAM".into());
            }
            ranges.push(Range {
                host: region.as_ptr() as usize,
                length: region.len() as usize,
                offset: saved.file_offset,
            });
        }
        let length = ranges
            .last()
            .and_then(|r| r.offset.checked_add(r.length as u64))
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or("invalid branch length")?;
        let pagemap = File::open("/proc/self/pagemap").map_err(|e| e.to_string())?;
        // Sparse private storage, not an eager copy of the common base.
        delta.set_len(length).map_err(|e| e.to_string())?;
        Ok(Self {
            delta,
            pagemap,
            ranges,
            page,
            private: BTreeSet::new(),
            padding_pages: 0,
            poisoned: false,
        })
    }

    pub(super) fn private_candidate(&self, address: u64) -> io::Result<bool> {
        let Some((range, relative)) = self.ranges.iter().find_map(|range| {
            let offset = address.checked_sub(range.host as u64)?;
            (offset < range.length as u64).then_some((range, offset))
        }) else { return Ok(false) };
        if !relative.is_multiple_of(self.page as u64) || self.private.contains(&(range.offset + relative)) {
            return Ok(false);
        }
        let mut entry = [0; 8];
        self.pagemap.read_exact_at(&mut entry, address / self.page as u64 * 8)?;
        Ok(u64::from_ne_bytes(entry) & (PRESENT | SWAPPED | FILE_PAGE) == PRESENT)
    }

    fn runs(&self, pages: &BTreeSet<u64>) -> Vec<Range> {
        let mut out: Vec<Range> = Vec::new();
        for region in &self.ranges {
            let first_run = out.len();
            for &offset in pages.range(region.offset..region.offset + region.length as u64) {
                if let Some(last) = out[first_run..].last_mut() {
                    if last.offset + last.length as u64 == offset
                        && last.host + last.length
                            == region.host + (offset - region.offset) as usize
                    {
                        last.length += self.page;
                        continue;
                    }
                }
                out.push(Range {
                    host: region.host + (offset - region.offset) as usize,
                    length: self.page,
                    offset,
                });
            }
        }
        out
    }

    // Consider only gaps internal to one restore region. Fill the cheapest
    // first, and charge padding to this branch across successive offloads.
    fn compact_delta_runs(
        &self,
        next: &mut BTreeSet<u64>,
        added: &mut BTreeSet<u64>,
        limit: usize,
    ) -> io::Result<(Vec<Range>, usize)> {
        let runs = self.runs(next);
        if runs.len() <= limit {
            return Ok((runs, 0));
        }
        let needed = runs.len() - limit;
        let mut gaps = [Vec::new(), Vec::new()];
        for region in &self.ranges {
            let end = region.offset + region.length as u64;
            let start_index = runs.partition_point(|r| r.offset < region.offset);
            let end_index = runs.partition_point(|r| r.offset < end);
            for pair in runs[start_index..end_index].windows(2) {
                let start = pair[0].offset + pair[0].length as u64;
                let bytes = pair[1].offset - start;
                if bytes == self.page as u64 {
                    gaps[0].push(start);
                } else if bytes == 2 * self.page as u64 {
                    gaps[1].push(start);
                }
            }
        }
        let singles = needed.min(gaps[0].len());
        let doubles = needed - singles;
        let padding = singles + 2 * doubles;
        let data_pages = next.len() - self.padding_pages;
        if doubles > gaps[1].len() || self.padding_pages + padding > data_pages / 10 {
            return Err(io::Error::other(format!(
                "branch delta VMA budget exceeded: {} runs, {} one-page and {} two-page gaps, {} prior padding pages, {} data pages",
                runs.len(), gaps[0].len(), gaps[1].len(), self.padding_pages, data_pages
            )));
        }
        for (index, count) in [singles, doubles].into_iter().enumerate() {
            for &start in gaps[index].iter().take(count) {
                for page in 0..=index {
                    let offset = start + (page * self.page) as u64;
                    next.insert(offset);
                    added.insert(offset);
                }
            }
        }
        Ok((self.runs(next), padding))
    }

    /// # Safety
    /// All recorded mappings must still be exclusively owned and unchanged.
    /// No CPU/device/host writer or other pager may access RAM until return.
    /// On any error the caller must terminate, never resume or retry the VM.
    pub(super) unsafe fn offload(&mut self) -> Result<Report, String> {
        if self.poisoned {
            return Err("failed branch offload; terminate VMM".into());
        }
        self.poisoned = true;
        let result = unsafe { self.offload_inner() };
        if result.is_ok() {
            self.poisoned = false;
        }
        result.map_err(|e| format!("branch offload: {e}"))
    }

    unsafe fn offload_inner(&mut self) -> io::Result<Report> {
        let mut added = BTreeSet::new();
        let mut entries = [0u8; 8 * 1024];
        for range in &self.ranges {
            for begin in (0..range.length).step_by(self.page * 1024) {
                let count = ((range.length - begin) / self.page).min(1024);
                self.pagemap.read_exact_at(
                    &mut entries[..count * 8],
                    ((range.host + begin) / self.page) as u64 * 8,
                )?;
                for n in 0..count {
                    let offset = range.offset + (begin + n * self.page) as u64;
                    let entry = u64::from_ne_bytes(entries[n * 8..n * 8 + 8].try_into().unwrap());
                    if !self.private.contains(&offset) && needs_save(entry) {
                        added.insert(offset);
                    }
                }
            }
        }
        let mut next = self.private.union(&added).copied().collect::<BTreeSet<_>>();
        let (private_runs, padding) = self.compact_delta_runs(&mut next, &mut added, MAX_DELTA_RUNS)?;
        // Stage every new COW page before changing any mapping. Zero writes are
        // intentional: a modified-to-zero page overrides nonzero baseline bytes.
        let added_runs = self.runs(&added);
        for run in &added_runs {
            let bytes = unsafe { std::slice::from_raw_parts(run.host as *const u8, run.length) };
            self.delta.write_all_at(bytes, run.offset)?;
        }
        if !added_runs.is_empty() {
            self.delta.sync_data()?;
        }
        for run in &added_runs {
            let mapped = unsafe {
                libc::mmap(
                    run.host as *mut _,
                    run.length,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED | libc::MAP_FIXED,
                    self.delta.as_raw_fd(),
                    run.offset as libc::off_t,
                )
            };
            if mapped == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
        }
        self.private = next;
        self.padding_pages += padding;
        // Kernel dirty tracking writes only modified delta pages. Never msync or
        // fadvise the immutable common baseline: other branches may be using it.
        for run in &private_runs {
            if unsafe { libc::msync(run.host as *mut _, run.length, libc::MS_SYNC) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        for range in &self.ranges {
            if unsafe { libc::madvise(range.host as *mut _, range.length, libc::MADV_DONTNEED) }
                != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        for run in &private_runs {
            let err = unsafe {
                libc::posix_fadvise(
                    self.delta.as_raw_fd(),
                    run.offset as libc::off_t,
                    run.length as libc::off_t,
                    libc::POSIX_FADV_DONTNEED,
                )
            };
            if err != 0 {
                return Err(io::Error::from_raw_os_error(err));
            }
        }
        let logical_bytes = self.ranges.iter().map(|r| r.length as u64).sum();
        let private_backing_bytes = self.private.len() as u64 * self.page as u64;
        Ok(Report {
            logical_bytes,
            newly_private_bytes: added.len() as u64 * self.page as u64,
            private_backing_bytes,
            retained_base_bytes: logical_bytes - private_backing_bytes,
            delta_runs: private_runs.len(),
        })
    }
}
