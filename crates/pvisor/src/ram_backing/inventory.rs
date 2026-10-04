//! Opt-in diagnostic of mapped backing pages, not private RSS or PFNs.
//! Mapping/allocator changes can race this self-query even with vCPUs paused.
use std::io;

#[derive(Debug)]
pub struct ProcessInventory {
    pub timestamp_ns: u128,
    pub query_us: u128,
    pub query_attempts: u32,
    pub page_bytes: u64,
    pub regions: u64,
    pub scanned_pages: u64,
    /// Host address, object identity, object offset, disposition.
    pub pages: Vec<[u64; 4]>,
}

// Native mach/vm_region.h VM_PAGE_INFO_BASIC: eight integer_t words.
#[repr(C)]
#[derive(Default)]
struct PageInfo {
    disposition: i32,
    ref_count: i32,
    object_id: u64,
    offset: u64,
    depth: i32,
    pad: i32,
}
unsafe extern "C" {
    static mach_task_self_: u32;
    fn getpagesize() -> i32;
    fn mach_vm_allocate(task: u32, address: *mut u64, size: u64, flags: i32) -> i32;
    fn mach_vm_deallocate(task: u32, address: u64, size: u64) -> i32;
    fn mach_port_deallocate(task: u32, port: u32) -> i32;
    fn mach_vm_region(
        task: u32,
        address: *mut u64,
        size: *mut u64,
        flavor: i32,
        info: *mut u32,
        count: *mut u32,
        object: *mut u32,
    ) -> i32;
    fn mach_vm_page_info(
        task: u32,
        address: u64,
        flavor: i32,
        info: *mut PageInfo,
        count: *mut u32,
    ) -> i32;
    fn mach_vm_page_range_query(
        task: u32,
        address: u64,
        size: u64,
        dispositions: u64,
        count: *mut u64,
    ) -> i32;
}
fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!("Mach query failed: {result}")))
    }
}
fn identity(address: u64) -> io::Result<[u64; 4]> {
    let mut info = PageInfo::default();
    let mut count = 8;
    check(unsafe { mach_vm_page_info(mach_task_self_, address, 1, &mut info, &mut count) })?;
    if count != 8 {
        return Err(io::Error::other("unexpected page info ABI"));
    }
    Ok([
        address,
        info.object_id,
        info.offset,
        info.disposition as u32 as u64,
    ])
}

/// Walk every mapped region without faulting in its contents. Fail rather than
/// publish partial coverage. Caller must separately account unmapped backing.
pub fn process_inventory() -> io::Result<ProcessInventory> {
    let started = std::time::Instant::now();
    for attempt in 1..=3 {
        match process_inventory_once() {
            Ok(mut inventory) => {
                inventory.query_attempts = attempt;
                inventory.query_us = started.elapsed().as_micros();
                return Ok(inventory);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock && attempt < 3 => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}

fn process_inventory_once() -> io::Result<ProcessInventory> {
    let started = std::time::Instant::now();
    let timestamp_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let native_page = unsafe { getpagesize() };
    if native_page <= 0 {
        return Err(io::Error::other("invalid host page size"));
    }
    let page = native_page as u64;
    struct Canary(u64, u64);
    impl Drop for Canary {
        fn drop(&mut self) {
            unsafe {
                mach_vm_deallocate(mach_task_self_, self.0, self.1);
            }
        }
    }
    let mut address = 0;
    check(unsafe { mach_vm_allocate(mach_task_self_, &mut address, page * 2, 1) })?;
    let canary = Canary(address, page * 2);
    unsafe {
        std::ptr::write_bytes(address as *mut u8, 0x51, page as usize);
        std::ptr::write_bytes((address + page) as *mut u8, 0x72, page as usize);
    }
    let first = identity(address)?;
    let second = identity(address + page)?;
    if first[3] & 3 != 1 || second[3] & 3 != 1 || first[1..3] == second[1..3] {
        return Err(io::Error::other(
            "ordinary resident page identities unavailable",
        ));
    }
    let mut result = ProcessInventory {
        timestamp_ns,
        query_us: 0,
        query_attempts: 1,
        page_bytes: page,
        regions: 0,
        scanned_pages: 0,
        pages: Vec::new(),
    };
    let mut found = 0;
    address = 0;
    loop {
        let mut size = 0;
        let mut info = [0u32; 5]; // VM_REGION_TOP_INFO, including padded share_mode.
        let mut count = 5;
        let mut object = 0;
        let rc = unsafe {
            mach_vm_region(
                mach_task_self_,
                &mut address,
                &mut size,
                12,
                info.as_mut_ptr(),
                &mut count,
                &mut object,
            )
        };
        if object != 0 {
            check(unsafe { mach_port_deallocate(mach_task_self_, object) })?;
        }
        if rc == 1 {
            break;
        } // KERN_INVALID_ADDRESS: no remaining mapped region.
        check(rc)?;
        if count != 5 || size == 0 || address % page != 0 || size % page != 0 {
            return Err(io::Error::other("invalid region query ABI or alignment"));
        }
        let end = address
            .checked_add(size)
            .ok_or_else(|| io::Error::other("region overflow"))?;
        // ponytail: diagnostic caps (1 TiB virtual / 16 GiB rows on 16 KiB hosts),
        // reject larger workloads; no silent truncation or zero-residency skipping.
        if size / page > 67_108_864 - result.scanned_pages {
            return Err(io::Error::other(
                "process inventory virtual scan budget exceeded",
            ));
        }
        result.regions += 1;
        while address < end {
            let mut dispositions = [0i32; 4096];
            let n = ((end - address) / page).min(dispositions.len() as u64);
            let mut returned = n;
            check(unsafe {
                mach_vm_page_range_query(
                    mach_task_self_,
                    address,
                    n * page,
                    dispositions.as_mut_ptr() as u64,
                    &mut returned,
                )
            })?;
            if returned != n {
                return Err(io::Error::other("incomplete page range query"));
            }
            for (i, disposition) in dispositions[..n as usize].iter().enumerate() {
                let location = address + i as u64 * page;
                if *disposition & 0x11 == 0 {
                    continue;
                }
                let row = identity(location)?;
                if row[3] != *disposition as u32 as u64 {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!(
                            "page state changed during inventory: address={location} before={} after={}",
                            *disposition, row[3]
                        ),
                    ));
                }
                if location == canary.0 {
                    found |= 1;
                }
                if location == canary.0 + page {
                    found |= 2;
                }
                if result.pages.len() >= 1_048_576 {
                    return Err(io::Error::other(
                        "process inventory resident row budget exceeded",
                    ));
                }
                result.pages.push(row);
            }
            address += n * page;
            result.scanned_pages += n;
        }
    }
    if found != 3 {
        return Err(io::Error::other("process inventory missed canary mappings"));
    }
    result.query_us = started.elapsed().as_micros();
    Ok(result)
}

#[cfg(test)]
mod tests {
    #[test]
    fn complete_process_inventory_has_unique_addresses() {
        // A live self-query can explicitly reject a racing page-state change.
        // Require a complete inventory within a bounded window, without
        // relaxing the production consistency checks or accepting partial rows.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let inventory = loop {
            match super::process_inventory() {
                Ok(inventory) => break inventory,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(error) => panic!("complete process inventory unavailable: {error}"),
            }
        };
        assert!(inventory.regions > 0 && inventory.scanned_pages > 0);
        assert!(inventory.pages.len() >= 2);
        let addresses: std::collections::BTreeSet<_> =
            inventory.pages.iter().map(|p| p[0]).collect();
        assert_eq!(addresses.len(), inventory.pages.len());
        assert!(
            inventory
                .pages
                .iter()
                .all(|p| p[0] % inventory.page_bytes == 0)
        );
    }
}
