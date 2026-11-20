//! Product-side cold RAM pool authorization and diagnostic destinations.
//! VM mapping transitions and fault/device recovery live in pvisor-vm.
use crate::env::{self, ram::METRICS_ENV};
use crate::ram_backing::ipc::{PoolClient, RemoteObject};
use pvisor_vm::api::ColdRamControl;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use pvisor_vm::api::{FrozenMemory, RuntimeSupport, VmPlatform};
use std::io;
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

const POOL_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);

/// Owned, negotiated storage prepared before building a VMM. Starting the pager
/// consumes this connection rather than reconnecting to a possibly replaced pool.
pub(crate) struct PreparedPager {
    store: PagerStore,
    metrics: bool,
}

enum PagerStore {
    Disabled,
    Local(crate::ram_backing::resident::LocalColdRamStore),
    Physical(PoolClient),
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    RestorePhysical(PoolClient),
    #[cfg(target_os = "macos")]
    Compressed(PoolClient),
    #[cfg(target_os = "linux")]
    Sealed(crate::ram_backing::ipc::SealedClient),
}

impl PreparedPager {
    pub(crate) fn start(self, handle: pvisor_vm::api::VmmHandle) -> io::Result<()> {
        let options = |physical| pvisor_vm::api::ColdRamOptions {
            metrics: self.metrics,
            page_granular: physical,
            share_resident: physical,
        };
        match self.store {
            PagerStore::Disabled => Ok(()),
            PagerStore::Local(store) => handle.start_cold_pager(store, options(false)),
            PagerStore::Physical(pool) => handle.start_cold_pager(pool, options(true)),
            #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
            PagerStore::RestorePhysical(pool) => {
                handle.start_restored_physical_pool(pool, self.metrics)
            }
            #[cfg(target_os = "macos")]
            PagerStore::Compressed(pool) => handle.start_cold_pager(pool, options(false)),
            #[cfg(target_os = "linux")]
            PagerStore::Sealed(pool) => handle.start_cold_pager(pool, options(false)),
        }
    }
}

pub(super) fn prepare(
    selection: &super::supported::RamPager,
    memory_mib: u32,
) -> io::Result<PreparedPager> {
    let metrics = env::os(METRICS_ENV).is_some();
    if matches!(selection, super::supported::RamPager::Local) {
        // Per-instance ceilings: at most half configured RAM as encoded payload,
        // and one object per 64 KiB RAM block. Index overhead is bounded separately.
        let ram_bytes = usize::try_from(u64::from(memory_mib) * 1024 * 1024)
            .map_err(|_| io::Error::other("local cold RAM budget exceeds address space"))?;
        let store = crate::ram_backing::resident::LocalColdRamStore::new(
            ram_bytes / 2,
            ram_bytes.div_ceil(64 * 1024),
        );
        return Ok(PreparedPager {
            store: PagerStore::Local(store),
            metrics,
        });
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    if let super::supported::RamPager::RestorePhysical { socket, generation } = selection {
        let pool = prepare_restore_pool(socket)?;
        if pool_generation(pool.shared_file()?.as_ref())? != *generation {
            return Err(io::Error::other("Physical restore pool generation changed"));
        }
        return Ok(PreparedPager {
            store: PagerStore::RestorePhysical(pool),
            metrics,
        });
    }
    let super::supported::RamPager::Pool(path) = selection else {
        return Ok(PreparedPager {
            store: PagerStore::Disabled,
            metrics,
        });
    };
    prepare_pool(path, metrics)
}

/// Start opt-in self-query diagnostics independently of cold-pool selection.
pub(crate) fn start_diagnostics(handle: &pvisor_vm::api::VmmHandle) -> io::Result<()> {
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    let _ = handle;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if let Some(directory) = env::os(env::ram::PAGE_INVENTORY_ENV) {
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
        let process_directory =
            env::os(env::ram::PROCESS_INVENTORY_ENV).map(std::path::PathBuf::from);
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
                        let result = (|| {
                            let mut file = tempfile::NamedTempFile::new_in(&directory)?;
                            serde_json::to_writer(file.as_file_mut(), &value).map_err(io::Error::other)?;
                            file.persist_noclobber(directory.join(format!("{}.json", std::process::id())))
                                .map_err(|error|error.error)?;
                            Ok::<_,io::Error>(())
                        })();
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
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if env::os(METRICS_ENV).is_some() {
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
    Ok(())
}

fn connect_pool(
    path: &Path,
    connect: impl FnOnce(&Path) -> io::Result<UnixStream>,
) -> io::Result<PoolClient> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("pool parent missing"))?;
    let metadata = std::fs::symlink_metadata(parent)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::other(
            "experimental pool directory must belong to this user and be private",
        ));
    }
    let stream = connect(path)?;
    crate::host_transport::authorize_host_peer_sync(&stream).map_err(io::Error::other)?;
    PoolClient::new(stream, POOL_EXCHANGE_TIMEOUT)
}

/// Authenticate and negotiate an explicit host-only pool before VMM construction.
/// Failure leaves the socket and pool ownership untouched; dropping the prepared
/// pager closes the session. No RAM is published until `start` is called.
pub(crate) fn prepare_pool(path: &Path, metrics: bool) -> io::Result<PreparedPager> {
    let mut pool = connect_pool(path, |path| UnixStream::connect(path))?;
    #[cfg(target_os = "macos")]
    let store = match pool.enable_shared_mapping() {
        Ok(()) => PagerStore::Physical(pool),
        Err(_) => {
            // Only a fully consumed mode rejection permits legacy compression.
            // A malformed descriptor or dead transport must fail startup.
            pool.stats()?;
            PagerStore::Compressed(pool)
        }
    };
    #[cfg(target_os = "linux")]
    let store = {
        if pool.enable_shared_mapping().is_err() {
            // Only the negotiated sealed protocol may fall back to local
            // decode; legacy remote GET ownership is never used on Linux.
            PagerStore::Sealed(crate::ram_backing::ipc::SealedClient::new(pool)?)
        } else {
            PagerStore::Physical(pool)
        }
    };
    Ok(PreparedPager { store, metrics })
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(super) struct PoolGeneration {
    device: u64,
    inode: u64,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn pool_generation(file: &std::fs::File) -> io::Result<PoolGeneration> {
    let metadata = file.metadata()?;
    Ok(PoolGeneration {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn connect_restore_pool(path: &Path) -> io::Result<UnixStream> {
    use std::os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    };

    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Physical restore pool socket path",
        ));
    }
    address.sun_family = libc::AF_UNIX as _;
    for (destination, byte) in address.sun_path.iter_mut().zip(bytes) {
        *destination = *byte as _;
    }
    let length =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    let raw = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: socket returned an owned fd, and the address fits in sockaddr_un.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // Linux returns EAGAIN for a full Unix accept queue. Fail this admission
    // before doing any peer negotiation; another call can retry.
    if unsafe { libc::connect(fd.as_raw_fd(), std::ptr::from_ref(&address).cast(), length) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd.into())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn prepare_restore_pool(path: &Path) -> io::Result<PoolClient> {
    use std::os::unix::fs::FileTypeExt;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::other(
            "Physical restore pool requires a private owned socket",
        ));
    }
    let mut pool = connect_pool(path, connect_restore_pool)?;
    pool.enable_shared_mapping()?;
    pool.stats()?;
    Ok(pool)
}

// Retain the preflight inode without transferring a supervisor-owned session.
// Only the runner's connection may publish pages under its own pidfd.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(super) struct BoundRestorePool {
    pub(super) socket: std::path::PathBuf,
    pub(super) generation: PoolGeneration,
    _mapping: std::sync::Arc<std::fs::File>,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(super) fn preflight_restore_pool(path: &Path) -> io::Result<BoundRestorePool> {
    let file = prepare_restore_pool(path)?.shared_file()?;
    Ok(BoundRestorePool {
        socket: path.canonicalize()?,
        generation: pool_generation(&file)?,
        _mapping: file,
    })
}

impl pvisor_vm::api::ColdRamStore for PoolClient {
    type Object = RemoteObject;
    fn shared_mapping(&self, object: &Self::Object) -> Option<pvisor_vm::api::SharedRamMapping> {
        PoolClient::shared_mapping(self, object)
    }
    fn put(&mut self, bytes: &[u8]) -> io::Result<Self::Object> {
        PoolClient::put_duplicate(self, bytes)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Mutex};

    fn private_directory() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    #[test]
    fn lost_pool_is_rejected_before_vm_entry_without_replacing_its_socket() {
        let directory = private_directory();
        let socket = directory.path().join("pool.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let before = std::fs::symlink_metadata(&socket).unwrap();
        drop(listener);

        for _ in 0..2 {
            let error = prepare_pool(&socket, false).err().unwrap();
            assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
            let after = std::fs::symlink_metadata(&socket).unwrap();
            assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        }
    }

    #[test]
    fn pool_protocol_failure_is_rejected_during_preparation() {
        use std::io::Write;
        let directory = private_directory();
        let socket = directory.path().join("pool.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"INVALID!").unwrap();
        });
        let error = prepare_pool(&socket, false).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        server.join().unwrap();
    }

    #[test]
    fn interrupted_pool_negotiation_preserves_endpoint_for_healthy_retry() {
        use std::io::{Read, Write};
        use std::net::Shutdown;

        for fault in ["identity_eof", "mapping_eof", "mapping_status"] {
            let directory = private_directory();
            let socket = directory.path().join("pool.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let before = std::fs::symlink_metadata(&socket).unwrap();
            let accepting = listener.try_clone().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = accepting.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                stream.write_all(b"PVMEMP2\0").unwrap();
                if fault == "identity_eof" {
                    stream.write_all(&[1; 7]).unwrap();
                } else {
                    stream.write_all(&[1; 16]).unwrap();
                    let mut operation = [0];
                    stream.read_exact(&mut operation).unwrap();
                    assert_eq!(operation, [6], "expected mapping negotiation");
                    if fault == "mapping_status" {
                        stream.write_all(&[2]).unwrap(); // Not a mode rejection.
                    }
                }
                stream.shutdown(Shutdown::Write).unwrap();
                let mut operation = [0];
                assert_eq!(
                    stream.read(&mut operation).unwrap(),
                    0,
                    "broken negotiation must close, not try another mode"
                );
            });
            let error = prepare_pool(&socket, false).err().unwrap();
            if fault != "identity_eof" {
                // Fallback cannot negotiate or query an already-poisoned client.
                assert_eq!(error.to_string(), "pool session has failed");
            } else {
                assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
            }
            server.join().unwrap();
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                io::ErrorKind::WouldBlock,
                "failed preparation must not reconnect"
            );
            listener.set_nonblocking(false).unwrap();

            // The original owner can serve a new session at the unchanged endpoint.
            let pool = Arc::new(Mutex::new(
                crate::ram_backing::shared::SharedPool::new(1 << 20, 256).unwrap(),
            ));
            let shared = pool.clone();
            let server = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                crate::ram_backing::ipc::serve_shared(stream, shared, 256).unwrap();
            });
            let prepared = prepare_pool(&socket, false).unwrap();
            assert!(matches!(&prepared.store, PagerStore::Physical(_)));
            assert_eq!(pool.lock().unwrap().object_count(), 0);
            drop(prepared);
            server.join().unwrap();
            let after = std::fs::symlink_metadata(&socket).unwrap();
            assert_eq!(
                (before.dev(), before.ino(), before.uid(), before.mode()),
                (after.dev(), after.ino(), after.uid(), after.mode())
            );
            assert_eq!(pool.lock().unwrap().object_count(), 0);
        }
    }

    #[test]
    fn prepared_physical_pool_owns_one_session_without_publishing_ram() {
        let directory = private_directory();
        let socket = directory.path().join("pool.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let pool = Arc::new(Mutex::new(
            crate::ram_backing::shared::SharedPool::new(1 << 20, 256).unwrap(),
        ));
        let shared = pool.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            crate::ram_backing::ipc::serve_shared(stream, shared, 256).unwrap();
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        });
        let prepared = prepare_pool(&socket, true).unwrap();
        assert!(matches!(&prepared.store, PagerStore::Physical(_)));
        assert!(prepared.metrics);
        assert_eq!(pool.lock().unwrap().object_count(), 0);
        drop(prepared);
        server.join().unwrap();
        assert_eq!(pool.lock().unwrap().object_count(), 0);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn accept_restore_client(listener: &UnixListener) -> UnixStream {
        use std::os::fd::AsRawFd;

        let mut fd = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut fd, 1, 5000) }, 1);
        assert_ne!(fd.revents & libc::POLLIN, 0);
        listener.accept().unwrap().0
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn serve_restore_client(
        listener: &UnixListener,
        owner: &Arc<Mutex<crate::ram_backing::shared::SharedPool>>,
    ) -> std::thread::JoinHandle<()> {
        let listener = listener.try_clone().unwrap();
        let owner = owner.clone();
        std::thread::spawn(move || {
            let stream = accept_restore_client(&listener);
            crate::ram_backing::ipc::serve_shared(stream, owner, 32).unwrap();
        })
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn physical_restore_pool_rejects_missing_and_stale_endpoints() {
        let directory = private_directory();
        let socket = directory.path().join("pool.sock");
        assert_eq!(
            preflight_restore_pool(&socket).err().unwrap().kind(),
            io::ErrorKind::NotFound
        );
        assert!(!socket.exists());

        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let before = std::fs::symlink_metadata(&socket).unwrap();
        drop(listener);
        assert_eq!(
            preflight_restore_pool(&socket).err().unwrap().kind(),
            io::ErrorKind::ConnectionRefused
        );
        let after = std::fs::symlink_metadata(&socket).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn physical_restore_pool_rejects_a_full_listener_and_can_retry() {
        use super::super::supported::RamPager;
        use crate::ram_backing::shared::SharedPool;
        use std::os::fd::AsRawFd;

        let directory = private_directory();
        let socket = directory.path().join("pool.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let before = std::fs::symlink_metadata(&socket).unwrap();
        let owner = Arc::new(Mutex::new(SharedPool::new(1 << 20, 32).unwrap()));
        let server = serve_restore_client(&listener, &owner);
        let bound = preflight_restore_pool(&socket).unwrap();
        server.join().unwrap();
        let selection = RamPager::RestorePhysical {
            socket: bound.socket.clone(),
            generation: bound.generation,
        };

        // Linux's zero backlog admits one queued connection while no one accepts.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        let filler = UnixStream::connect(&socket).unwrap();
        let (results, received) = std::sync::mpsc::channel();
        let probe_socket = socket.clone();
        let probe = std::thread::spawn(move || {
            let preflight = preflight_restore_pool(&probe_socket)
                .err()
                .map(|e| e.kind());
            let runner = prepare(&selection, 1).err().map(|e| e.kind());
            results.send((preflight, runner)).unwrap();
            selection
        });
        let Ok(outcome) = received.recv_timeout(Duration::from_secs(2)) else {
            // Closing the listener releases a regressed blocking connect.
            drop(listener);
            drop(filler);
            probe.join().unwrap();
            panic!("saturated listener stalled Raw admission");
        };
        let selection = probe.join().unwrap();
        assert_eq!(
            outcome,
            (
                Some(io::ErrorKind::WouldBlock),
                Some(io::ErrorKind::WouldBlock)
            )
        );
        listener.set_nonblocking(true).unwrap();
        assert!(listener.accept().is_ok());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        listener.set_nonblocking(false).unwrap();
        drop(filler);

        let server = serve_restore_client(&listener, &owner);
        let prepared = prepare(&selection, 1).unwrap();
        assert!(matches!(prepared.store, PagerStore::RestorePhysical(_)));
        drop(prepared);
        server.join().unwrap();
        let after = std::fs::symlink_metadata(&socket).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        assert_eq!(owner.lock().unwrap().object_count(), 0);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn physical_restore_pool_times_out_during_mapping_and_can_retry() {
        use crate::ram_backing::shared::SharedPool;
        use std::io::{Read, Write};

        let directory = private_directory();
        let socket = directory.path().join("pool.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let before = std::fs::symlink_metadata(&socket).unwrap();
        let accepting = listener.try_clone().unwrap();
        let server = std::thread::spawn(move || {
            let mut stream = accept_restore_client(&accepting);
            let timeout = Some(POOL_EXCHANGE_TIMEOUT + Duration::from_secs(2));
            stream.set_read_timeout(timeout).unwrap();
            stream.set_write_timeout(timeout).unwrap();
            stream.write_all(b"PVMEMP2\0").unwrap();
            stream.write_all(&[1; 16]).unwrap();
            let mut operation = [0];
            stream.read_exact(&mut operation).unwrap();
            assert_eq!(operation, [6], "expected mapping negotiation");
            stream.write_all(&[0]).unwrap();
            // Keep the session open without sending the mapping descriptor.
            assert_eq!(stream.read(&mut operation).unwrap(), 0);
        });
        assert_eq!(
            preflight_restore_pool(&socket).err().unwrap().kind(),
            io::ErrorKind::TimedOut
        );
        server.join().unwrap();
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        listener.set_nonblocking(false).unwrap();

        let owner = Arc::new(Mutex::new(SharedPool::new(1 << 20, 32).unwrap()));
        let server = serve_restore_client(&listener, &owner);
        assert!(preflight_restore_pool(&socket).is_ok());
        server.join().unwrap();
        let after = std::fs::symlink_metadata(&socket).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        assert_eq!(owner.lock().unwrap().object_count(), 0);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn physical_restore_pool_negotiates_two_sessions_and_rejects_a_different_generation() {
        use super::super::supported::RamPager;
        use crate::ram_backing::shared::SharedPool;
        let directory = private_directory();
        let socket = directory.path().join("pool.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let pool = Arc::new(Mutex::new(SharedPool::new(1 << 20, 32).unwrap()));
        let (disconnect, connections) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            std::thread::scope(|scope| {
                for index in 0..3 {
                    let (stream, _) = listener.accept().unwrap();
                    if index == 2 {
                        disconnect.send(stream.try_clone().unwrap()).unwrap();
                    }
                    let pool = pool.clone();
                    scope.spawn(move || {
                        crate::ram_backing::ipc::serve_shared(stream, pool, 32).unwrap();
                    });
                }
            });
        });
        let bound = preflight_restore_pool(&socket).unwrap();
        let selection = RamPager::RestorePhysical {
            socket: bound.socket.clone(),
            generation: bound.generation,
        };
        let connect = || match prepare(&selection, 1).unwrap().store {
            PagerStore::RestorePhysical(pool) => pool,
            _ => panic!("Physical restore cannot fall back to a different store"),
        };
        let mut first = connect();
        let mut second = connect();
        let bytes = [0x42; 4096];
        assert!(first.put_duplicate(&bytes).is_err());
        let second_object = second.put_duplicate(&bytes).unwrap();
        let first_object = first.put_duplicate(&bytes).unwrap();
        let first_mapping = first.shared_mapping(&first_object).unwrap();
        let second_mapping = second.shared_mapping(&second_object).unwrap();
        assert_eq!(first_mapping.offset, second_mapping.offset);
        assert_eq!(
            pool_generation(&first_mapping.file).unwrap(),
            bound.generation
        );
        assert_eq!(
            pool_generation(&second_mapping.file).unwrap(),
            bound.generation
        );
        assert_eq!(second.stats().unwrap().cross_session_objects, 1);
        first.release(first_object).unwrap();
        assert_eq!(first.stats().unwrap().session_references, 0);
        drop(first);
        let mut output = [0; 4096];
        second.restore(&second_object, &mut output).unwrap();
        assert_eq!(output, bytes);
        assert_eq!(second.stats().unwrap().cross_session_objects, 0);
        second.release(second_object).unwrap();
        assert_eq!(second.stats().unwrap().session_references, 0);
        let connection = connections.recv_timeout(Duration::from_secs(5)).unwrap();
        connection.shutdown(std::net::Shutdown::Both).unwrap();
        assert!(second.stats().is_err());
        assert!(
            second
                .stats()
                .unwrap_err()
                .to_string()
                .contains("session has failed")
        );
        drop(second);
        server.join().unwrap();

        let replacement = directory.path().join("replacement.sock");
        let listener = UnixListener::bind(&replacement).unwrap();
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o600)).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let pool = Arc::new(Mutex::new(SharedPool::new(1 << 20, 32).unwrap()));
            crate::ram_backing::ipc::serve_shared(stream, pool, 32).unwrap();
        });
        let selection = RamPager::RestorePhysical {
            socket: replacement,
            generation: bound.generation,
        };
        let error = prepare(&selection, 1).err().unwrap();
        assert!(error.to_string().contains("generation changed"));
        server.join().unwrap();
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn physical_restore_pool_does_not_fall_back_to_sealed_compression() {
        use crate::ram_backing::sealed::SealedPool;
        let directory = private_directory();
        let socket = directory.path().join("pool.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let pool = Arc::new(Mutex::new(SealedPool::new(1 << 20, 32)));
            crate::ram_backing::ipc::serve_sealed(stream, pool, 32).unwrap();
        });
        let error = preflight_restore_pool(&socket).err().unwrap();
        assert!(error.to_string().contains("physical sharing unsupported"));
        assert!(socket.exists());
        server.join().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sealed_pool_is_negotiated_before_vm_entry() {
        let directory = private_directory();
        let socket = directory.path().join("pool.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            crate::ram_backing::ipc::serve_sealed(
                stream,
                Arc::new(Mutex::new(crate::ram_backing::sealed::SealedPool::new(
                    1 << 20,
                    256,
                ))),
                256,
            )
            .unwrap();
        });
        let prepared = prepare_pool(&socket, false).unwrap();
        assert!(matches!(&prepared.store, PagerStore::Sealed(_)));
        drop(prepared);
        server.join().unwrap();
    }
}
