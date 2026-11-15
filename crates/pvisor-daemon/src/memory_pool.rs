//! Daemon-owned cold-page pool. Its detached service outlives API restarts.
//! Physical clients retain their backing across owner loss. Recovery fences the
//! old owner and charges witnessed retired generations before allocating again.
//! Legacy macOS compression still requires a live owner.
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use pvisor::ram_backing::ipc::PoolClient;
#[cfg(not(target_os = "linux"))]
use pvisor::ram_backing::resident::CompressedPool;
use pvisor::ram_backing::{
    generation::{PhysicalGeneration, retired_bytes_before_start},
    ipc::AsyncPoolService,
    shared::SharedPool,
};
#[cfg(target_os = "linux")]
use pvisor::ram_backing::{ipc::SealedClient, sealed::SealedPool};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Storage selection, fixed for the lifetime of this pool state directory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum PoolMode {
    /// Linux raw 4 KiB pages shared by private mappings and kernel COW;
    /// no userfaultfd. Old macOS records use this label for legacy compression.
    #[default]
    Physical,
    /// Deduplicated compressed blocks. Linux retains sealed client backing;
    /// macOS HVF restores through the live pool and requires it to stay alive.
    Compressed,
    /// Raw host pages with private COW mappings: 16 KiB on Apple Silicon,
    /// 4 KiB on Linux. Separate from the legacy macOS `physical` record label.
    PhysicalPages,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolConfig {
    /// Missing in older persisted configurations means physical sharing.
    #[serde(default)]
    pub mode: PoolMode,
    pub max_bytes: usize,
    pub max_objects: usize,
    pub max_connections: usize,
    pub max_references: usize,
    /// Disposable physical-page fingerprints; independent of payload slots.
    /// Zero disables observation retention; existing fast buckets remain usable.
    #[serde(default = "default_max_candidates")]
    pub max_candidates: usize,
}
fn default_max_candidates() -> usize {
    pvisor::ram_backing::shared::DEFAULT_MAX_CANDIDATES
}
impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            mode: if cfg!(target_os = "macos") {
                PoolMode::Compressed
            } else {
                PoolMode::Physical
            },
            max_bytes: 512 * 1024 * 1024,
            max_objects: 32768,
            max_connections: 32,
            max_references: 32768,
            max_candidates: default_max_candidates(),
        }
    }
}
fn private_directory(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0,
        "pool directory must be private and owned by this user"
    );
    Ok(())
}

fn physical_mode(mode: PoolMode) -> bool {
    mode == PoolMode::PhysicalPages || (cfg!(target_os = "linux") && mode == PoolMode::Physical)
}

fn service_lock(directory: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory.join("service.lock"))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o7777 == 0o600
            && metadata.nlink() == 1,
        "unsafe physical service lock"
    );
    Ok(file)
}

fn recover_endpoint(directory: &Path, socket: &Path, mode: PoolMode) -> Result<()> {
    ensure!(
        physical_mode(mode),
        "legacy pool cannot recover lost ownership"
    );
    let lock = service_lock(directory)?;
    lock.try_lock_exclusive()
        .context("pool owner is still alive; refusing endpoint replacement")?;
    PhysicalGeneration::recoverable(directory)
        .context("pool has no recoverable generation; preserve legacy VM ownership")?;
    let metadata = fs::symlink_metadata(socket)?;
    ensure!(
        metadata.file_type().is_socket()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o7777 == 0o600,
        "unsafe stale pool endpoint"
    );
    let checked = fs::symlink_metadata(socket)?;
    ensure!(
        metadata.dev() == checked.dev() && metadata.ino() == checked.ino(),
        "pool endpoint changed during recovery"
    );
    // Only this fenced, private endpoint is removed. Backings and generation
    // records remain owned by their existing processes, never by a saved PID.
    fs::remove_file(socket)?;
    Ok(())
}

fn probe(socket: &Path, mode: PoolMode) -> Result<()> {
    let stream = UnixStream::connect(socket)?;
    pvisor::host_transport::authorize_host_peer_sync(&stream)?;
    let mut client = PoolClient::new(stream, Duration::from_secs(2))?;
    client.stats()?;
    #[cfg(target_os = "linux")]
    match mode {
        PoolMode::Physical | PoolMode::PhysicalPages => client
            .check_shared_mapping()
            .context("pool lacks physical sharing; preserve active VMs and use fresh pool state")?,
        PoolMode::Compressed => {
            SealedClient::new(client).context(
                "pool lacks sealed compression; preserve active VMs and use fresh pool state",
            )?;
        }
    }
    // Older macOS configurations called the legacy compressed backend physical.
    // Keep those records usable without changing their persisted ownership.
    #[cfg(target_os = "macos")]
    if mode == PoolMode::PhysicalPages {
        client
            .check_shared_mapping()
            .context("pool lacks physical pages; use fresh state")?;
    }
    Ok(())
}
/// Start or reuse a private, configuration-bound service. A pool startup lock
/// serializes callers, including standalone managers. The state directory must
/// already exist and be owned/private. A stale physical endpoint is replaced
/// only after owner fencing and generation validation; never kills by saved PID.
pub fn ensure_service(state: &Path, executable: &Path, config: PoolConfig) -> Result<PathBuf> {
    private_directory(state)?;
    let state = state.canonicalize()?;
    let directory = state.join("memory-pool");
    use std::os::unix::fs::DirBuilderExt;
    if let Err(error) = fs::DirBuilder::new().mode(0o700).create(&directory)
        && error.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(error.into());
    }
    private_directory(&directory)?;
    let bases = directory.join("shared-kernel");
    if let Err(error) = fs::DirBuilder::new().mode(0o700).create(&bases)
        && error.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(error.into());
    }
    private_directory(&bases)?;
    let startup = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("startup.lock"))?;
    startup.lock_exclusive()?;
    let record = directory.join("config.json");
    if record.try_exists()? {
        ensure!(
            serde_json::from_slice::<PoolConfig>(&fs::read(&record)?)? == config,
            "existing memory-pool configuration differs; do not replace live pool"
        );
    } else {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&record)?;
        serde_json::to_writer(&mut file, &config)?;
        file.sync_all()?;
        File::open(&directory)?.sync_all()?;
    }
    let socket = directory.join("pool.sock");
    if socket.try_exists()? {
        match probe(&socket, config.mode) {
            Ok(()) => return Ok(socket),
            Err(error) => {
                recover_endpoint(&directory, &socket, config.mode).with_context(|| {
                    format!("existing pool unavailable ({error:#}); recovery refused")
                })?;
            }
        }
    }
    let log = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(directory.join("service.log"))?;
    let mut command = Command::new(executable);
    command
        .arg("memory-pool")
        .arg("--directory")
        .arg(&directory)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("memory-pool service exited: {status}");
        }
        if socket.try_exists()? && probe(&socket, config.mode).is_ok() {
            return Ok(socket);
        }
        ensure!(
            Instant::now() < deadline,
            "memory-pool startup timed out; retained child may still own RAM"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Foreground daemon component, also used by bounded real-VM benchmarks.
/// The private config fixes memory/connection bounds before any VM connects.
pub fn run(directory: &Path) -> Result<()> {
    private_directory(directory)?;
    let config: PoolConfig = serde_json::from_slice(&fs::read(directory.join("config.json"))?)?;
    ensure!(
        config.max_bytes > 0
            && config.max_objects > 0
            && config.max_connections > 0
            && config.max_references > 0,
        "pool budgets must be positive"
    );
    let lock = service_lock(directory)?;
    lock.try_lock_exclusive()
        .context("memory-pool already has an owner")?;
    let socket = directory.join("pool.sock");
    let (service, _maintenance) = if physical_mode(config.mode) {
        let mut pool = SharedPool::with_candidate_limit(
            config.max_bytes,
            config.max_objects,
            config.max_candidates,
        )?;
        let retired = retired_bytes_before_start(directory)?;
        pool.limit_publication_bytes(config.max_bytes.saturating_sub(retired));
        let generation = Arc::new(PhysicalGeneration::publish(
            directory,
            &pool.readonly_file(),
        )?);
        let pool = Arc::new(Mutex::new(pool));
        let maintenance =
            GenerationMaintenance::start(pool.clone(), generation.clone(), config.max_bytes);
        (
            AsyncPoolService::shared(
                pool,
                Some(generation),
                config.max_connections,
                config.max_references,
            )?,
            Some(maintenance),
        )
    } else {
        #[cfg(target_os = "linux")]
        let service = AsyncPoolService::sealed(
            Arc::new(Mutex::new(SealedPool::new(
                config.max_bytes,
                config.max_objects,
            ))),
            config.max_connections,
            config.max_references,
        )?;
        #[cfg(not(target_os = "linux"))]
        let service = AsyncPoolService::compressed(
            Arc::new(Mutex::new(CompressedPool::new(
                config.max_bytes,
                config.max_objects,
            ))),
            config.max_connections,
            config.max_references,
        )?;
        (service, None)
    };
    // A foreground owner never unlinks/adopts a previous endpoint. Only the
    // manager's fenced recovery path may remove a validated stale socket.
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(4)
        .thread_name("pvisor-pool")
        .enable_all()
        .build()?
        .block_on(async move {
            let listener = tokio::net::UnixListener::from_std(listener)?;
            println!(
                "{}",
                serde_json::json!({"ready": true, "socket": socket, "daemon_owned": true, "mode": config.mode, "async_sessions": true})
            );
            loop {
                let (stream, _) = listener.accept().await?;
                match service.connection(stream) {
                    Ok(connection) => {
                        tokio::spawn(async move {
                            if let Err(error) = connection.await {
                                eprintln!("memory-pool session: {error}");
                            }
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => eprintln!("memory-pool admission: {error}"),
                }
            }
            #[allow(unreachable_code)]
            Ok::<(), std::io::Error>(())
        })?;
    Ok(())
}

struct GenerationMaintenance {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl GenerationMaintenance {
    fn start(
        pool: Arc<Mutex<SharedPool>>,
        generation: Arc<PhysicalGeneration>,
        budget: usize,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                // One bounded metadata walk per owner, not per VM or page.
                // Any accounting failure closes admission, not live mappings.
                let available = match generation.retained_bytes() {
                    Ok(retired) => budget.saturating_sub(retired),
                    Err(error) => {
                        eprintln!(
                            "memory-pool generation accounting: {error}; publication disabled"
                        );
                        0
                    }
                };
                pool.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .limit_publication_bytes(available);
                std::thread::park_timeout(Duration::from_secs(1));
            }
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for GenerationMaintenance {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn legacy_configs_get_an_independent_fingerprint_budget() {
        let config: PoolConfig = serde_json::from_value(serde_json::json!({
            "max_bytes": 4096,
            "max_objects": 1,
            "max_connections": 1,
            "max_references": 1
        }))
        .unwrap();
        assert_eq!(config.max_candidates, default_max_candidates());
        let mut value = serde_json::to_value(&config).unwrap();
        value["max_candidates"] = 0.into();
        let disabled: PoolConfig = serde_json::from_value(value).unwrap();
        assert_eq!(disabled.max_candidates, 0);
        assert_eq!(disabled.max_objects, 1);
    }
}
