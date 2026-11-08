//! HVF free-page reports are completed only after a quiesced host discard.
use crate::{api::VmmHandle, devices::virtio::memory_gate::SharedFreeReports, hvf::HvfVm};
use std::{io, sync::atomic::Ordering};
use vm_memory::{GuestAddress, GuestMemory, GuestMemoryMmap, GuestMemoryRegion};

#[derive(Debug, PartialEq)]
struct Range {
    region: u64,
    guest: u64,
    host: usize,
    bytes: usize,
}

fn ranges(
    memory: &GuestMemoryMmap,
    reported: &[(u64, usize)],
    page: usize,
) -> Result<Vec<Range>, String> {
    if !page.is_power_of_two() {
        return Err("invalid host page size".into());
    }
    let mut result: Vec<Range> = Vec::new();
    for &(address, length) in reported {
        let end = address
            .checked_add(length as u64)
            .ok_or("free-page range overflow")?;
        let region = memory
            .find_region(GuestAddress(address))
            .ok_or("free-page range outside RAM")?;
        if end
            > region
                .start_addr()
                .0
                .checked_add(region.len())
                .ok_or("RAM range overflow")?
        {
            return Err("free-page range crosses RAM boundary".into());
        }
        // File/COW snapshots, shared kernel mappings and device windows keep their own policy.
        if region.file_offset().is_some()
            || region.flags() & (libc::MAP_ANON | libc::MAP_PRIVATE)
                != (libc::MAP_ANON | libc::MAP_PRIVATE)
        {
            continue;
        }
        let host = memory
            .get_host_address(GuestAddress(address))
            .map_err(|e| e.to_string())? as usize;
        let start = host.checked_add(page - 1).ok_or("host range overflow")? & !(page - 1);
        let finish = host.checked_add(length).ok_or("host range overflow")? & !(page - 1);
        if finish > start {
            result.push(Range {
                region: region.start_addr().0,
                guest: address + (start - host) as u64,
                host: start,
                bytes: finish - start,
            });
        }
    }
    result.sort_unstable_by_key(|range| range.guest);
    let mut merged: Vec<Range> = Vec::new();
    for range in result {
        if let Some(previous) = merged.last_mut().filter(|previous| {
            previous.region == range.region && range.guest <= previous.guest + previous.bytes as u64
        }) {
            previous.bytes = previous
                .bytes
                .max((range.guest - previous.guest) as usize + range.bytes);
        } else {
            merged.push(range);
        }
    }
    Ok(merged)
}

/// The event callback has returned, so its descriptor/device leases are gone.
/// This runs on the normal event loop; no polling worker or periodic wake is added.
pub(crate) fn process(handle: &VmmHandle, reports: &SharedFreeReports) -> io::Result<()> {
    let Some(report) = reports.pending() else {
        return Ok(());
    };
    let result = handle.ram_quiesced(|vmm| {
        if handle.cold_pager_started.load(Ordering::Acquire)
            || !vmm.device_memory_gate().ordinary_balloon_reclaim_allowed()
        {
            return Ok(0);
        }
        // SAFETY: sysconf takes no pointers; an invalid result is rejected before mapping changes.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page <= 0 {
            return Err("cannot determine host page size".into());
        }
        let ranges = match ranges(vmm.guest_memory(), &report.ranges, page as usize) {
            Ok(ranges) => ranges,
            Err(error) => {
                warn!("balloon: rejected free-page report: {error}");
                return Ok(0);
            }
        };
        let mut bytes = 0;
        for range in ranges {
            // CPUs are paused, devices drained, and the guest still owns an uncompleted report.
            HvfVm {}
                .unmap_memory(range.guest, range.bytes as u64)
                .map_err(|e| format!("balloon HVF unmap: {e:?}"))?;
            // SAFETY: the range consists solely of reported free host pages. The original
            // HVA stays owned by GuestMemoryMmap; the gate excludes all pointer users.
            let mapped = unsafe {
                libc::mmap(
                    range.host as *mut _,
                    range.bytes,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_FIXED | libc::MAP_PRIVATE | libc::MAP_ANON,
                    -1,
                    0,
                )
            };
            if mapped == libc::MAP_FAILED {
                return Err(format!(
                    "balloon zero replacement: {}",
                    io::Error::last_os_error()
                ));
            }
            HvfVm {}
                .map_memory(range.host as u64, range.guest, range.bytes as u64)
                .map_err(|e| format!("balloon HVF remap: {e:?}"))?;
            bytes += range.bytes;
        }
        Ok(bytes)
    });
    match result {
        Ok(None) if report.submitted.elapsed() < std::time::Duration::from_secs(2) => {
            // Allow event callbacks and retained device buffers to finish before retrying.
            std::thread::sleep(std::time::Duration::from_millis(1));
            report.retry()
        }
        Ok(bytes) => {
            // Persistent pause/busy state must not indefinitely reserve guest free pages.
            if bytes.is_none() {
                warn!("balloon: skipped HVF discard after quiescence retry budget");
            }
            debug!("balloon: HVF released_bytes={}", bytes.unwrap_or(0));
            reports.complete(&report)
        }
        Err(error) => {
            // A partial mapping transaction must never resume or acknowledge page reuse.
            error!("balloon: unrecoverable mapping transition: {error}");
            std::process::exit(125);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planning_preserves_partial_host_pages_and_merges_only_owned_ranges() {
        let memory = GuestMemoryMmap::from_ranges(&[(GuestAddress(0x100000), 0x10000)]).unwrap();
        let base = memory.get_host_address(GuestAddress(0x100000)).unwrap() as usize;
        assert_eq!(base % 16384, 0);
        let plan = ranges(&memory, &[(0x100001, 0xbfff), (0x108000, 0x8000)], 16384).unwrap();
        assert_eq!(
            plan,
            vec![Range {
                region: 0x100000,
                guest: 0x104000,
                host: base + 0x4000,
                bytes: 0xc000
            }]
        );
        assert!(
            ranges(&memory, &[(0x100001, 16382)], 16384)
                .unwrap()
                .is_empty()
        );
        assert!(ranges(&memory, &[(u64::MAX - 2, 8)], 16384).is_err());
        assert!(ranges(&memory, &[(0x10f000, 0x2000)], 16384).is_err());
        assert!(ranges(&memory, &[(0x100000, 4096)], 0).is_err());
    }

    #[test]
    fn report_reuse_waits_for_completion_and_policy_requires_a_drained_gate() {
        use crate::devices::virtio::memory_gate;
        let memory = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 65536)]).unwrap();
        let gate = memory_gate::register(&memory);
        let reports = SharedFreeReports::default();
        let event = std::sync::Arc::new(
            crate::utils::eventfd::EventFd::new(crate::utils::eventfd::EFD_NONBLOCK).unwrap(),
        );
        let lease = memory_gate::balloon_access(&memory).unwrap();
        let report = reports.submit(vec![(0, 65536)], event.clone());
        assert!(!gate.try_close().unwrap());
        assert!(!report.is_done());
        assert!(!gate.ordinary_balloon_reclaim_allowed());
        report.retry().unwrap();
        assert_eq!(event.read().unwrap(), 1);
        assert!(!report.is_done());
        drop(lease);
        assert!(gate.try_close().unwrap());
        assert!(gate.ordinary_balloon_reclaim_allowed());
        gate.set_prepare(Some(std::sync::Arc::new(|_| Ok(()))))
            .unwrap();
        assert!(!gate.ordinary_balloon_reclaim_allowed());
        gate.set_prepare(None).unwrap();
        gate.open();
        reports.complete(&report).unwrap();
        assert!(report.is_done());
        assert!(reports.pending().is_none());
        assert_eq!(event.read().unwrap(), 1);
    }

    #[test]
    fn file_backed_ram_is_never_replaced() {
        let file = tempfile::tempfile().unwrap();
        file.set_len(65536).unwrap();
        let memory = GuestMemoryMmap::from_ranges_with_files([(
            GuestAddress(0x100000),
            65536,
            Some(vm_memory::FileOffset::new(file, 0)),
        )])
        .unwrap();
        assert!(
            ranges(&memory, &[(0x100000, 65536)], 16384)
                .unwrap()
                .is_empty()
        );
    }
}
