//! Best-effort region registration, never mapping replacement or global policy.
use crate::api::{
    RamDedupControl, RamDedupMapping, RamDedupReport, RamDedupSkipReason, RamDedupStatus, VmmHandle,
};
#[cfg(any(target_os = "linux", test))]
use std::io;
use vm_memory::{GuestMemory, GuestMemoryMmap, GuestMemoryRegion};

impl RamDedupControl for VmmHandle {
    fn advise_ram_dedup(&self) -> Result<RamDedupReport, String> {
        let _transition = self
            .transition
            .lock()
            .map_err(|_| "VM transition lock poisoned")?;
        let vmm = self.vmm.upgrade().ok_or("VMM has stopped")?;
        let locked = vmm.lock().map_err(|_| "VMM lock poisoned")?;
        let cold = self
            .cold_pager_started
            .load(std::sync::atomic::Ordering::Acquire);
        locked
            .device_memory_gate()
            .advise_dedup(|prepared| {
                let blocked = if cold {
                    Some(RamDedupSkipReason::ColdPagerActive)
                } else if prepared {
                    Some(RamDedupSkipReason::DevicePreparationActive)
                } else {
                    None
                };
                let report = advise(
                    locked.guest_memory(),
                    locked.ram_device_window_start(),
                    blocked,
                    mergeable,
                );
                let accepted = report.accepted_bytes != 0;
                (report, accepted)
            })
            .map_err(str::to_owned)
    }
}

fn eligibility(
    flags: i32,
    prot: i32,
    address: usize,
    length: usize,
    page: usize,
    device: bool,
) -> Option<RamDedupSkipReason> {
    if device {
        return Some(RamDedupSkipReason::DeviceWindow);
    }
    if flags & libc::MAP_SHARED != 0 {
        return Some(RamDedupSkipReason::SharedMapping);
    }
    if flags & libc::MAP_PRIVATE == 0 {
        return Some(RamDedupSkipReason::NotPrivate);
    }
    #[cfg(target_os = "linux")]
    if flags & libc::MAP_HUGETLB != 0 {
        return Some(RamDedupSkipReason::HugePages);
    }
    if prot & (libc::PROT_READ | libc::PROT_WRITE) != libc::PROT_READ | libc::PROT_WRITE {
        return Some(RamDedupSkipReason::NotWritable);
    }
    if page == 0 || length == 0 || !address.is_multiple_of(page) || !length.is_multiple_of(page) {
        return Some(RamDedupSkipReason::Unaligned);
    }
    None
}

fn advise(
    memory: &GuestMemoryMmap,
    device_start: u64,
    blocked: Option<RamDedupSkipReason>,
    mut advice: impl FnMut(*mut u8, usize) -> RamDedupStatus,
) -> RamDedupReport {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page = usize::try_from(page).unwrap_or(0);
    let mut report = RamDedupReport {
        accepted_bytes: 0,
        mappings: Vec::new(),
    };
    for region in memory.iter() {
        let base = region.start_addr().0;
        let length = region.len();
        // Architecture layouts place device windows at/above shm_start_addr.
        // Zero denotes layouts without a usable ordinary-RAM boundary (TEE).
        let device = base
            .checked_add(length)
            .is_none_or(|end| end > device_start);
        let skip = eligibility(
            region.flags(),
            region.prot(),
            region.as_ptr() as usize,
            length as usize,
            page,
            device,
        )
        .or(blocked);
        let status = match skip {
            Some(reason) => RamDedupStatus::Skipped(reason),
            None => advice(region.as_ptr(), length as usize),
        };
        if status == RamDedupStatus::Accepted {
            report.accepted_bytes += length;
        }
        report.mappings.push(RamDedupMapping {
            guest_address: base,
            length,
            status,
        });
    }
    report
}

#[cfg(target_os = "linux")]
fn mergeable(address: *mut u8, length: usize) -> RamDedupStatus {
    // The GuestMemory owner and VMM/transition locks pin this range. madvise
    // preserves bytes and addresses; kernel COW serializes concurrent writes.
    if unsafe { libc::madvise(address.cast(), length, libc::MADV_MERGEABLE) } == 0 {
        RamDedupStatus::Accepted
    } else {
        advice_error(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn mergeable(_address: *mut u8, _length: usize) -> RamDedupStatus {
    RamDedupStatus::Unsupported {
        errno: None,
        reason: "Linux KSM advice is unsupported on this platform".into(),
    }
}

#[cfg(any(target_os = "linux", test))]
fn advice_error(error: io::Error) -> RamDedupStatus {
    let errno = error.raw_os_error();
    let reason = error.to_string();
    // EINVAL also covers a kernel built without CONFIG_KSM. Eligibility is
    // checked first; retaining errno avoids claiming any particular cause.
    if matches!(errno, Some(libc::ENOSYS | libc::EOPNOTSUPP | libc::EINVAL)) {
        RamDedupStatus::Unsupported { errno, reason }
    } else {
        RamDedupStatus::Error { errno, reason }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::virtio::memory_gate::MemoryGate;
    use std::{
        fs::File,
        io::{Read, Write},
        sync::Arc,
    };
    use vm_memory::{Bytes, GuestAddress};

    /// Benchmark: B-MEMORY-DIAG (benchmark/README.md#b-memory-diag), role diagnostic.
    /// Motivation: distinguish baseline sharing, advice registration and reclaim.
    /// Conclusion sought: address-scoped counters and full recovery correctness.
    /// Design: three fresh 64 MiB trials, disabled KSM, complete byte verification.
    /// Special diagnostic runner: cargo test -p pvisor-vm --lib
    /// ram_dedup::tests::memory_diagnostic -- --exact --ignored --nocapture --test-threads=1
    /// Run only after B-MEMORY-DIAG registration. No KVM, FUSE, or sysfs writes.
    #[test]
    #[cfg(target_os = "linux")]
    #[ignore = "B-MEMORY-DIAG: opt-in address-scoped memory diagnostic"]
    fn memory_diagnostic() {
        use serde_json::{json, Value};
        use std::{
            os::fd::AsRawFd,
            os::unix::fs::MetadataExt,
            time::{Duration, Instant},
        };

        const SIZE: usize = 64 * 1024 * 1024;
        const CHUNK: usize = 1024 * 1024;
        const SEED: u64 = 0x2026_1006;
        let started = Instant::now();
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
        assert!(page > 0 && SIZE.is_multiple_of(page) && CHUNK.is_multiple_of(page));
        let ksm_run = || {
            std::fs::read_to_string("/sys/kernel/mm/ksm/run")
                .ok()
                .map(|s| s.trim().to_owned())
        };
        let initial_run = ksm_run();
        assert_eq!(
            initial_run.as_deref(),
            Some("0"),
            "diagnostic requires host KSM run=0; never changes sysfs"
        );
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        assert!(directory.is_dir(), "build target directory must exist");
        let disk = tempfile::tempdir_in(&directory).unwrap();
        let probe = tempfile::tempfile_in(disk.path()).unwrap();
        let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
        assert_eq!(
            unsafe { libc::fstatfs(probe.as_raw_fd(), fs.as_mut_ptr()) },
            0
        );
        let fs = unsafe { fs.assume_init() };
        assert!(
            !matches!(fs.f_type as u64, 0x0102_1994 | 0x8584_58f6 | 0x6573_5546),
            "diagnostic backing must not be tmpfs, ramfs, or FUSE"
        );

        fn report_json(report: RamDedupReport) -> Value {
            let mappings: Vec<_> = report.mappings.into_iter().map(|mapping| {
                let status = match mapping.status {
                    RamDedupStatus::Accepted => json!({"kind": "accepted"}),
                    RamDedupStatus::Skipped(reason) => json!({"kind": "skipped", "reason": format!("{reason:?}")}),
                    RamDedupStatus::Unsupported { errno, reason } => json!({"kind": "unsupported", "errno": errno, "reason": reason}),
                    RamDedupStatus::Error { errno, reason } => json!({"kind": "error", "errno": errno, "reason": reason}),
                };
                json!({"guest_address": mapping.guest_address, "length": mapping.length, "status": status})
            }).collect();
            json!({"accepted_bytes": report.accepted_bytes, "mappings": mappings})
        }
        fn checksum(bytes: &[u8]) -> u64 {
            bytes.as_chunks::<8>().0.iter().fold(0u64, |sum, word| {
                sum.wrapping_add(u64::from_le_bytes(*word))
            })
        }
        fn verify(memory: &GuestMemoryMmap, expected: &[u8]) -> u64 {
            let mut buffer = vec![0; CHUNK];
            let mut sum = 0u64;
            for offset in (0..SIZE).step_by(CHUNK) {
                memory
                    .read_slice(&mut buffer, GuestAddress(offset as u64))
                    .unwrap();
                assert_eq!(buffer, expected, "full-byte verification at {offset}");
                sum = sum.wrapping_add(checksum(&buffer));
            }
            assert_eq!(sum, checksum(expected).wrapping_mul((SIZE / CHUNK) as u64));
            sum
        }
        fn verify_file(file: &File, expected: &[u8]) {
            use std::os::unix::fs::FileExt;
            let mut buffer = vec![0; CHUNK];
            for offset in (0..SIZE).step_by(CHUNK) {
                file.read_exact_at(&mut buffer, offset as u64).unwrap();
                assert_eq!(buffer, expected, "immutable backing changed at {offset}");
            }
        }
        fn smaps(memory: &GuestMemoryMmap) -> Value {
            let region = memory.iter().next().unwrap();
            let address = region.as_ptr() as usize;
            let end = address + SIZE;
            let text = std::fs::read_to_string("/proc/self/smaps").unwrap();
            let mut rows = Vec::new();
            let mut current = None;
            let mut covered = 0;
            for line in text.lines() {
                let token = line.split_whitespace().next().unwrap_or("");
                if let Some((lo, hi)) = token.split_once('-') {
                    if let (Ok(lo), Ok(hi)) =
                        (usize::from_str_radix(lo, 16), usize::from_str_radix(hi, 16))
                    {
                        if let Some(row) = current.take() {
                            rows.push(row);
                        }
                        if lo < end && hi > address {
                            // Never prorate aggregate VMA counters to a subrange.
                            assert!(
                                lo >= address && hi <= end,
                                "VMA extends beyond diagnostic mapping: {line}"
                            );
                            covered += hi - lo;
                            current = Some(json!({"start": lo, "end": hi, "header": line,
                                "rss_bytes": null, "pss_bytes": null, "shared_clean_bytes": null,
                                "private_dirty_bytes": null, "ksm_bytes": null, "mg": false}));
                        }
                        continue;
                    }
                }
                if let Some(row) = current.as_mut() {
                    if let Some((key, value)) = line.split_once(':') {
                        let field = match key {
                            "Rss" => Some("rss_bytes"),
                            "Pss" => Some("pss_bytes"),
                            "Shared_Clean" => Some("shared_clean_bytes"),
                            "Private_Dirty" => Some("private_dirty_bytes"),
                            "KSM" => Some("ksm_bytes"),
                            _ => None,
                        };
                        if let Some(field) = field {
                            row[field] = json!(
                                value
                                    .split_whitespace()
                                    .next()
                                    .unwrap()
                                    .parse::<u64>()
                                    .unwrap()
                                    * 1024
                            );
                        } else if key == "VmFlags" {
                            row["vm_flags"] = json!(value.trim());
                            row["mg"] = json!(value.split_whitespace().any(|flag| flag == "mg"));
                        }
                    }
                }
            }
            if let Some(row) = current {
                rows.push(row);
            }
            assert_eq!(covered, SIZE);
            json!({"address": address, "length": SIZE, "vmas": rows})
        }
        let emit = |trial: usize, seed: u64, case: &str, phase: &str, data: Value| {
            println!(
                "{}",
                json!({"benchmark": "B-MEMORY-DIAG", "schema": 1,
                "trial": trial, "seed": seed, "case": case, "phase": phase,
                "bytes_per_mapping": SIZE, "page_bytes": page, "ksm_run": ksm_run(),
                "filesystem_type_hex": format!("0x{:x}", fs.f_type),
                "backing_directory": disk.path(), "checksum_algorithm": "wrapping-sum-le-u64",
                "scope": "per-address mapping RSS/PSS, not total-machine net savings; mincore measures file page-cache residency, not mapping RSS",
                "data": data})
            );
        };
        for trial in 0..3 {
            let seed = SEED + trial as u64;
            let mut state = seed;
            let repeated: Vec<u8> = (0..page)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (state % 255 + 1) as u8
                })
                .collect();
            let expected: Vec<u8> = repeated.iter().copied().cycle().take(CHUNK).collect();
            let ranges = [(GuestAddress(0), SIZE)];
            {
                let mut baseline = tempfile::NamedTempFile::new_in(disk.path()).unwrap();
                for _ in 0..SIZE / CHUNK {
                    baseline.write_all(&expected).unwrap();
                }
                baseline.as_file().sync_all().unwrap();
                // Close the only writable descriptor; retain only unlink ownership.
                let baseline = baseline.into_temp_path();
                let path = baseline.to_path_buf();
                let file = Arc::new(File::open(&path).unwrap());
                let inode = file.metadata().unwrap().ino();
                let mappings = [crate::api::RamMappingSnapshot {
                    base: 0,
                    len: SIZE as u64,
                    file_offset: 0,
                }];
                let first =
                    crate::memory::map_snapshot_ram(&mappings, file.clone(), &ranges).unwrap();
                let second =
                    crate::memory::map_snapshot_ram(&mappings, file.clone(), &ranges).unwrap();
                for memory in [&first, &second] {
                    assert_eq!(
                        memory
                            .iter()
                            .next()
                            .unwrap()
                            .file_offset()
                            .unwrap()
                            .file()
                            .metadata()
                            .unwrap()
                            .ino(),
                        inode
                    );
                    verify(memory, &expected);
                }
                emit(
                    trial,
                    seed,
                    "snapshot_cow",
                    "before_advice",
                    json!({"inode": inode, "mappings": [smaps(&first), smaps(&second)], "checksum": verify(&first, &expected)}),
                );
                let reports =
                    [&first, &second].map(|m| report_json(advise(m, u64::MAX, None, mergeable)));
                emit(
                    trial,
                    seed,
                    "snapshot_cow",
                    "after_advice",
                    json!({"reports": reports, "mappings": [smaps(&first), smaps(&second)]}),
                );
                let mut changed = [expected.clone(), expected.clone()];
                for (memory, (bytes, mask)) in [&first, &second]
                    .into_iter()
                    .zip(changed.iter_mut().zip([0x55, 0xaa]))
                {
                    for offset in (0..CHUNK).step_by(page) {
                        bytes[offset] ^= mask;
                    }
                    for offset in (0..SIZE).step_by(page) {
                        memory
                            .write_slice(&[repeated[0] ^ mask], GuestAddress(offset as u64))
                            .unwrap();
                    }
                }
                let sums = [verify(&first, &changed[0]), verify(&second, &changed[1])];
                verify_file(&file, &expected);
                assert!(crate::vmm::ram::reclaim(&first)
                    .unwrap_err()
                    .contains("private"));
                emit(
                    trial,
                    seed,
                    "snapshot_cow",
                    "after_distinct_cow",
                    json!({"checksums": sums, "mappings": [smaps(&first), smaps(&second)], "backing_unchanged": true}),
                );
                drop(first);
                drop(file);
                drop(baseline);
                assert!(!path.exists());
                verify(&second, &changed[1]);
                let retained = Arc::new(
                    second
                        .iter()
                        .next()
                        .unwrap()
                        .file_offset()
                        .unwrap()
                        .file()
                        .try_clone()
                        .unwrap(),
                );
                let third = crate::memory::map_snapshot_ram(&mappings, retained, &ranges).unwrap();
                verify(&third, &expected);
                emit(
                    trial,
                    seed,
                    "snapshot_cow",
                    "after_unlink",
                    json!({"isolation": true, "unlinked_lifetime": true, "survivor_checksum": sums[1], "baseline_checksum": verify(&third, &expected)}),
                );
            }
            {
                let first = GuestMemoryMmap::from_ranges(&ranges).unwrap();
                let second = GuestMemoryMmap::from_ranges(&ranges).unwrap();
                // Distinct VMA attributes prevent coalescing with adjacent heap/peer
                // mappings so smaps counters belong to these exact addresses.
                for (memory, flag) in [
                    (&first, libc::MADV_DONTDUMP),
                    (&second, libc::MADV_DONTFORK),
                ] {
                    assert_eq!(
                        unsafe {
                            libc::madvise(memory.iter().next().unwrap().as_ptr().cast(), SIZE, flag)
                        },
                        0
                    );
                    for offset in (0..SIZE).step_by(CHUNK) {
                        memory
                            .write_slice(&expected, GuestAddress(offset as u64))
                            .unwrap();
                    }
                    verify(memory, &expected);
                }
                emit(
                    trial,
                    seed,
                    "anonymous_ksm",
                    "before_advice",
                    json!({"mappings": [smaps(&first), smaps(&second)]}),
                );
                let reports =
                    [&first, &second].map(|m| report_json(advise(m, u64::MAX, None, mergeable)));
                emit(
                    trial,
                    seed,
                    "anonymous_ksm",
                    "after_advice",
                    json!({"reports": reports, "mappings": [smaps(&first), smaps(&second)]}),
                );
                let wait = Instant::now();
                std::thread::sleep(Duration::from_secs(2));
                let maps = [smaps(&first), smaps(&second)];
                for map in &maps {
                    for vma in map["vmas"].as_array().unwrap() {
                        // Kernels without KSM may omit the counter: retain null
                        // rather than manufacture a zero-byte measurement.
                        if let Some(bytes) = vma["ksm_bytes"].as_u64() {
                            assert_eq!(bytes, 0, "disabled KSM must not be interpreted as savings");
                        }
                    }
                }
                emit(
                    trial,
                    seed,
                    "anonymous_ksm",
                    "after_wait",
                    json!({"wait_ms": wait.elapsed().as_secs_f64() * 1000.0, "mappings": maps,
                    "checksums": [verify(&first, &expected), verify(&second, &expected)], "expected_ksm_bytes": 0}),
                );
            }
            {
                let file = Arc::new(tempfile::tempfile_in(disk.path()).unwrap());
                let memory = crate::vmm::ram::map(&ranges, &[], file).unwrap();
                let fill = Instant::now();
                for offset in (0..SIZE).step_by(CHUNK) {
                    memory
                        .write_slice(&expected, GuestAddress(offset as u64))
                        .unwrap();
                }
                let fill_ms = fill.elapsed().as_secs_f64() * 1000.0;
                let sum = verify(&memory, &expected);
                emit(
                    trial,
                    seed,
                    "raw_offload",
                    "before_reclaim",
                    json!({"mapping": smaps(&memory), "mincore_bytes": crate::vmm::ram::residency(&memory), "fill_ms": fill_ms, "checksum": sum}),
                );
                let reclaim = Instant::now();
                let backed_bytes = crate::vmm::ram::reclaim(&memory).unwrap();
                let reclaim_ms = reclaim.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(backed_bytes, SIZE as u64);
                emit(
                    trial,
                    seed,
                    "raw_offload",
                    "after_reclaim",
                    json!({"mapping": smaps(&memory), "mincore_bytes": crate::vmm::ram::residency(&memory), "backed_bytes": backed_bytes, "reclaim_ms": reclaim_ms}),
                );
                let reread = Instant::now();
                let reread_sum = verify(&memory, &expected);
                let reread_ms = reread.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(reread_sum, sum);
                emit(
                    trial,
                    seed,
                    "raw_offload",
                    "after_full_reread",
                    json!({"mapping": smaps(&memory), "mincore_bytes": crate::vmm::ram::residency(&memory), "checksum": reread_sum, "reread_ms": reread_ms}),
                );
            }
        }
        assert_eq!(
            ksm_run(),
            initial_run,
            "host KSM setting changed externally during diagnostic"
        );
        println!(
            "{}",
            json!({"benchmark": "B-MEMORY-DIAG", "phase": "complete", "trials": 3,
            "elapsed_ms": started.elapsed().as_secs_f64() * 1000.0, "tail_claims": false})
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "diagnostic exceeded 30s budget"
        );
    }

    #[test]
    fn private_ram_eligibility_excludes_shared_huge_and_device_windows() {
        let private = libc::MAP_PRIVATE;
        let rw = libc::PROT_READ | libc::PROT_WRITE;
        for flags in [private, private | libc::MAP_ANONYMOUS] {
            assert_eq!(eligibility(flags, rw, 4096, 4096, 4096, false), None);
        }
        for (flags, prot, address, device, expected) in [
            (
                libc::MAP_SHARED,
                rw,
                4096,
                false,
                RamDedupSkipReason::SharedMapping,
            ),
            (0, rw, 4096, false, RamDedupSkipReason::NotPrivate),
            (
                private,
                libc::PROT_READ,
                4096,
                false,
                RamDedupSkipReason::NotWritable,
            ),
            (private, rw, 4097, false, RamDedupSkipReason::Unaligned),
            (private, rw, 4096, true, RamDedupSkipReason::DeviceWindow),
        ] {
            assert_eq!(
                eligibility(flags, prot, address, 4096, 4096, device),
                Some(expected)
            );
        }
        #[cfg(target_os = "linux")]
        assert_eq!(
            eligibility(private | libc::MAP_HUGETLB, rw, 4096, 4096, 4096, false),
            Some(RamDedupSkipReason::HugePages)
        );
        assert_eq!(
            eligibility(private, rw, 4096, 4095, 4096, false),
            Some(RamDedupSkipReason::Unaligned)
        );
    }

    #[test]
    fn reports_partial_acceptance_and_errors_without_changing_mappings() {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let memory = GuestMemoryMmap::from_ranges(&[
            (GuestAddress(0), page),
            (GuestAddress((page * 2) as u64), page),
            (GuestAddress((page * 4) as u64), page),
            (GuestAddress((page * 6) as u64), page),
        ])
        .unwrap();
        memory.write_slice(b"intact", GuestAddress(0)).unwrap();
        let pointers: Vec<_> = memory.iter().map(|r| r.as_ptr()).collect();
        let mut calls = 0;
        let report = advise(&memory, (page * 6) as u64, None, |_, _| {
            calls += 1;
            match calls {
                1 => RamDedupStatus::Accepted,
                2 => advice_error(io::Error::from_raw_os_error(libc::EPERM)),
                _ => advice_error(io::Error::from_raw_os_error(libc::EINVAL)),
            }
        });
        assert_eq!(calls, 3);
        assert_eq!(report.accepted_bytes, page as u64);
        assert_eq!(report.mappings[0].status, RamDedupStatus::Accepted);
        assert!(matches!(
            report.mappings[1].status,
            RamDedupStatus::Error {
                errno: Some(libc::EPERM),
                ..
            }
        ));
        assert!(matches!(
            report.mappings[2].status,
            RamDedupStatus::Unsupported {
                errno: Some(libc::EINVAL),
                ..
            }
        ));
        assert_eq!(
            report.mappings[3].status,
            RamDedupStatus::Skipped(RamDedupSkipReason::DeviceWindow)
        );
        assert_eq!(
            pointers,
            memory.iter().map(|r| r.as_ptr()).collect::<Vec<_>>()
        );
        let mut bytes = [0; 6];
        memory.read_slice(&mut bytes, GuestAddress(0)).unwrap();
        assert_eq!(&bytes, b"intact");
        for reason in [
            RamDedupSkipReason::ColdPagerActive,
            RamDedupSkipReason::DevicePreparationActive,
        ] {
            let report = advise(&memory, u64::MAX, Some(reason), |_, _| {
                panic!("blocked advice")
            });
            assert_eq!(report.accepted_bytes, 0);
            assert!(report
                .mappings
                .iter()
                .all(|r| r.status == RamDedupStatus::Skipped(reason)));
        }
    }

    #[test]
    fn shared_backing_and_device_windows_never_receive_advice() {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let file = Arc::new(tempfile::tempfile().unwrap());
        let memory = crate::vmm::ram::map(
            &[(GuestAddress(0), page)],
            &[(GuestAddress((page * 2) as u64), page)],
            file,
        )
        .unwrap();
        memory.write_slice(b"live RAM", GuestAddress(0)).unwrap();
        let report = advise(&memory, (page * 2) as u64, None, |_, _| {
            panic!("shared RAM/device window received advice")
        });
        assert_eq!(report.accepted_bytes, 0);
        assert_eq!(
            report.mappings[0].status,
            RamDedupStatus::Skipped(RamDedupSkipReason::SharedMapping)
        );
        assert_eq!(
            report.mappings[1].status,
            RamDedupStatus::Skipped(RamDedupSkipReason::DeviceWindow)
        );
        let mut bytes = [0; 8];
        memory.read_slice(&mut bytes, GuestAddress(0)).unwrap();
        assert_eq!(&bytes, b"live RAM");
    }

    #[test]
    fn real_anonymous_advice_preserves_writes_and_is_repeatable() {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let memory = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), page)]).unwrap();
        for expected in [b"before".as_slice(), b"after!".as_slice()] {
            memory.write_slice(expected, GuestAddress(0)).unwrap();
            let report = advise(&memory, u64::MAX, None, mergeable);
            assert!(!matches!(
                report.mappings[0].status,
                RamDedupStatus::Skipped(_)
            ));
            #[cfg(not(target_os = "linux"))]
            assert!(matches!(
                report.mappings[0].status,
                RamDedupStatus::Unsupported { errno: None, .. }
            ));
            let mut bytes = [0; 6];
            memory.read_slice(&mut bytes, GuestAddress(0)).unwrap();
            assert_eq!(bytes, expected);
        }
    }

    #[test]
    fn accepted_advice_and_preparation_are_mutually_exclusive() {
        let gate = MemoryGate::default();
        gate.advise_dedup(|prepared| {
            assert!(!prepared);
            ((), false)
        })
        .unwrap();
        assert!(!gate.has_dedup_advice());
        gate.close(std::time::Duration::ZERO).unwrap();
        gate.set_prepare(Some(Arc::new(|_| Ok(())))).unwrap();
        gate.advise_dedup(|prepared| {
            assert!(prepared);
            ((), false)
        })
        .unwrap();
        assert!(!gate.has_dedup_advice());
        gate.set_prepare(None).unwrap();
        gate.advise_dedup(|prepared| {
            assert!(!prepared);
            ((), true)
        })
        .unwrap();
        assert!(gate.has_dedup_advice());
        assert!(gate
            .set_prepare(Some(Arc::new(|_| Ok(()))))
            .unwrap_err()
            .contains("dedup"));
        gate.set_prepare(None).unwrap();
        gate.open();
        assert!(gate.has_dedup_advice());
    }

    #[test]
    fn cow_advice_preserves_isolation_baseline_lifetime_and_reclaim_refusal() {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let mut baseline = tempfile::NamedTempFile::new().unwrap();
        baseline.as_file().set_len(page as u64).unwrap();
        baseline.write_all(b"baseline").unwrap();
        let file = Arc::new(File::open(baseline.path()).unwrap());
        let mappings = [crate::api::RamMappingSnapshot {
            base: 0,
            len: page as u64,
            file_offset: 0,
        }];
        let ranges = [(GuestAddress(0), page)];
        let first = crate::memory::map_snapshot_ram(&mappings, file.clone(), &ranges).unwrap();
        let second = crate::memory::map_snapshot_ram(&mappings, file.clone(), &ranges).unwrap();
        // Real advice is allowed to be unavailable on the test host; it must
        // never prevent private COW writes or require a running KSM scanner.
        for memory in [&first, &second] {
            let report = advise(memory, u64::MAX, None, mergeable);
            assert_eq!(report.mappings.len(), 1);
            assert!(!matches!(
                report.mappings[0].status,
                RamDedupStatus::Skipped(_)
            ));
        }
        first.write_slice(b"first VM", GuestAddress(0)).unwrap();
        second.write_slice(b"other VM", GuestAddress(0)).unwrap();
        assert!(crate::vmm::ram::reclaim(&first)
            .unwrap_err()
            .contains("private"));
        let mut bytes = [0; 8];
        first.read_slice(&mut bytes, GuestAddress(0)).unwrap();
        assert_eq!(&bytes, b"first VM");
        second.read_slice(&mut bytes, GuestAddress(0)).unwrap();
        assert_eq!(&bytes, b"other VM");
        drop(first);
        drop(file);
        let mut disk = File::open(baseline.path()).unwrap();
        disk.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"baseline");
        // Unlink the cache name and let its first owner exit. The second mapping
        // still owns its backing object, including untouched baseline bytes.
        drop(baseline);
        second.read_slice(&mut bytes, GuestAddress(0)).unwrap();
        assert_eq!(&bytes, b"other VM");
        drop(disk);
        let retained_file = Arc::new(
            second
                .iter()
                .next()
                .unwrap()
                .file_offset()
                .unwrap()
                .file()
                .try_clone()
                .unwrap(),
        );
        let third = crate::memory::map_snapshot_ram(&mappings, retained_file, &ranges).unwrap();
        third.read_slice(&mut bytes, GuestAddress(0)).unwrap();
        assert_eq!(&bytes, b"baseline");
    }
}
