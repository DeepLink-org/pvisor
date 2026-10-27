//! Same-user discovery and detached manager bootstrap. Pool ownership remains in
//! the existing pool component; removing a manager endpoint never removes RAM.
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use pvisor::host_transport::{authorize_host_peer, read_host_frame, write_host_frame};
use pvisor_daemon::memory_pool::{self, PoolConfig, PoolMode};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const PROTOCOL: u32 = 1;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const START_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, clap::Args)]
pub(super) struct Location {
    /// Private instance directory; defaults to PVISOR_DAEMON_HOME or /tmp/pvisor-daemon-UID.
    #[arg(long)]
    state: Option<PathBuf>,
}

#[derive(Clone, Debug, clap::Args)]
pub(super) struct StartOptions {
    #[command(flatten)]
    pub(super) location: Location,
    /// Enable the experimental shared pool; absent capabilities are not added to a running daemon.
    #[arg(long)]
    pub(super) memory_pool: bool,
    /// Require this mode; omission reuses any existing pool mode, or the platform default.
    #[arg(long, value_enum, requires = "memory_pool")]
    pub(super) memory_pool_mode: Option<PoolMode>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct PoolCapability {
    pub(super) socket: PathBuf,
    config: PoolConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct ServiceStatus {
    protocol: u32,
    instance_id: uuid::Uuid,
    pub(super) memory_pool: Option<PoolCapability>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    protocol: u32,
    operation: Operation,
    // Mutations are bound to the instance observed by the caller.
    instance_id: Option<uuid::Uuid>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Status,
    Stop,
}

type Response = std::result::Result<ServiceStatus, String>;

pub(super) fn print(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn private_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o777 == 0o700,
        "daemon directory must be an owned, non-symlink 0700 directory: {}",
        path.display()
    );
    Ok(())
}

impl Location {
    fn path(&self, create: bool) -> Result<PathBuf> {
        let path = match self
            .state
            .clone()
            .or_else(|| std::env::var_os("PVISOR_DAEMON_HOME").map(PathBuf::from))
        {
            Some(path) => path,
            None => Path::new("/tmp")
                .canonicalize()?
                .join(format!("pvisor-daemon-{}", unsafe { libc::geteuid() })),
        };
        ensure!(path.is_absolute(), "daemon state must be absolute");
        // Check each non-symlink ancestor. Shared /tmp is allowed only with its
        // sticky bit; all other writable ancestors must belong to this user.
        for parent in path.ancestors().skip(1) {
            let metadata = fs::symlink_metadata(parent)?;
            ensure!(
                metadata.is_dir(),
                "daemon ancestor must be a non-symlink directory"
            );
            ensure!(
                metadata.mode() & 0o022 == 0
                    || metadata.uid() == unsafe { libc::geteuid() }
                    || (metadata.uid() == 0 && metadata.mode() & 0o1000 != 0),
                "unsafe daemon state ancestor: {}",
                parent.display()
            );
        }
        if create {
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        private_directory(&path)?;
        let path = path.canonicalize()?;
        ensure!(
            path.join("control.sock").as_os_str().len() < 104,
            "daemon socket path is too long"
        );
        Ok(path)
    }
}

fn lock_file(state: &Path, name: &str) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(state.join(name))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o777 == 0o600
            && metadata.nlink() == 1,
        "unsafe daemon lock"
    );
    Ok(file)
}

fn socket_metadata(path: &Path) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.file_type().is_socket()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o777 == 0o600,
        "unsafe daemon socket"
    );
    Ok(metadata)
}

fn call(state: &Path, request: Request) -> Result<ServiceStatus> {
    let path = state.join("control.sock");
    let before = socket_metadata(&path)?;
    // These commands run before main creates its service runtime. One deadline
    // bounds connect (including a full accept queue), write and the whole frame.
    let response: Response = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            tokio::time::timeout(IO_TIMEOUT, async {
                let mut stream = tokio::net::UnixStream::connect(&path).await?;
                ensure!(
                    authorize_host_peer(&stream).is_ok(),
                    "unauthorized daemon peer"
                );
                let after = socket_metadata(&path)?;
                ensure!(
                    (before.dev(), before.ino()) == (after.dev(), after.ino()),
                    "daemon socket changed during connection"
                );
                write_host_frame(&mut stream, &request).await?;
                read_host_frame::<Response>(&mut stream).await
            })
            .await
            .context("daemon response timed out; refusing another startup")?
        })?;
    let status = response.map_err(anyhow::Error::msg)?;
    ensure!(
        status.protocol == PROTOCOL,
        "incompatible daemon protocol {}",
        status.protocol
    );
    Ok(status)
}

fn query(state: &Path) -> Result<ServiceStatus> {
    call(
        state,
        Request {
            protocol: PROTOCOL,
            operation: Operation::Status,
            instance_id: None,
        },
    )
}

pub(super) fn status(location: &Location) -> Result<ServiceStatus> {
    query(&location.path(false)?)
        .context("same-user daemon unavailable (status never starts a daemon)")
}

fn satisfies(status: ServiceStatus, options: &StartOptions) -> Result<ServiceStatus> {
    if options.memory_pool {
        let pool = status.memory_pool.as_ref().context("running daemon lacks memory-pool capability; stop its manager and restart with --memory-pool")?;
        if let Some(mode) = options.memory_pool_mode {
            ensure!(
                pool.config.mode == mode,
                "running daemon memory-pool mode conflicts; preserve live VMs and use a different --state"
            );
        }
    }
    Ok(status)
}

// Only absence or refused connections allow bootstrap. Authentication, malformed
// replies, incompatible protocols and timeouts must never start a second owner.
fn absent(error: &anyhow::Error) -> bool {
    error.downcast_ref::<std::io::Error>().is_some_and(|error| {
        matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        )
    })
}

pub(super) fn start(options: &StartOptions) -> Result<ServiceStatus> {
    let state = options.location.path(true)?;
    match query(&state) {
        Ok(status) => return satisfies(status, options),
        Err(error) if absent(&error) => {}
        Err(error) => return Err(error),
    }
    let startup = lock_file(&state, "startup.lock")?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match startup.try_lock_exclusive() {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
        ensure!(Instant::now() < deadline, "daemon startup lock timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
    match query(&state) {
        Ok(status) => return satisfies(status, options),
        Err(error) if absent(&error) => {}
        Err(error) => return Err(error),
    }
    // A missing socket is insufficient proof of manager death.
    let owner = lock_file(&state, "manager.lock")?;
    owner
        .try_lock_exclusive()
        .context("daemon manager exists but is not ready; refusing another startup")?;
    drop(owner);
    let log = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(state.join("manager.log"))?;
    let metadata = log.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o777 == 0o600
            && metadata.nlink() == 1,
        "unsafe daemon log"
    );
    let mut command = Command::new(std::env::current_exe()?.canonicalize()?);
    command
        .args(["user-service", "--state"])
        .arg(&state)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    if options.memory_pool {
        command.arg("--memory-pool");
    }
    if let Some(mode) = options.memory_pool_mode {
        command.arg("--memory-pool-mode").arg(match mode {
            PoolMode::Physical => "physical",
            PoolMode::Compressed => "compressed",
            PoolMode::PhysicalPages => "physical-pages",
        });
    }
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    loop {
        if let Some(exit) = child.try_wait()? {
            anyhow::bail!(
                "daemon startup exited {exit}; see {}",
                state.join("manager.log").display()
            );
        }
        match query(&state) {
            Ok(status) => {
                // Reap if the parent remains alive; the daemon itself has no
                // parent-lifetime pipe and survives CLI exit.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return satisfies(status, options);
            }
            Err(error) if absent(&error) => {}
            Err(error) => return Err(error),
        }
        ensure!(
            Instant::now() < deadline,
            "daemon startup timed out; child retained, inspect manager.log before retrying"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub(super) fn stop(location: &Location) -> Result<ServiceStatus> {
    let state = location.path(false)?;
    let startup = lock_file(&state, "startup.lock")?;
    // Stop does not queue behind an uncertain bootstrap.
    startup
        .try_lock_exclusive()
        .context("daemon startup/stop in progress")?;
    let observed = query(&state)?;
    let stopped = call(
        &state,
        Request {
            protocol: PROTOCOL,
            operation: Operation::Stop,
            instance_id: Some(observed.instance_id),
        },
    )?;
    let deadline = Instant::now() + START_TIMEOUT;
    let owner = lock_file(&state, "manager.lock")?;
    loop {
        match owner.try_lock_exclusive() {
            Ok(()) => return Ok(stopped),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
        ensure!(
            Instant::now() < deadline,
            "manager stop unconfirmed; pool retained"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct Endpoint(PathBuf);
impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub(super) async fn serve(options: StartOptions) -> Result<()> {
    let state = options.location.path(false)?;
    let owner = lock_file(&state, "manager.lock")?;
    owner
        .try_lock_exclusive()
        .context("same-user daemon already running")?;
    // Install signal handlers before starting any retained resource.
    let shutdown = crate::shutdown()?;
    tokio::pin!(shutdown);
    let socket = state.join("control.sock");
    match socket_metadata(&socket) {
        Ok(_) => {
            fs::remove_file(&socket)?;
        } // manager lock proves no manager owns it
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
        Err(error) => return Err(error),
    }
    let pool = if options.memory_pool {
        let config_path = state.join("memory-pool/config.json");
        let config = if config_path.try_exists()? {
            let config: PoolConfig = serde_json::from_slice(&fs::read(config_path)?)?;
            if let Some(mode) = options.memory_pool_mode {
                ensure!(
                    mode == config.mode,
                    "persisted pool mode conflicts; preserve pool and use another --state"
                );
            }
            config
        } else {
            PoolConfig {
                mode: options
                    .memory_pool_mode
                    .unwrap_or(PoolConfig::default().mode),
                ..Default::default()
            }
        };
        #[cfg(target_os = "macos")]
        ensure!(
            config.mode != PoolMode::Physical,
            "use physical-pages or compressed on macOS"
        );
        let directory = state.clone();
        let executable = std::env::current_exe()?.canonicalize()?;
        let selected = config.clone();
        let socket = tokio::task::spawn_blocking(move || {
            memory_pool::ensure_service(&directory, &executable, selected)
        })
        .await??;
        Some(PoolCapability { socket, config })
    } else {
        None
    };
    let status = ServiceStatus {
        protocol: PROTOCOL,
        instance_id: uuid::Uuid::new_v4(),
        memory_pool: pool,
    };
    let listener = tokio::net::UnixListener::bind(&socket)?;
    let endpoint = Endpoint(socket.clone());
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let monitor = crate::wait_pool(status.memory_pool.as_ref().map(|pool| pool.socket.clone()));
    tokio::pin!(monitor);
    let (stop_tx, mut stop_rx) = tokio::sync::mpsc::channel(1);
    let mut sessions = tokio::task::JoinSet::new();
    let result = loop {
        tokio::select! {
            _ = &mut shutdown => break Ok(()),
            _ = stop_rx.recv() => break Ok(()),
            result = &mut monitor => break result,
            Some(_) = sessions.join_next(), if !sessions.is_empty() => {},
            accepted = listener.accept(), if sessions.len() < 16 => {
                let (mut stream, _) = accepted?;
                if authorize_host_peer(&stream).is_err() { continue; }
                let status = status.clone();
                let stop = stop_tx.clone();
                sessions.spawn(async move {
                    let Ok(Ok(request)) = tokio::time::timeout(IO_TIMEOUT,
                        read_host_frame::<Request>(&mut stream)).await else { return; };
                    let stopping = matches!(request.operation, Operation::Stop);
                    let response: Response = if request.protocol != PROTOCOL {
                        Err("incompatible daemon protocol".into())
                    } else if stopping && request.instance_id != Some(status.instance_id) {
                        Err("daemon instance changed; stop not applied".into())
                    } else { Ok(status) };
                    let apply_stop = stopping && response.is_ok();
                    // An accepted stop survives failed/timed-out acknowledgement delivery.
                    let _ = tokio::time::timeout(IO_TIMEOUT,
                        write_host_frame(&mut stream, &response)).await;
                    if apply_stop {
                        let _ = stop.send(()).await;
                    };
                });
            }
        }
    };
    sessions.abort_all();
    while sessions.join_next().await.is_some() {}
    drop(listener);
    drop(endpoint);
    drop(owner);
    result
}
