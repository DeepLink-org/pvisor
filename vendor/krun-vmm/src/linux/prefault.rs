//! Best-effort preparation of the loaded kernel and Linux's early page metadata.
//!
//! Only cold, file-backed libkrunfw boots call this module. Preparing a bounded
//! tail and the loader's kernel region avoids paying host faults and nested
//! page-table faults one guest page at a time during early boot, without eagerly
//! allocating all guest RAM.

use std::io;
use std::os::fd::RawFd;
use std::time::{Duration, Instant};
use vm_memory::{GuestMemory, GuestMemoryMmap, GuestMemoryRegion};

// Linux UAPI, not yet exposed by our pinned kvm-bindings/kvm-ioctls versions.
// https://docs.kernel.org/virt/kvm/api.html#kvm-pre-fault-memory
pub(crate) const KVM_CAP_PRE_FAULT_MEMORY: u64 = 236;
// _IOWR(KVMIO = 0xae, 0xd5, struct kvm_pre_fault_memory), x86_64 ABI.
const KVM_PRE_FAULT_MEMORY: libc::c_ulong = 0xc040_aed5;

#[repr(C)]
#[derive(Default)]
struct PreFaultMemory {
    gpa: u64,
    size: u64,
    flags: u64,
    padding: [u64; 5],
}
const _: () = assert!(std::mem::size_of::<PreFaultMemory>() == 64);

const MIB: u64 = 1024 * 1024;
const MAX_BYTES: u64 = 128 * MIB;
const MAX_KERNEL_BYTES: u64 = 16 * MIB;
const CHUNK_BYTES: u64 = 2 * MIB;
const MAX_TIME: Duration = Duration::from_millis(50);

#[derive(Debug)]
struct Range {
    gpa: u64,
    host: usize,
    size: usize,
}

fn plan(memory: &GuestMemoryMmap, page: u64, limit: u64) -> Vec<Range> {
    if !page.is_power_of_two() {
        return Vec::new();
    }
    let mut regions: Vec<_> = memory
        .iter()
        .filter(|r| {
            r.file_offset().is_some()
                && r.flags() & libc::MAP_SHARED != 0
                && r.flags() & libc::MAP_PRIVATE == 0
                && r.prot() & libc::PROT_WRITE != 0
                && r.start_addr().0 % page == 0
                && r.len() % page == 0
                && (r.as_ptr() as usize).is_multiple_of(page as usize)
        })
        .collect();
    let total: u64 = regions.iter().map(|r| r.len()).sum();
    // 4 KiB guest pages and roughly 64-byte struct page entries. The slack and
    // rounding cover early memblock allocations/alignment. This is a hint, not
    // an ABI assumption: uncovered pages fault normally, including large VMs.
    let budget = (total / 64 + 6 * MIB)
        .div_ceil(CHUNK_BYTES)
        .saturating_mul(CHUNK_BYTES)
        .min(limit)
        .min(total);
    let mut remaining = budget / page * page;
    regions.sort_unstable_by_key(|r| std::cmp::Reverse(r.start_addr().0));
    let mut ranges = Vec::new();
    for region in regions {
        let size = remaining.min(region.len());
        if size == 0 {
            break;
        }
        let offset = region.len() - size;
        ranges.push(Range {
            gpa: region.start_addr().0 + offset,
            host: region.as_ptr() as usize + offset as usize,
            size: size as usize,
        });
        remaining -= size;
    }
    ranges
}

fn boot_plan(memory: &GuestMemoryMmap, page: u64, kernel: Option<(u64, u64)>) -> Vec<Range> {
    if !page.is_power_of_two() {
        return Vec::new();
    }
    // The loader has already written this exact file-backed region. Preparing
    // its stage-2 mappings makes early instruction fetches and alternatives
    // patching avoid one nested page fault per kernel page. Do not infer a
    // kernel address from a slot number, and do not populate a restore's COW.
    let primed = kernel.and_then(|(address, size)| {
        memory
            .iter()
            .find(|r| {
                r.start_addr().0 == address
                    && r.len() == size
                    && r.file_offset().is_some()
                    && r.flags() & libc::MAP_SHARED != 0
                    && r.flags() & libc::MAP_PRIVATE == 0
                    && r.prot() & libc::PROT_WRITE != 0
                    && address % page == 0
                    && size % page == 0
                    && (r.as_ptr() as usize).is_multiple_of(page as usize)
            })
            .map(|r| Range {
                gpa: address,
                host: r.as_ptr() as usize,
                size: size.min(MAX_KERNEL_BYTES) as usize,
            })
    });
    let Some(primed) = primed else {
        return plan(memory, page, MAX_BYTES);
    };
    let end = primed.gpa + primed.size as u64;
    let tail = plan(memory, page, MAX_BYTES - primed.size as u64);
    let mut ranges = Vec::new();
    for range in tail {
        let range_end = range.gpa + range.size as u64;
        if range_end <= primed.gpa || range.gpa >= end {
            ranges.push(range);
        } else {
            if range.gpa < primed.gpa {
                ranges.push(Range {
                    gpa: range.gpa,
                    host: range.host,
                    size: (primed.gpa - range.gpa) as usize,
                });
            }
            if range_end > end {
                ranges.push(Range {
                    gpa: end,
                    host: range.host + (end - range.gpa) as usize,
                    size: (range_end - end) as usize,
                });
            }
        }
    }
    ranges.insert(0, primed);
    ranges
}

fn populate_host(range: &Range) -> io::Result<()> {
    // POPULATE_WRITE resolves writable host mappings, including holes in the
    // backing file, without overwriting guest bytes or changing mapping identity.
    let result = unsafe {
        libc::madvise(
            range.host as *mut libc::c_void,
            range.size,
            libc::MADV_POPULATE_WRITE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn populate_kvm(
    range: &Range,
    page: u64,
    deadline: Instant,
    ioctl: &mut impl FnMut(&mut PreFaultMemory) -> io::Result<()>,
) -> io::Result<()> {
    let end = range.gpa + range.size as u64;
    let mut gpa = range.gpa;
    let mut calls = 0;
    while gpa < end {
        let mut request = PreFaultMemory {
            gpa,
            size: (end - gpa).min(CHUNK_BYTES),
            ..Default::default()
        };
        while request.size != 0 {
            if Instant::now() >= deadline || calls >= 4096 {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "prefault budget exhausted",
                ));
            }
            calls += 1;
            let old_gpa = request.gpa;
            let old_size = request.size;
            // EINTR means no page was processed. Stop rather than retrying a
            // pending signal indefinitely; all remaining faults are demand faults.
            ioctl(&mut request)?;
            if request.size >= old_size
                || request.gpa != old_gpa + old_size - request.size
                || !request.gpa.is_multiple_of(page)
                || !request.size.is_multiple_of(page)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid KVM prefault progress",
                ));
            }
        }
        gpa = request.gpa;
    }
    Ok(())
}

pub(crate) fn prepare(vcpu: RawFd, memory: &GuestMemoryMmap, kernel: Option<(u64, u64)>) {
    let start = Instant::now();
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page <= 0 {
        return;
    }
    let page = page as u64;
    let ranges = boot_plan(memory, page, kernel);
    let mut host_us = 0;
    let mut kvm_us = 0;
    let mut bytes = 0;
    let mut outcome = Ok(());
    // Bound work between syscalls as well as total planned bytes. A single
    // syscall can still wait for the host scheduler or backing-file I/O.
    'prepare: for range in &ranges {
        for offset in (0..range.size).step_by(CHUNK_BYTES as usize) {
            if start.elapsed() >= MAX_TIME {
                outcome = Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "prefault budget exhausted",
                ));
                break 'prepare;
            }
            let chunk = Range {
                gpa: range.gpa + offset as u64,
                host: range.host + offset,
                size: (range.size - offset).min(CHUNK_BYTES as usize),
            };
            let step = Instant::now();
            outcome = populate_host(&chunk);
            host_us += step.elapsed().as_micros();
            if outcome.is_err() {
                break 'prepare;
            }
            let step = Instant::now();
            outcome = populate_kvm(&chunk, page, start + MAX_TIME, &mut |request| {
                let result = unsafe { libc::ioctl(vcpu, KVM_PRE_FAULT_MEMORY as _, request) };
                if result == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            });
            kvm_us += step.elapsed().as_micros();
            if outcome.is_err() {
                break 'prepare;
            }
            bytes += chunk.size;
        }
    }
    if std::env::var("PVISOR_STARTUP_TIMING").as_deref() == Ok("1")
        || std::env::var("KRUN_BOOT_PREFAULT_DIAGNOSTICS").as_deref() == Ok("1")
    {
        eprintln!(
            "libkrun-boot-prefault bytes={bytes} planned_bytes={} host_us={host_us} kvm_us={kvm_us} outcome={}",
            ranges.iter().map(|r| r.size).sum::<usize>(),
            outcome
                .map(|()| "ok".to_owned())
                .unwrap_or_else(|e| format!("fallback:{e}"))
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd;
    use std::sync::Arc;

    fn backing() -> Arc<std::fs::File> {
        let fd = unsafe { libc::memfd_create(c"prefault-test".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(fd >= 0, "{}", io::Error::last_os_error());
        Arc::new(unsafe { std::fs::File::from_raw_fd(fd) })
    }
    use vm_memory::{Bytes, FileOffset, GuestAddress, GuestRegionMmap, MmapRegion};

    #[test]
    fn preparation_preserves_bytes_and_skips_dax_private_and_readonly_regions() {
        let file = backing();
        file.set_len(16 * MIB).unwrap();
        let backed = |gpa, offset, flags, prot| {
            let region = MmapRegion::build(
                Some(FileOffset::from_arc(file.clone(), offset)),
                4 * MIB as usize,
                prot,
                flags,
            )
            .unwrap();
            GuestRegionMmap::new(region, GuestAddress(gpa)).unwrap()
        };
        let memory = GuestMemoryMmap::from_regions(vec![
            backed(0, 0, libc::MAP_SHARED, libc::PROT_READ | libc::PROT_WRITE),
            backed(
                8 * MIB,
                4 * MIB,
                libc::MAP_PRIVATE,
                libc::PROT_READ | libc::PROT_WRITE,
            ),
            backed(16 * MIB, 8 * MIB, libc::MAP_SHARED, libc::PROT_READ),
            GuestRegionMmap::new(
                MmapRegion::new(4 * MIB as usize).unwrap(),
                GuestAddress(32 * MIB),
            )
            .unwrap(),
        ])
        .unwrap();
        memory
            .write_slice(b"keep shared bytes", GuestAddress(2 * MIB))
            .unwrap();
        memory
            .write_slice(b"keep COW bytes", GuestAddress(8 * MIB))
            .unwrap();
        let ranges = plan(&memory, 4096, MAX_BYTES);
        assert_eq!(ranges.len(), 1);
        assert_eq!((ranges[0].gpa, ranges[0].size), (0, 4 * MIB as usize));
        if let Err(e) = populate_host(&ranges[0]) {
            assert!(matches!(
                e.raw_os_error(),
                Some(libc::EINVAL | libc::EOPNOTSUPP)
            ));
        }
        let mut shared = [0; 17];
        memory
            .read_slice(&mut shared, GuestAddress(2 * MIB))
            .unwrap();
        assert_eq!(&shared, b"keep shared bytes");
        let mut private = [0; 14];
        memory
            .read_slice(&mut private, GuestAddress(8 * MIB))
            .unwrap();
        assert_eq!(&private, b"keep COW bytes");
    }

    #[test]
    fn sparse_multislot_plan_clips_to_ram_and_caps_large_guests() {
        let file = backing();
        file.set_len(16 * 1024 * MIB).unwrap();
        let memory = GuestMemoryMmap::from_ranges_with_files(&[
            (
                GuestAddress(0),
                16 * 1024 * MIB as usize,
                Some(FileOffset::from_arc(file.clone(), 0)),
            ),
            (
                GuestAddress(32 * 1024 * MIB),
                4 * MIB as usize,
                Some(FileOffset::from_arc(file, 0)),
            ),
        ])
        .unwrap();
        let ranges = plan(&memory, 4096, MAX_BYTES);
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges.iter().map(|r| r.size as u64).sum::<u64>(), MAX_BYTES);
        assert_eq!(ranges[0].gpa, 32 * 1024 * MIB);
        assert_eq!(ranges[1].gpa + ranges[1].size as u64, 16 * 1024 * MIB);
        assert!(plan(&memory, 0, MAX_BYTES).is_empty());
    }

    #[test]
    fn loaded_kernel_and_ram_tail_are_disjoint_and_share_the_size_budget() {
        for ram_size in [4 * MIB, 16 * 1024 * MIB] {
            let file = backing();
            file.set_len(MAX_KERNEL_BYTES + ram_size).unwrap();
            let address = 16 * MIB;
            let memory = GuestMemoryMmap::from_ranges_with_files(&[
                (
                    GuestAddress(address),
                    MAX_KERNEL_BYTES as usize,
                    Some(FileOffset::from_arc(file.clone(), 0)),
                ),
                (
                    GuestAddress(64 * MIB),
                    ram_size as usize,
                    Some(FileOffset::from_arc(file, MAX_KERNEL_BYTES)),
                ),
            ])
            .unwrap();
            let mut ranges = boot_plan(&memory, 4096, Some((address, MAX_KERNEL_BYTES)));
            assert_eq!(
                (ranges[0].gpa, ranges[0].size),
                (address, MAX_KERNEL_BYTES as usize)
            );
            assert!(ranges.iter().map(|r| r.size as u64).sum::<u64>() <= MAX_BYTES);
            ranges.sort_by_key(|r| r.gpa);
            assert!(ranges
                .windows(2)
                .all(|r| r[0].gpa + r[0].size as u64 <= r[1].gpa));
            // An address that is not the loader's exact RAM region gets no
            // special treatment; it cannot prime a hole or another mapping.
            let fallback = boot_plan(&memory, 4096, Some((address + 4096, MAX_KERNEL_BYTES)));
            assert_eq!(
                fallback[0].gpa + fallback[0].size as u64,
                64 * MIB + ram_size
            );
        }
    }

    #[test]
    fn partial_progress_is_resumed_but_errors_and_no_progress_do_not_spin() {
        let range = Range {
            gpa: 0x100000,
            host: 0,
            size: 3 * MIB as usize,
        };
        let mut next = range.gpa;
        populate_kvm(
            &range,
            4096,
            Instant::now() + Duration::from_secs(1),
            &mut |r| {
                assert_eq!(r.gpa, next);
                assert!(r.size <= CHUNK_BYTES);
                assert_eq!(r.flags, 0);
                assert_eq!(r.padding, [0; 5]);
                let processed = r.size.min(4096);
                r.gpa += processed;
                r.size -= processed;
                next = r.gpa;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(next, range.gpa + range.size as u64);
        for errno in [libc::EINTR, libc::ENOTTY, libc::EOPNOTSUPP, libc::ENOMEM] {
            let mut calls = 0;
            let error = populate_kvm(
                &range,
                4096,
                Instant::now() + Duration::from_secs(1),
                &mut |_| {
                    calls += 1;
                    Err(io::Error::from_raw_os_error(errno))
                },
            )
            .unwrap_err();
            assert_eq!(calls, 1);
            assert_eq!(error.raw_os_error(), Some(errno));
        }
        let error = populate_kvm(
            &range,
            4096,
            Instant::now() + Duration::from_secs(1),
            &mut |_| Ok(()),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let error = populate_kvm(&range, 4096, Instant::now(), &mut |_| {
            panic!("expired budget must not issue ioctl")
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
