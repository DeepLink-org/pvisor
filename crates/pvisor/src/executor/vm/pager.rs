//! Product-side cold RAM pool authorization and diagnostic destinations.
//! VM mapping transitions and fault/device recovery live in pvisor-vm.
use crate::ram_backing::ipc::{PoolClient, RemoteObject};
use pvisor_vm::api::ColdRamControl;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use pvisor_vm::api::{FrozenMemory, RuntimeSupport, VmPlatform};
use std::io;
#[cfg(not(target_os = "linux"))]
use std::{
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::UnixStream,
    },
    path::Path,
    time::Duration,
};

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::time::Instant;

pub const POOL_ENV: &str = "PVISOR_EXPERIMENTAL_MEMORY_POOL";
pub const METRICS_ENV: &str = "PVISOR_EXPERIMENTAL_MEMORY_METRICS";
pub(super) fn start_if_requested(
    handle: pvisor_vm::api::VmmHandle,
    local: bool,
    memory_mib: u32,
) -> io::Result<()> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
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
                let pending_before = VmPlatform::cold_ram_activity().pending_file_bytes;
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
                        let pending_upper = pending_before.max(VmPlatform::cold_ram_activity().pending_file_bytes);
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
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if metrics {
        let measured = handle.clone();
        std::thread::Builder::new()
            .name("pvisor-ram-metrics".into())
            .spawn(move || {
                loop {
                    let pending_before = VmPlatform::cold_ram_activity().pending_file_bytes;
                    let snapshots_before = VmPlatform::cold_ram_activity().pending_snapshot_bytes;
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
                                pending_before.max(VmPlatform::cold_ram_activity().pending_file_bytes),
                                snapshots_before.max(VmPlatform::cold_ram_activity().pending_snapshot_bytes)
                            );
                        }
                        Err(error) => {
                            if error != "VMM has stopped" {
                                eprintln!("experimental RAM metrics stopped: {error}");
                            }
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            })?;
    }
    if local {
        if std::env::var_os(POOL_ENV).is_some() {
            return Err(io::Error::other(
                "local cold RAM compression cannot use an external memory pool",
            ));
        }
        // Per-instance ceilings: at most half configured RAM as encoded payload,
        // and one object per 64 KiB RAM block. Index overhead is bounded separately.
        let ram_bytes = usize::try_from(u64::from(memory_mib) * 1024 * 1024)
            .map_err(|_| io::Error::other("local cold RAM budget exceeds address space"))?;
        let store = crate::ram_backing::resident::LocalColdRamStore::new(
            ram_bytes / 2,
            ram_bytes.div_ceil(64 * 1024),
        );
        return handle.start_cold_pager(store, pvisor_vm::api::ColdRamOptions { metrics });
    }
    let Some(_path) = std::env::var_os(POOL_ENV) else {
        return Ok(());
    };
    #[cfg(target_os = "linux")]
    return Err(io::Error::other(
        "Linux cold pager supports bounded instance-local storage only; external pool latency is not supported",
    ));
    #[cfg(not(target_os = "linux"))]
    {
        let path = Path::new(&_path);
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("pool parent missing"))?;
        let metadata = parent.metadata()?;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::other(
                "experimental pool directory must belong to this user and be private",
            ));
        }
        let pool = PoolClient::new(UnixStream::connect(path)?, Duration::from_secs(5))?;
        handle.start_cold_pager(pool, pvisor_vm::api::ColdRamOptions { metrics })
    }
}

impl pvisor_vm::api::ColdRamStore for PoolClient {
    type Object = RemoteObject;
    fn put(&mut self, bytes: &[u8]) -> io::Result<Self::Object> {
        PoolClient::put(self, bytes)
    }
    fn restore(&mut self, object: &Self::Object, output: &mut [u8]) -> io::Result<()> {
        PoolClient::restore(self, object, output)
    }
    fn release(&mut self, object: Self::Object) -> io::Result<()> {
        PoolClient::release(self, object)
    }
    fn stats(&mut self) -> io::Result<pvisor_vm::api::ColdRamPoolStats> {
        let stats = PoolClient::stats(self)?;
        Ok(pvisor_vm::api::ColdRamPoolStats {
            encoded_bytes: stats.encoded_bytes,
            objects: stats.objects,
            session_references: stats.session_references,
            cross_session_objects: stats.cross_session_objects,
        })
    }
}
