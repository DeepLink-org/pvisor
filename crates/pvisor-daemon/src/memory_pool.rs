//! Daemon-owned cold-page pool. Its detached service outlives API restarts.
//! Physical sharing and macOS compression require a live owner. Linux compressed
//! clients retain sealed backing for local restore after service loss. No mode
//! silently replaces state or reconnects a running VM.
use crate::api::{MemoryPoolService, PoolConfig, PoolMode};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use pvisor::ram_backing::ipc::PoolClient;
#[cfg(not(target_os = "linux"))]
use pvisor::ram_backing::{ipc::serve, resident::CompressedPool};
use pvisor::ram_backing::{ipc::serve_shared, shared::SharedPool};
#[cfg(target_os = "linux")]
use pvisor::ram_backing::{
    ipc::{SealedClient, serve_sealed},
    sealed::SealedPool,
};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

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
fn probe(socket: &Path, mode: PoolMode) -> Result<()> {
    let stream = UnixStream::connect(socket)?;
    pvisor::host_transport::authorize_host_peer_sync(&stream)?;
    let mut client = PoolClient::new(stream, Duration::from_secs(2))?;
    client.stats()?;
    #[cfg(target_os = "linux")]
    match mode {
        PoolMode::Physical | PoolMode::PhysicalPages => client
            .enable_shared_mapping()
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
            .enable_shared_mapping()
            .context("pool lacks physical pages; use fresh state")?;
    }
    Ok(())
}

impl MemoryPoolService for PoolConfig {
    fn ensure_service(self, state: &Path, executable: &Path) -> Result<PathBuf> {
        let config = self;
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
            probe(&socket, config.mode)
                .context("existing pool unavailable; refusing to replace live RAM ownership")?;
            return Ok(socket);
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
    #[allow(deprecated)]
    fn run(directory: &Path) -> Result<()> {
        private_directory(directory)?;
        let config: PoolConfig = serde_json::from_slice(&fs::read(directory.join("config.json"))?)?;
        ensure!(
            config.max_bytes > 0
                && config.max_objects > 0
                && config.max_connections > 0
                && config.max_references > 0,
            "pool budgets must be positive"
        );
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(directory.join("service.lock"))?;
        lock.try_lock_exclusive()
            .context("memory-pool already has an owner")?;
        let socket = directory.join("pool.sock");
        // An existing endpoint is never unlinked or silently adopted.
        let listener = UnixListener::bind(&socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        #[cfg(not(target_os = "linux"))]
        let compressed = (config.mode != PoolMode::PhysicalPages).then(|| {
            Arc::new(Mutex::new(CompressedPool::new(
                config.max_bytes,
                config.max_objects,
            )))
        });
        let physical = config.mode == PoolMode::PhysicalPages
            || (cfg!(target_os = "linux") && config.mode == PoolMode::Physical);
        let pool = physical
            .then(|| SharedPool::new(config.max_bytes, config.max_objects))
            .transpose()?
            .map(|pool| Arc::new(Mutex::new(pool)));
        #[cfg(target_os = "linux")]
        let sealed = (config.mode == PoolMode::Compressed).then(|| {
            Arc::new(Mutex::new(SealedPool::new(
                config.max_bytes,
                config.max_objects,
            )))
        });
        let connections = Arc::new(AtomicUsize::new(0));
        println!(
            "{}",
            serde_json::json!({"ready": true, "socket": socket, "daemon_owned": true, "mode": config.mode})
        );
        for stream in listener.incoming() {
            let stream = stream?;
            if pvisor::host_transport::authorize_host_peer_sync(&stream).is_err() {
                continue;
            }
            if connections
                .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    (n < config.max_connections).then_some(n + 1)
                })
                .is_err()
            {
                continue;
            }
            let owner = pool.clone();
            #[cfg(not(target_os = "linux"))]
            let compressed = compressed.clone();
            #[cfg(target_os = "linux")]
            let sealed = sealed.clone();
            let count = connections.clone();
            let refs = config.max_references;
            std::thread::spawn(move || {
                #[cfg(target_os = "linux")]
                let result = match owner {
                    Some(owner) => serve_shared(stream, owner, refs),
                    None => serve_sealed(stream, sealed.unwrap(), refs),
                };
                #[cfg(not(target_os = "linux"))]
                let result = match owner {
                    Some(owner) => serve_shared(stream, owner, refs),
                    None => serve(stream, compressed.unwrap(), refs),
                };
                if let Err(error) = result {
                    eprintln!("memory-pool session: {error}");
                }
                count.fetch_sub(1, Ordering::AcqRel);
            });
        }
        Ok(())
    }
}
