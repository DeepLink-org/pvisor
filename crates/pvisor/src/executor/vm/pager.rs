//! Opt-in macOS cold RAM experiment. The runner owns all mappings and faults.
//! Queue accesses prepare descriptor and payload ranges before retaining views.
//! Sustained device traffic and physical memory savings still require validation.
use crate::ram_backing::{
    BLOCK_BYTES,
    ipc::{PoolClient, PoolStats, RemoteObject},
};
use pvisor_vm::api::{ColdRamControl, FrozenMemory, RamAccess};
use std::{
    io,
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::UnixStream,
    },
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub const POOL_ENV: &str = "PVISOR_EXPERIMENTAL_MEMORY_POOL";
pub const METRICS_ENV: &str = "PVISOR_EXPERIMENTAL_MEMORY_METRICS";
// One isolated VM per runner. Count detached file ranges until punching completes.
static PENDING_FILE_BYTES: AtomicU64 = AtomicU64::new(0);
static PENDING_SNAPSHOT_BYTES: AtomicU64 = AtomicU64::new(0);
enum State {
    Resident,
    Deferred(Instant),
    Observing(Instant),
    Publishing,
    Cold(RemoteObject),
}
struct Page {
    block: pvisor_vm::api::RamBlock,
    state: State,
    file_detached: bool,
}
struct Pager {
    pages: Vec<Page>,
    pool: Arc<Mutex<PoolClient>>,
    scratch: Vec<u8>,
    restored: u64,
    restore_count: u64,
    observation_restores: u64,
    cancelled_publications: u64,
    invalidated_publications: u64,
    restore_total_us: u64,
    restore_max_us: u64,
    device_max_us: u64,
    device_restored: u64,
    device_payload_restored: u64,
    cursor: usize,
    pool_rejections: u64,
    deferred_rearms: u64,
}
struct Sample {
    started: Instant,
    visited: usize,
    worked: usize,
    snapshots: Vec<(usize, Vec<u8>)>,
    snapshot_max_us: u128,
    put_max_us: u128,
}
impl Drop for Sample {
    fn drop(&mut self) {
        let bytes: usize = self.snapshots.iter().map(|(_, bytes)| bytes.len()).sum();
        self.snapshots.clear();
        PENDING_SNAPSHOT_BYTES.fetch_sub(bytes as u64, Ordering::SeqCst);
    }
}

impl Pager {
    fn restore(&mut self, index: usize) -> Result<(), String> {
        let started = Instant::now();
        let page = &mut self.pages[index];
        let was_cold = matches!(page.state, State::Cold(_));
        match &page.state {
            State::Resident | State::Deferred(_) => return Ok(()),
            State::Observing(_) | State::Publishing => {
                unsafe {
                    page.block.observe(false)?;
                }
                self.observation_restores += 1;
            }
            State::Cold(object) => {
                self.pool
                    .lock()
                    .map_err(|_| "cold pool poisoned".to_string())?
                    .restore(object, &mut self.scratch)
                    .map_err(|error| error.to_string())?;
                // CPU faults and device preparation serialize through this mutex.
                // Cold blocks have neither accessible host pages nor guest mappings.
                unsafe {
                    page.block.restore(&self.scratch)?;
                }
                self.restored += page.block.length() as u64;
            }
        }
        let previous = std::mem::replace(&mut page.state, State::Resident);
        if let State::Cold(object) = previous {
            self.pool
                .lock()
                .map_err(|_| "cold pool poisoned".to_string())?
                .release(object)
                .map_err(|error| error.to_string())?;
        }
        if was_cold {
            let elapsed = started.elapsed().as_micros() as u64;
            self.restore_count += 1;
            self.restore_total_us += elapsed;
            self.restore_max_us = self.restore_max_us.max(elapsed);
        }
        Ok(())
    }
    fn fault(&mut self, address: u64) -> Result<bool, String> {
        let end = self
            .pages
            .partition_point(|page| page.block.guest_address() <= address);
        let Some(index) = end.checked_sub(1) else {
            return Ok(false);
        };
        let page = &self.pages[index];
        if address - page.block.guest_address() >= page.block.length() as u64 {
            return Ok(false);
        }
        // Another vCPU or a device may have restored this block after the
        // hardware exit was recorded. Its stale exit still needs instruction retry.
        self.restore(index)?;
        Ok(true)
    }
    fn device_prepare(&mut self, ranges: &[(u64, usize)]) -> Result<(), String> {
        let started = Instant::now();
        let previous = self.restored;
        if ranges.is_empty() {
            for index in 0..self.pages.len() {
                self.restore(index)?;
            }
        } else {
            for &(address, length) in ranges {
                if length == 0 {
                    continue;
                }
                let end = address
                    .checked_add(length as u64)
                    .ok_or("device RAM range overflow")?;
                let first = self.pages.partition_point(|page| {
                    page.block.guest_address() + page.block.length() as u64 <= address
                });
                for index in first..self.pages.len() {
                    if self.pages[index].block.guest_address() >= end {
                        break;
                    }
                    self.restore(index)?;
                }
            }
        }
        if self.restored != previous {
            let restored = self.restored - previous;
            self.device_restored += restored;
            // Queue rings use three ranges; descriptor headers use one 16-byte
            // range. Count only larger single-range payload preparation here.
            if ranges.len() == 1 && ranges[0].1 > 16 {
                self.device_payload_restored += restored;
            }
            self.device_max_us = self.device_max_us.max(started.elapsed().as_micros() as u64);
        }
        Ok(())
    }
    /// Copies under the CPU/device barrier; publication happens after resumption.
    fn sample(&mut self, now: Instant) -> Result<Sample, String> {
        let mut sample = Sample {
            started: now,
            visited: 0,
            worked: 0,
            snapshots: Vec::new(),
            snapshot_max_us: 0,
            put_max_us: 0,
        };
        // ponytail: at most 4 MiB of transient snapshots per VM. Increase only
        // after measuring reclamation throughput and accounting the extra memory.
        while sample.visited < self.pages.len()
            && sample.worked < 256
            && sample.snapshots.len() < 64
            && now.elapsed() < Duration::from_millis(8)
        {
            let index = self.cursor;
            self.cursor = (self.cursor + 1) % self.pages.len();
            sample.visited += 1;
            let page = &mut self.pages[index];
            match page.state {
                State::Resident | State::Deferred(_)
                    if !matches!(page.state,
                    State::Deferred(until) if now < until) =>
                {
                    sample.worked += 1;
                    if matches!(page.state, State::Deferred(_)) {
                        self.deferred_rearms += 1;
                    }
                    unsafe {
                        page.block.observe(true)?;
                    }
                    page.state = State::Observing(now);
                }
                State::Observing(since)
                    if now.saturating_duration_since(since) >= Duration::from_millis(200) =>
                {
                    sample.worked += 1;
                    let started = Instant::now();
                    sample.snapshots.push((index, vec![0; page.block.length()]));
                    PENDING_SNAPSHOT_BYTES.fetch_add(page.block.length() as u64, Ordering::SeqCst);
                    unsafe {
                        page.block
                            .snapshot(&mut sample.snapshots.last_mut().unwrap().1)
                            .map_err(|error| error.to_string())?;
                    }
                    sample.snapshot_max_us =
                        sample.snapshot_max_us.max(started.elapsed().as_micros());
                    // Only this maintenance thread can enter Publishing. Before
                    // it finishes this batch, CPU/device access can only cancel it.
                    page.state = State::Publishing;
                }
                _ => {}
            }
        }
        Ok(sample)
    }

    /// Called under the CPU/device barrier, with no pool RPCs or hashing.
    fn commit(
        &mut self,
        published: Vec<(usize, Option<RemoteObject>)>,
    ) -> Result<(Vec<pvisor_vm::api::RamBlock>, Vec<RemoteObject>, u128), String> {
        let mut detached = Vec::new();
        let mut unused = Vec::new();
        let mut discard_max_us = 0;
        let started = Instant::now();
        for (index, object) in published {
            let page = &mut self.pages[index];
            let Some(object) = object else {
                if matches!(page.state, State::Publishing) {
                    unsafe {
                        page.block.observe(false)?;
                    }
                    page.state = State::Deferred(Instant::now() + Duration::from_secs(30));
                }
                continue;
            };
            let invalidated = !matches!(page.state, State::Publishing);
            if invalidated || started.elapsed() >= Duration::from_millis(8) {
                if matches!(page.state, State::Publishing) {
                    page.state = State::Observing(Instant::now());
                }
                self.cancelled_publications += 1;
                if invalidated {
                    self.invalidated_publications += 1;
                }
                unused.push(object);
                continue;
            }
            // A successful immutable reference must precede destructive mapping changes.
            page.state = State::Cold(object);
            let discard_started = Instant::now();
            unsafe {
                page.block.discard()?;
            }
            discard_max_us = discard_max_us.max(discard_started.elapsed().as_micros());
            if !page.file_detached {
                page.file_detached = true;
                PENDING_FILE_BYTES.fetch_add(page.block.length() as u64, Ordering::SeqCst);
                detached.push(page.block.clone());
            }
        }
        Ok((detached, unused, discard_max_us))
    }

    fn abandon(&mut self, sample: &Sample) {
        for (index, _) in &sample.snapshots {
            if matches!(self.pages[*index].state, State::Publishing) {
                // Metadata only: both states retain the same protected original mapping.
                self.pages[*index].state = State::Observing(Instant::now());
            }
        }
    }

    fn emit_sample(&self, sample: &Sample, stats: PoolStats, stats_us: u128, discard_max_us: u128) {
        let cold: usize = self
            .pages
            .iter()
            .filter(|p| matches!(p.state, State::Cold(_)))
            .map(|p| p.block.length())
            .sum();
        let deferred: usize = self
            .pages
            .iter()
            .filter(|p| matches!(p.state, State::Deferred(_)))
            .map(|p| p.block.length())
            .sum();
        eprintln!(
            "pvisor-cold-sample cold_bytes={cold} deferred_bytes={deferred} deferred_rearms={} restored_bytes={} pool_encoded={} pool_objects={} references={} cross_session_objects={} sample_us={} restore_count={} restore_total_us={} restore_max_us={} device_max_us={} visited={} worked={} observation_restores={} cursor={} snapshot_max_us={} put_max_us={} discard_max_us={discard_max_us} stats_us={stats_us} pool_rejections={} device_restored_bytes={} device_payload_restored_bytes={} cancelled_publications={} invalidated_publications={} pid={}",
            self.deferred_rearms,
            self.restored,
            stats.encoded_bytes,
            stats.objects,
            stats.session_references,
            stats.cross_session_objects,
            sample.started.elapsed().as_micros(),
            self.restore_count,
            self.restore_total_us,
            self.restore_max_us,
            self.device_max_us,
            sample.visited,
            sample.worked,
            self.observation_restores,
            self.cursor,
            sample.snapshot_max_us,
            sample.put_max_us,
            self.pool_rejections,
            self.device_restored,
            self.device_payload_restored,
            self.cancelled_publications,
            self.invalidated_publications,
            std::process::id()
        );
    }
}
/// Raw host counters are ambient machine measurements, not process attribution.
fn emit_memory(phase: &str) {
    unsafe extern "C" {
        fn mach_host_self() -> libc::mach_port_t;
        fn mach_port_deallocate(
            task: libc::mach_port_t,
            name: libc::mach_port_t,
        ) -> libc::kern_return_t;
        static mach_task_self_: libc::mach_port_t;
    }
    let mut usage: libc::rusage_info_v0 = unsafe { std::mem::zeroed() };
    let mut host_info: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    let host = unsafe { mach_host_self() };
    let process_ok = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V0,
            (&mut usage as *mut libc::rusage_info_v0).cast(),
        )
    } == 0;
    let host_ok = unsafe {
        libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            (&mut host_info as *mut libc::vm_statistics64).cast(),
            &mut count,
        )
    } == libc::KERN_SUCCESS;
    unsafe {
        mach_port_deallocate(mach_task_self_, host);
    }
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    eprintln!(
        "pvisor-cold-memory phase={phase} pid={} timestamp_ns={timestamp} available={} footprint={} resident={} wired={} host_page_bytes={page} host_free={} host_active={} host_inactive={} host_wired={} host_speculative={} host_compressor={} host_internal={} host_external={}",
        std::process::id(),
        u8::from(process_ok && host_ok && page > 0),
        usage.ri_phys_footprint,
        usage.ri_resident_size,
        usage.ri_wired_size,
        host_info.free_count,
        host_info.active_count,
        host_info.inactive_count,
        host_info.wire_count,
        host_info.speculative_count,
        host_info.compressor_page_count,
        host_info.internal_page_count,
        host_info.external_page_count
    );
}
fn stopped(error: &str) -> bool {
    error == "VMM has stopped"
}
fn fatal(error: impl std::fmt::Display) -> ! {
    eprintln!("experimental cold RAM failure: {error}");
    std::process::exit(1)
}
pub(super) fn start_if_requested(handle: pvisor_vm::api::VmmHandle) -> io::Result<()> {
    if let Some(directory) = std::env::var_os("PVISOR_EXPERIMENTAL_MEMORY_PAGE_INVENTORY") {
        let directory = std::path::PathBuf::from(directory);
        let metadata = directory.metadata()?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::other(
                "RAM inventory directory must be private and owned by this user",
            ));
        }
        let measured = handle.clone();
        let process_directory = std::env::var_os("PVISOR_EXPERIMENTAL_MEMORY_PROCESS_INVENTORY")
            .map(std::path::PathBuf::from);
        std::thread::Builder::new().name("pvisor-ram-inventory".into()).spawn(move || {
            // Diagnostic only: one full query during the quiet window. Do not
            // include this run's pauses in production pager performance claims.
            std::thread::sleep(Duration::from_secs(25));
            let deadline = Instant::now() + Duration::from_secs(5);
            for _ in 0..20 {
                if Instant::now() >= deadline { break; }
                let started = Instant::now();
                let timestamp_ns = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default().as_nanos();
                let pending_before = PENDING_FILE_BYTES.load(Ordering::SeqCst);
                if pending_before != 0 {
                    std::thread::sleep(Duration::from_millis(250));
                    continue;
                }
                // Read-only query failures must not park an otherwise healthy VM.
                match measured.with_ram_quiesced(|vmm| Ok((|| {
                    let ram = vmm.experimental_ram_page_inventory().map_err(io::Error::other)?;
                    let process = if process_directory.is_some() {
                        Some(crate::ram_backing::inventory::process_inventory()?)
                    } else { None };
                    Ok::<_, io::Error>((ram, process))
                })())) {
                    Ok(Some(Ok(((page_bytes, rows), process)))) => {
                        let pending_upper = pending_before.max(PENDING_FILE_BYTES.load(Ordering::SeqCst));
                        if pending_upper != 0 {
                            std::thread::sleep(Duration::from_millis(250));
                            continue;
                        }
                        let value = serde_json::json!({"pid": std::process::id(),
                            "timestamp_ns": timestamp_ns, "page_bytes": page_bytes,
                            "query_transaction_us": started.elapsed().as_micros(),
                            "pending_file_bytes_upper": pending_upper,
                            "columns": ["guest_address", "object_id", "object_offset", "disposition"],
                            "pages": rows});
                        use std::os::unix::fs::OpenOptionsExt;
                        let result = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
                            .open(directory.join(format!("{}.json", std::process::id())))
                            .and_then(|file| serde_json::to_writer(file, &value).map_err(io::Error::other));
                        if let Err(error) = result { eprintln!("experimental RAM inventory failed: {error}"); }
                        if let (Some(directory), Some(process)) = (&process_directory, process)
                            && let Err(error) = crate::ram_backing::write_process_inventory(directory, process)
                        {
                            eprintln!("experimental runner process inventory failed: {error}");
                        }
                        return;
                    }
                    Ok(Some(Err(error))) if error.kind() == io::ErrorKind::WouldBlock => {
                        // Discard both inventories and retry after resuming CPUs;
                        // keep exact disposition equality and the diagnostic deadline.
                        eprintln!("experimental RAM inventory retry: {error}");
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(250)),
                    result => { eprintln!("experimental RAM inventory stopped: {result:?}"); return; }
                }
            }
            eprintln!("experimental RAM inventory skipped: diagnostic budget exhausted");
        })?;
    }
    let metrics = std::env::var_os(METRICS_ENV).is_some();
    if metrics {
        let measured = handle.clone();
        std::thread::Builder::new()
            .name("pvisor-ram-metrics".into())
            .spawn(move || {
                loop {
                    let pending_before = PENDING_FILE_BYTES.load(Ordering::SeqCst);
                    let snapshots_before = PENDING_SNAPSHOT_BYTES.load(Ordering::SeqCst);
                    match measured.experimental_ram_residency() {
                        Ok(resident) => {
                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_nanos();
                            eprintln!(
                                "pvisor-ram-residency pid={} timestamp_ns={timestamp} available={} resident_bytes={} pending_file_bytes={} pending_snapshot_bytes={}",
                                std::process::id(),
                                u8::from(resident.is_some()),
                                resident.unwrap_or(0),
                                pending_before.max(PENDING_FILE_BYTES.load(Ordering::SeqCst)),
                                snapshots_before.max(PENDING_SNAPSHOT_BYTES.load(Ordering::SeqCst))
                            );
                        }
                        Err(error) => {
                            if !stopped(&error) {
                                eprintln!("experimental RAM metrics stopped: {error}");
                            }
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            })?;
    }
    let Some(path) = std::env::var_os(POOL_ENV) else {
        return Ok(());
    };
    let path = Path::new(&path);
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("pool parent missing"))?;
    let metadata = parent.metadata()?;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::other(
            "experimental pool directory must belong to this user and be private",
        ));
    }
    let pool = Arc::new(Mutex::new(PoolClient::new(
        UnixStream::connect(path)?,
        Duration::from_secs(5),
    )?));
    std::thread::Builder::new()
        .name("pvisor-cold-pager".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(1));
            let pager = loop {
                let initialized = handle.with_ram_quiesced(|vmm| {
                    let blocks = vmm.experimental_ram_blocks(BLOCK_BYTES)?;
                    let pager = Arc::new(Mutex::new(Pager {
                        pages: blocks
                            .into_iter()
                            .map(|block| Page {
                                block,
                                state: State::Resident,
                                file_detached: false,
                            })
                            .collect(),
                        pool: pool.clone(),
                        scratch: vec![0; BLOCK_BYTES],
                        restored: 0,
                        restore_count: 0,
                        observation_restores: 0,
                        cancelled_publications: 0,
                        invalidated_publications: 0,
                        restore_total_us: 0,
                        restore_max_us: 0,
                        device_max_us: 0,
                        device_restored: 0,
                        device_payload_restored: 0,
                        cursor: 0,
                        pool_rejections: 0,
                        deferred_rearms: 0,
                    }));
                    let cpu = pager.clone();
                    vmm.install_memory_fault_handler(Arc::new(move |fault| {
                        cpu.lock()
                            .map_err(|_| "cold pager poisoned".to_string())?
                            .fault(fault.guest_address)
                    }))
                    .map_err(|error| error.to_string())?;
                    let device = pager.clone();
                    vmm.set_device_prepare(Some(Arc::new(move |ranges| {
                            device
                                .lock()
                                .map_err(|_| "cold pager poisoned".to_string())?
                                .device_prepare(ranges)
                        })))
                        ?;
                    Ok(pager)
                });
                match initialized {
                    Ok(Some(pager)) => break pager,
                    Err(error) if stopped(&error) => return,
                    Ok(None) => {
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    Err(error) => fatal(error),
                }
            };
            loop {
                if metrics { emit_memory("before"); }
                let prepare_started = Instant::now();
                let result = handle.with_ram_quiesced(|_| {
                    pager.lock().map_err(|_| "cold pager poisoned".to_string())?
                        .sample(Instant::now())
                });
                let mut quiesce_total_us = prepare_started.elapsed().as_micros();
                let mut sample = match result {
                    Ok(Some(sample)) => sample,
                    Ok(None) => { std::thread::sleep(Duration::from_millis(250)); continue; }
                    Err(error) if stopped(&error) => return,
                    Err(error) => fatal(error),
                };
                let publication_started = Instant::now();
                let mut published = Vec::with_capacity(sample.snapshots.len());
                for (index, bytes) in &sample.snapshots {
                    let started = Instant::now();
                    // Drop the pool lock before taking the pager lock or CPU barrier.
                    // Hot observation cancellation does not need the pool connection.
                    let mut client = pool.lock().unwrap_or_else(|_| fatal("cold pool poisoned"));
                    let object = match client.put(bytes) {
                        Ok(object) => Some(object),
                        Err(error) => {
                            client.stats().unwrap_or_else(|_| fatal(error));
                            None // Complete capacity rejection preserves existing references.
                        }
                    };
                    sample.put_max_us = sample.put_max_us.max(started.elapsed().as_micros());
                    drop(client);
                    if object.is_none() {
                        pager.lock().unwrap_or_else(|_| fatal("cold pager poisoned")).pool_rejections += 1;
                    }
                    published.push((*index, object));
                }
                let publication_us = publication_started.elapsed().as_micros();
                let mut pending = Some(published);
                let commit_started = Instant::now();
                let result = if sample.snapshots.is_empty() {
                    Ok(Some((Vec::new(), Vec::new(), 0)))
                } else { handle.with_ram_quiesced(|_| {
                    pager.lock().map_err(|_| "cold pager poisoned".to_string())?
                        .commit(pending.take().ok_or("cold publication already committed")?)
                }) };
                quiesce_total_us += commit_started.elapsed().as_micros();
                let (detached, unused, discard_max_us) = match result {
                    Ok(Some(result)) => result,
                    Ok(None) => {
                        pager.lock().unwrap_or_else(|_| fatal("cold pager poisoned")).abandon(&sample);
                        let unused = pending.take().unwrap().into_iter().filter_map(|(_, o)| o).collect();
                        (Vec::new(), unused, 0)
                    }
                    Err(error) if stopped(&error) => return,
                    Err(error) => fatal(error),
                };
                if metrics {
                    eprintln!("pvisor-cold-transaction elapsed_us={quiesce_total_us} publication_us={publication_us} snapshots={} pid={}",
                        sample.snapshots.len(), std::process::id());
                }
                for object in unused {
                    pool.lock().unwrap_or_else(|_| fatal("cold pool poisoned"))
                        .release(object).unwrap_or_else(|error| fatal(error));
                }
                let reclaim_started = Instant::now();
                let detached_bytes: usize = detached.iter().map(|block| block.length()).sum();
                for block in detached {
                    // Concurrent restores use private RAM, never this original file range.
                    if let Err(error) = unsafe { block.reclaim_file() } { fatal(error); }
                    PENDING_FILE_BYTES.fetch_sub(block.length() as u64, Ordering::SeqCst);
                }
                if metrics {
                    eprintln!("pvisor-cold-file-reclaim bytes={detached_bytes} elapsed_us={} pending_bytes={} pid={}",
                        reclaim_started.elapsed().as_micros(), PENDING_FILE_BYTES.load(Ordering::SeqCst), std::process::id());
                }
                let stats_started = Instant::now();
                let stats = pool.lock().unwrap_or_else(|_| fatal("cold pool poisoned"))
                    .stats().unwrap_or_else(|error| fatal(error));
                if metrics {
                    pager.lock().unwrap_or_else(|_| fatal("cold pager poisoned"))
                        .emit_sample(&sample, stats, stats_started.elapsed().as_micros(), discard_max_us);
                    emit_memory("after");
                }
                drop(sample);
                std::thread::sleep(Duration::from_millis(250));
            }

        })?;
    Ok(())
}
