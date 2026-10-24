//! Native-page merging. Every slot stays pinned until its mapping is replaced;
//! publication and reclamation never hold the CPU/device barrier.
use crate::api::{ColdRamControl, ColdRamStore, FrozenMemory, RamAccess, RamBlock, VmmHandle};
use crate::cold_ram_scan::{self as scan, deadline, quiesce_nonempty};
use std::{io, sync::atomic::Ordering, time::Instant};

struct Page<O> {
    block: RamBlock,
    pinned: Option<(O, [u64; 2])>,
    cow_observed: bool,
    detached: bool,
    eligible_at: u64,
}

fn fatal(error: impl std::fmt::Display) -> ! {
    eprintln!("physical RAM sharing failed: {error}");
    unsafe { libc::_exit(125) }
}

pub(crate) fn start<S: ColdRamStore + 'static>(
    handle: VmmHandle,
    store: S,
    metrics: bool,
) -> io::Result<()> {
    if cfg!(any(
        feature = "tee",
        feature = "aws-nitro",
        feature = "efi",
        feature = "gpu",
        feature = "snd",
        feature = "input"
    )) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "HVF physical sharing requires plain CPU/device RAM",
        ));
    }
    {
        let _transition = handle
            .transition
            .lock()
            .map_err(|_| io::Error::other("VM transition poisoned"))?;
        let live = handle
            .vmm
            .upgrade()
            .ok_or_else(|| io::Error::other("VMM has stopped"))?;
        let vmm = live.lock().map_err(|_| io::Error::other("VMM poisoned"))?;
        if vmm.device_memory_gate().has_dedup_advice() || vmm.device_memory_gate().has_prepare() {
            return Err(io::Error::other(
                "physical sharing conflicts with existing RAM preparation/advice",
            ));
        }
        handle
            .cold_pager_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                io::Error::new(io::ErrorKind::AlreadyExists, "RAM pager already started")
            })?;
    }
    let reserved = handle.cold_pager_started.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("pvisor-physical-pager".into())
        .spawn(move || run(handle, store, metrics))
    {
        reserved.store(false, Ordering::Release);
        return Err(error);
    }
    Ok(())
}

// Requires the CPU/device barrier: Mach metadata alone does not authorize a copy.
fn sample<O>(
    pages: &mut [Page<O>],
    scanner: &mut scan::Scanner,
    bytes: usize,
) -> Result<Vec<(usize, Vec<u8>)>, String> {
    let started = Instant::now();
    let candidates = scanner.select(scan::BATCH_BYTES / bytes, started, |visit, samples| {
        for index in visit.pages {
            if started.elapsed() >= scan::WORK_BUDGET {
                break;
            }
            debug_assert!(visit.region.contains(&index));
            let page = &pages[index];
            if deadline(started) < page.eligible_at {
                continue;
            }
            let Some(identity) = page.block.inner.resident_identity()? else {
                continue;
            };
            if page
                .pinned
                .as_ref()
                .is_some_and(|(_, shared)| *shared == identity)
            {
                continue;
            }
            let mut data = vec![0; bytes];
            unsafe {
                page.block.snapshot(&mut data).map_err(|e| e.to_string())?;
            }
            samples.push((index, data));
        }
        Ok::<_, String>(())
    })?;
    for (index, _) in &candidates {
        scanner.sampled(*index);
        let page = &mut pages[*index];
        if page.pinned.is_some() && !page.cow_observed {
            page.cow_observed = true;
            scanner.cow(*index);
        }
    }
    Ok(candidates)
}

fn run<S: ColdRamStore>(handle: VmmHandle, mut store: S, metrics: bool) {
    let native = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if native != 16384 {
        fatal("physical sharing requires 16 KiB native pages");
    }
    let bytes = native as usize;
    let mut pages = loop {
        match handle.with_ram_quiesced(|machine| {
            let blocks = machine.experimental_ram_blocks(bytes)?;
            if blocks.is_empty() {
                return Err("physical sharing requires ordinary file-backed RAM".into());
            }
            let eligible_at = deadline(Instant::now() + std::time::Duration::from_secs(1));
            Ok(blocks
                .into_iter()
                .map(|block| Page {
                    block,
                    pinned: None,
                    cow_observed: false,
                    detached: false,
                    eligible_at,
                })
                .collect::<Vec<Page<S::Object>>>())
        }) {
            Ok(Some(pages)) => break pages,
            Ok(None) => std::thread::sleep(scan::REJECT_RETRY),
            Err(error) if error == "VMM has stopped" => return,
            Err(error) => fatal(error),
        }
    };
    // Each block retains the original mapping's file Arc, including across
    // later MAP_FIXED replacements. Never group across a kernel/device gap.
    let mut first = 0;
    let mappings = pages
        .chunk_by(|a, b| a.block.inner.same_mapping(&b.block.inner))
        .map(|group| {
            let range = first..first + group.len();
            first = range.end;
            range
        });
    let mut scanner = scan::Scanner::new(mappings, bytes, rand::random());
    let mut scratch = vec![0; bytes];
    let mut rejections = 0u64;
    let mut mapped = 0u64;
    let mut privatized = 0u64;
    loop {
        std::thread::sleep(scanner.delay());
        if handle.vmm.upgrade().is_none() {
            return;
        }
        scanner.begin_round();
        let candidates = if scanner.round_complete() {
            Vec::new()
        } else {
            match handle.with_ram_quiesced(|_| sample(&mut pages, &mut scanner, bytes)) {
                Ok(Some(candidates)) => candidates,
                Ok(None) => {
                    std::thread::sleep(scan::REJECT_RETRY);
                    continue;
                }
                Err(error) if error == "VMM has stopped" => return,
                Err(error) => fatal(error),
            }
        };
        let mut published = Vec::with_capacity(candidates.len());
        for (index, data) in candidates {
            let object = match store.put(&data) {
                Ok(object) => {
                    let mapping = store
                        .shared_mapping(&object)
                        .unwrap_or_else(|| fatal("store omitted pinned mapping"));
                    Some((object, mapping))
                }
                Err(_) => {
                    rejections += 1;
                    pages[index].eligible_at = deadline(Instant::now() + scan::REJECT_RETRY);
                    None
                }
            };
            published.push((index, data, object));
        }
        let mut unused = Vec::new();
        let mut detached = Vec::new();
        let result = quiesce_nonempty(&mut published, |published| {
            handle.with_ram_quiesced(|_| {
                let started = Instant::now();
                for (index, data, object) in published {
                    if started.elapsed() >= scan::WORK_BUDGET {
                        break;
                    }
                    let page = &mut pages[*index];
                    unsafe {
                        page.block
                            .snapshot(&mut scratch)
                            .map_err(|e| e.to_string())?;
                    }
                    // Publication grants no authority over bytes changed in flight.
                    if scratch != *data {
                        scanner.invalidated(*index);
                        page.eligible_at = deadline(Instant::now() + scan::INVALIDATED_RETRY);
                        continue;
                    }
                    if let Some((object, mapping)) = object.take() {
                        unsafe {
                            page.block.inner.map_shared(&mapping)?;
                        }
                        let identity = page
                            .block
                            .inner
                            .resident_identity()?
                            .ok_or("shared page not resident after publication")?;
                        if let Some((old, _)) = page.pinned.replace((object, identity)) {
                            unused.push(old);
                        }
                        page.cow_observed = false;
                        scanner.merged(*index);
                        mapped += 1;
                    } else if page.pinned.is_some() {
                        // COW still has a shadow reference to the source file.
                        // Remove that relationship before allowing slot reuse.
                        unsafe {
                            page.block.discard()?;
                            page.block.restore(data)?;
                        }
                        unused.push(page.pinned.take().unwrap().0);
                        page.cow_observed = false;
                        privatized += 1;
                    } else {
                        continue;
                    }
                    if !page.detached {
                        page.detached = true;
                        detached.push(page.block.clone());
                    }
                }
                Ok(())
            })
        });
        match result {
            Ok(_) => {}
            Err(error) if error == "VMM has stopped" => return,
            Err(error) => fatal(error),
        }
        for (_, _, object) in published {
            if let Some((object, _)) = object {
                unused.push(object);
            }
        }
        for object in unused {
            store.release(object).unwrap_or_else(|e| fatal(e));
        }
        for block in detached {
            unsafe {
                block.reclaim_file().unwrap_or_else(|e| fatal(e));
            }
        }
        // Also proves transport health after a complete capacity rejection.
        let stats = store.stats().unwrap_or_else(|e| fatal(e));
        if metrics {
            let shared = pages.iter().filter(|page| page.pinned.is_some()).count() * bytes;
            eprintln!(
                "pvisor-shared-sample pid={} shared_bytes={shared} mapped_total={mapped} privatized_total={privatized} pool_bytes={} pool_objects={} references={} cross_session_objects={} rejections={rejections}",
                std::process::id(),
                stats.encoded_bytes,
                stats.objects,
                stats.session_references,
                stats.cross_session_objects
            );
            scanner.report(0);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires a signed HVF test binary"]
    fn hvf_live_remap_preserves_cpu_device_cow_and_instruction_reads() {
        use crate::hvf::{HvfVcpu, HvfVm, bindings::*};
        use std::{fs::File, io::Write, os::unix::fs::FileExt, sync::Arc};
        use vm_memory::{Bytes, GuestAddress, GuestMemory, GuestMemoryMmap};
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let file = Arc::new(tempfile::tempfile().unwrap());
        let memory =
            crate::vmm::ram::map(&[(GuestAddress(0x200000), page * 2)], &[], file).unwrap();
        memory
            .write_slice(&vec![0x53; page * 2], GuestAddress(0x200000))
            .unwrap();
        let mut source = tempfile::NamedTempFile::new().unwrap();
        let mut shared_bytes = vec![0x53; page];
        shared_bytes[64..68].copy_from_slice(&0xf9400023u32.to_le_bytes());
        shared_bytes[68..72].copy_from_slice(&0xd4000002u32.to_le_bytes());
        source.write_all(&shared_bytes).unwrap();
        for offset in [0, page] {
            memory
                .write_slice(&shared_bytes, GuestAddress(0x200000 + offset as u64))
                .unwrap();
        }
        let readonly = Arc::new(File::open(source.path()).unwrap());
        let code = GuestMemoryMmap::<()>::from_ranges(&[(GuestAddress(0x100000), page)]).unwrap();
        for (i, word) in [0xf9400023u32, 0xd4000002, 0xf9000022, 0xd4000002]
            .iter()
            .enumerate()
        {
            code.write_slice(&word.to_le_bytes(), GuestAddress(0x100000 + i as u64 * 4))
                .unwrap();
        }
        let code_host = code.get_host_address(GuestAddress(0x100000)).unwrap();
        unsafe extern "C" {
            fn sys_icache_invalidate(start: *mut libc::c_void, bytes: usize);
        }
        unsafe {
            sys_icache_invalidate(code_host.cast(), 16);
        }
        let vm = HvfVm::new(false).unwrap();
        vm.map_memory(code_host as u64, 0x100000, page as u64)
            .unwrap();
        vm.map_memory(
            memory.get_host_address(GuestAddress(0x200000)).unwrap() as u64,
            0x200000,
            (page * 2) as u64,
        )
        .unwrap();
        let cpu = HvfVcpu::new(0, false).unwrap();
        cpu.set_initial_state(0x100000, 0).unwrap();
        let read = |address| {
            cpu.write_reg(hv_reg_t_HV_REG_PC, 0x100000).unwrap();
            cpu.write_reg(hv_reg_t_HV_REG_X1, address).unwrap();
            assert_eq!(unsafe { hv_vcpu_run(cpu.id()) }, HV_SUCCESS);
            let mut value = 0;
            assert_eq!(
                unsafe { hv_vcpu_get_reg(cpu.id(), hv_reg_t_HV_REG_X3, &mut value) },
                HV_SUCCESS
            );
            value
        };
        assert_eq!(read(0x200000), 0x5353535353535353);
        let blocks = crate::vmm::ram::blocks(&memory, page).unwrap();
        let mapping = crate::api::SharedRamMapping {
            file: readonly.clone(),
            offset: 0,
        };
        // Each hv_vcpu_run has returned; there are no device workers in this VM.
        for block in &blocks {
            unsafe {
                block.map_shared(&mapping).unwrap();
            }
        }
        assert_eq!(
            blocks[0].resident_identity().unwrap(),
            blocks[1].resident_identity().unwrap()
        );
        let mut pages: Vec<_> = blocks
            .iter()
            .map(|block| super::Page {
                block: crate::api::RamBlock {
                    inner: block.clone(),
                },
                pinned: Some(((), block.resident_identity().unwrap().unwrap())),
                cow_observed: false,
                detached: true,
                eligible_at: 0,
            })
            .collect();
        let mut scanner = crate::cold_ram_scan::Scanner::new(std::iter::once(0..2), page, 7);
        assert!(
            super::sample(&mut pages, &mut scanner, page)
                .unwrap()
                .is_empty(),
            "the adaptive scanner must skip unchanged shared mappings"
        );
        assert_eq!(read(0x200000), 0x5353535353535353);
        cpu.write_reg(hv_reg_t_HV_REG_PC, 0x200040).unwrap();
        cpu.write_reg(hv_reg_t_HV_REG_X1, 0x200000 + page as u64)
            .unwrap();
        assert_eq!(unsafe { hv_vcpu_run(cpu.id()) }, HV_SUCCESS);
        let mut fetched = 0;
        assert_eq!(
            unsafe { hv_vcpu_get_reg(cpu.id(), hv_reg_t_HV_REG_X3, &mut fetched) },
            HV_SUCCESS
        );
        assert_eq!(
            fetched, 0x5353535353535353,
            "instructions fetched from newly shared RAM"
        );
        cpu.write_reg(hv_reg_t_HV_REG_PC, 0x100008).unwrap();
        cpu.write_reg(hv_reg_t_HV_REG_X1, 0x200000).unwrap();
        cpu.write_reg(hv_reg_t_HV_REG_X2, 0xa1).unwrap();
        assert_eq!(unsafe { hv_vcpu_run(cpu.id()) }, HV_SUCCESS);
        assert_eq!(read(0x200000), 0xa1);
        assert_eq!(read(0x200000 + page as u64), 0x5353535353535353);
        assert_ne!(
            blocks[0].resident_identity().unwrap(),
            blocks[1].resident_identity().unwrap()
        );
        memory
            .write_slice(&0xb2u64.to_le_bytes(), GuestAddress(0x200000 + page as u64))
            .unwrap();
        assert_eq!(
            read(0x200000 + page as u64),
            0xb2,
            "guest must see device-style COW writes after remapping"
        );
        assert_eq!(read(0x200000), 0xa1);
        scanner.begin_round();
        let samples = super::sample(&mut pages, &mut scanner, page).unwrap();
        assert_eq!(samples.len(), 2, "CPU and device COW must both be sampled");
        assert_eq!(scanner.cow_breaks, 2);
        for (index, bytes) in samples {
            assert_eq!(
                u64::from_le_bytes(bytes[..8].try_into().unwrap()),
                if index == 0 { 0xa1 } else { 0xb2 }
            );
        }
        scanner.begin_round();
        super::sample(&mut pages, &mut scanner, page).unwrap();
        assert_eq!(
            scanner.cow_breaks, 2,
            "a retained old pin must not count the same COW twice"
        );
        pages[0].eligible_at = crate::cold_ram_scan::deadline(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        );
        scanner.begin_round();
        let samples = super::sample(&mut pages, &mut scanner, page).unwrap();
        assert_eq!(
            samples.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            [1],
            "cooling pages must not be copied"
        );
        assert_eq!(std::fs::read(source.path()).unwrap(), shared_bytes);
        // Removing the shadow relationship must preserve private bytes before
        // a pool is allowed to overwrite the old shared slot.
        let mut private = vec![0; page];
        unsafe {
            blocks[0].snapshot(&mut private).unwrap();
            blocks[0].discard().unwrap();
            blocks[0].restore(&private).unwrap();
        }
        source.as_file().write_all_at(&vec![0x77; page], 0).unwrap();
        assert_eq!(read(0x200000), 0xa1);
        assert_eq!(read(0x200000 + page as u64), 0xb2);
        assert_eq!(unsafe { hv_vcpu_destroy(cpu.id()) }, HV_SUCCESS);
        vm.unmap_memory(0x100000, page as u64).unwrap();
        vm.unmap_memory(0x200000, (page * 2) as u64).unwrap();
        assert_eq!(unsafe { hv_vm_destroy() }, HV_SUCCESS);
    }
}
