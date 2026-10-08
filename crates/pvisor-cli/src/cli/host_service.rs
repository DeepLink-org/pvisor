//! Persistent typed Job authority. Frontends launch authorized workers in their
//! original terminal session; the service supplies the command over a private FD.
//! JSON uses the shared bounded newline host transport. SCM_RIGHTS marker bytes
//! are transport-only and must never be consumed by the JSON frame reader.
use super::{
    host::JobCommand,
    host_fds,
    host_process::{OwnedTree, TerminalOwner},
};
use anyhow::{Context, ensure};
use fs2::FileExt;
pub(super) use pvisor::host_transport::read_host_frame_sync as read_frame;
use pvisor::host_transport::{encode_host_frame as encoded, write_host_frame_sync as write_frame};
use pvisor_core::host_protocol::{
    AGENTCTL_HOST_VERSION, AgentCtlHostError, AgentCtlHostErrorCode, AgentCtlHostRequest,
    AgentCtlHostResponse, AgentCtlTarget,
};
#[cfg(all(test, target_os = "linux"))]
use pvisor_journal::api::JournalStore;
use serde::{Deserialize, Serialize};
use std::os::{
    fd::{AsRawFd, FromRawFd, OwnedFd},
    unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const INTERNAL: &str = "--pvisor-internal-host";
const INTERNAL_FD: i32 = 3;
const SCHEMA: &str = "pvisor-job-ticket-4";
const GRACE: Duration = Duration::from_secs(2);
static WORKER_DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
static WORKER_CANCEL: OnceLock<tokio_util::sync::CancellationToken> = OnceLock::new();
static WORKER_TARGET: OnceLock<Option<AgentCtlTarget>> = OnceLock::new();
static WORKER_EVENTS: OnceLock<Arc<Mutex<UnixStream>>> = OnceLock::new();
static WORKER_READY: AtomicBool = AtomicBool::new(false);

pub(super) fn notify_cancel(signal: i32) {
    if let Some(token) = WORKER_CANCEL.get() {
        token.cancel();
    }
    if WORKER_READY.load(Ordering::SeqCst)
        && let Some(channel) = WORKER_EVENTS.get()
    {
        let mut channel = channel.lock().unwrap_or_else(|e| e.into_inner());
        let _ = write_frame(&mut channel, &WorkerEvent::Cancelled { signal });
    }
}

pub(super) fn worker_directory() -> Option<&'static Path> {
    WORKER_DIRECTORY.get().map(PathBuf::as_path)
}

pub(super) fn notify_cleanup() {
    if WORKER_READY.load(Ordering::SeqCst)
        && let Some(channel) = WORKER_EVENTS.get()
    {
        let mut channel = channel.lock().unwrap_or_else(|e| e.into_inner());
        let _ = write_frame(&mut channel, &WorkerEvent::Finalizing);
    }
}

pub(super) fn worker_cancellation() -> Option<tokio_util::sync::CancellationToken> {
    WORKER_CANCEL.get().cloned()
}
pub(super) fn check_cancelled() -> anyhow::Result<()> {
    ensure!(
        !WORKER_CANCEL
            .get()
            .is_some_and(|token| token.is_cancelled()),
        "host request cancelled before operation"
    );
    Ok(())
}
pub(super) fn check_record(record: &pvisor::RunRecord) -> anyhow::Result<()> {
    check_cancelled()?;
    if let Some(Some(target)) = WORKER_TARGET.get() {
        super::host::check_target(target, record)?;
    }
    Ok(())
}
pub(super) fn vm_control_socket(requested: Option<&Path>) -> anyhow::Result<Option<PathBuf>> {
    let Some(dir) = WORKER_DIRECTORY.get() else {
        return Ok(requested.map(Path::to_owned));
    };
    if let Some(path) = requested {
        let absolute = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        };
        ensure!(
            absolute.parent() == Some(dir.as_path()),
            "custom VM control socket must be directly in the private host service directory {}",
            dir.display()
        );
        return Ok(Some(absolute));
    }
    Ok(Some(dir.join(format!("vm-{}.sock", uuid::Uuid::new_v4()))))
}
pub(super) fn reject_guest_exposure(
    paths: impl IntoIterator<Item = PathBuf>,
) -> anyhow::Result<()> {
    let Some(dir) = WORKER_DIRECTORY.get() else {
        return Ok(());
    };
    for path in paths {
        let path = fs::canonicalize(&path)
            .with_context(|| format!("resolve guest source {}", path.display()))?;
        ensure!(
            !dir.starts_with(&path) && !path.starts_with(dir),
            "guest filesystem source {} exposes private host service state",
            path.display()
        );
    }
    Ok(())
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Generation {
    generation: String,
    capability: String,
    // Old manifests are readable only to diagnose/refuse an old live listener.
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    digest: Option<String>,
    #[serde(default)]
    pid: Option<u32>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextData {
    client_pid: u32,
    executable: Executable,
    cwd: PathBuf,
    environment: Vec<(Vec<u8>, Vec<u8>)>,
    generation: String,
    command: JobCommand,
    terminal: Option<super::terminal::ServiceTerminalContext>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Executable {
    path: PathBuf,
    device: u64,
    inode: u64,
    digest: String,
}
fn hash_file(mut file: File) -> anyhow::Result<String> {
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hash.finalize().to_hex().to_string())
}
fn running_digest() -> anyhow::Result<String> {
    static DIGEST: OnceLock<String> = OnceLock::new();
    if let Some(digest) = DIGEST.get() {
        return Ok(digest.clone());
    }
    #[cfg(target_os = "linux")]
    let file = File::open("/proc/self/exe")?;
    #[cfg(not(target_os = "linux"))]
    let file = File::open(std::env::current_exe()?)?;
    #[cfg(target_os = "macos")]
    let file = {
        let mut file = file;
        super::host_image::attest(&mut file)?;
        file
    };
    let digest = hash_file(file)?;
    let _ = DIGEST.set(digest.clone());
    Ok(digest)
}
fn current_executable() -> anyhow::Result<Executable> {
    let path = fs::canonicalize(std::env::current_exe()?)?;
    let m = fs::symlink_metadata(&path)?;
    Ok(Executable {
        path,
        device: m.dev(),
        inode: m.ino(),
        digest: running_digest()?,
    })
}
fn validate_executable(executable: &Executable) -> anyhow::Result<()> {
    for parent in executable.path.ancestors().skip(1) {
        let m = fs::symlink_metadata(parent)?;
        #[cfg(target_os = "linux")]
        let sticky_bit = libc::S_ISVTX;
        // Darwin's mode_t is u16, while MetadataExt::mode() returns u32.
        #[cfg(not(target_os = "linux"))]
        let sticky_bit = libc::S_ISVTX as u32;
        let sticky_root = m.uid() == 0 && m.mode() & sticky_bit != 0;
        ensure!(
            m.is_dir() && [0, uid()].contains(&m.uid()) && (m.mode() & 0o022 == 0 || sticky_root),
            "unsafe worker executable ancestor {}",
            parent.display()
        );
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&executable.path)?;
    let m = file.metadata()?;
    ensure!(
        executable.path.is_absolute()
            && fs::canonicalize(&executable.path)? == executable.path
            && m.is_file()
            && [0, uid()].contains(&m.uid())
            && m.mode() & 0o022 == 0
            && m.mode() & 0o111 != 0
            && m.dev() == executable.device
            && m.ino() == executable.inode,
        "worker executable ownership/identity mismatch"
    );
    ensure!(
        hash_file(file)? == executable.digest,
        "worker executable content changed, including an in-place build"
    );
    Ok(())
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    version: u32,
    schema: String,
    package: String,
    digest: String,
}
fn hello() -> anyhow::Result<Hello> {
    Ok(Hello {
        version: AGENTCTL_HOST_VERSION,
        schema: SCHEMA.into(),
        package: env!("CARGO_PKG_VERSION").into(),
        digest: running_digest()?,
    })
}
fn compatible(hello: &Hello, digest: &str) -> Result<(), AgentCtlHostError> {
    if hello.version != AGENTCTL_HOST_VERSION
        || hello.schema != SCHEMA
        || hello.package != env!("CARGO_PKG_VERSION")
        || hello.digest != digest
    {
        return Err(AgentCtlHostError::new(
            AgentCtlHostErrorCode::VersionMismatch,
            "incompatible Job service executable/package/schema; drain and restart its listener; no command or descriptors admitted",
        ));
    }
    Ok(())
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Completion {
    exit_code: i32,
    cancelled: bool,
    signal: Option<i32>,
}
type Request = AgentCtlHostRequest<ContextData>;
type Response = AgentCtlHostResponse<Completion>;
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "control", rename_all = "snake_case", deny_unknown_fields)]
enum ClientControl {
    Started {
        request_id: String,
        pid: i32,
    },
    Cancel {
        version: u32,
        request_id: String,
        signal: i32,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum ServerEvent {
    Ticket { request_id: String },
    Complete { response: Response },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "instruction", rename_all = "snake_case", deny_unknown_fields)]
enum WorkerInstruction {
    Admit,
    Abort,
    Finish,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum WorkerEvent {
    Ready { pid: i32 },
    Cancelled { signal: i32 },
    Finalizing,
    Complete { response: Response },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bootstrap {
    directory: PathBuf,
    capability: String,
    worker: bool,
}
fn uid() -> u32 {
    unsafe { libc::geteuid() }
}
fn directory() -> anyhow::Result<PathBuf> {
    pvisor::host_transport::host_authority_root()
}
fn private_dir(path: &Path) -> anyhow::Result<()> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir()
            && !m.file_type().is_symlink()
            && m.uid() == uid()
            && m.mode() & 0o7777 == 0o700
            && fs::canonicalize(path)? == path,
        "host service directory must be same-UID, non-symlink and 0700"
    );
    Ok(())
}
fn load_generation(dir: &Path) -> anyhow::Result<Generation> {
    private_dir(dir)?;
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir.join("generation"))?;
    let m = f.metadata()?;
    ensure!(
        m.is_file()
            && m.uid() == uid()
            && m.mode() & 0o7777 == 0o600
            && m.nlink() == 1
            && m.len() <= 4096,
        "invalid private host generation file"
    );
    let g: Generation = serde_json::from_reader(f)?;
    ensure!(
        uuid::Uuid::parse_str(&g.generation).is_ok()
            && uuid::Uuid::parse_str(&g.capability).is_ok(),
        "invalid host generation"
    );
    Ok(g)
}
fn socket(dir: &Path, g: &Generation) -> PathBuf {
    dir.join(format!("{}.sock", g.generation))
}
fn same_uid(stream: &UnixStream) -> anyhow::Result<()> {
    // The shared Tokio helper uses the same kernel credentials; this explicit
    // synchronous variant avoids adopting/cloning the FD into a Tokio runtime.
    #[cfg(target_os = "linux")]
    {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
        ensure!(
            unsafe {
                libc::getsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    &mut cred as *mut _ as *mut _,
                    &mut len,
                )
            } == 0
                && cred.uid == uid(),
            "unauthorized host peer"
        );
    }
    #[cfg(target_os = "macos")]
    {
        let (mut peer, mut group) = (0, 0);
        ensure!(
            unsafe { libc::getpeereid(stream.as_raw_fd(), &mut peer, &mut group) } == 0
                && peer == uid(),
            "unauthorized host peer"
        );
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    anyhow::bail!("host peer authentication is unsupported");
    Ok(())
}
#[cfg(target_os = "macos")]
fn peer_pid(stream: &UnixStream) -> anyhow::Result<i32> {
    let mut pid = 0i32;
    let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
    ensure!(
        unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                (&mut pid as *mut i32).cast(),
                &mut len,
            )
        } == 0
            && len as usize == std::mem::size_of_val(&pid),
        "query frontend PID"
    );
    Ok(pid)
}

#[cfg(target_os = "linux")]
fn peer_pid(stream: &UnixStream) -> anyhow::Result<i32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
    ensure!(
        unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut _ as *mut _,
                &mut len,
            )
        } == 0,
        "query frontend PID"
    );
    Ok(cred.pid)
}

fn connect(dir: &Path, g: &Generation) -> anyhow::Result<UnixStream> {
    private_dir(dir)?;
    let path = socket(dir, g);
    let m = fs::symlink_metadata(&path)?;
    ensure!(
        m.file_type().is_socket() && m.uid() == uid() && m.mode() & 0o7777 == 0o600,
        "invalid host socket"
    );
    let stream = UnixStream::connect(&path)?;
    same_uid(&stream)?;
    let after = fs::symlink_metadata(path)?;
    ensure!(
        m.dev() == after.dev() && m.ino() == after.ino(),
        "host socket generation changed during connect"
    );
    Ok(stream)
}

fn readable(stream: &UnixStream) -> bool {
    let mut p = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe {
        libc::poll(&mut p, 1, 0);
    }
    p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0
}
fn spawn_internal(
    channel: &UnixStream,
    stdio: Option<Vec<OwnedFd>>,
    path: &Path,
) -> anyhow::Result<std::process::Child> {
    let _exec_guard = host_fds::exec_guard();
    let raw = unsafe { libc::fcntl(channel.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    ensure!(raw >= 0, "duplicate private worker channel");
    let inherited = unsafe { OwnedFd::from_raw_fd(raw) };
    let worker = stdio.is_some();
    let mut command = Command::new(path);
    command.arg(INTERNAL).env_clear();
    if let Some(mut fds) = stdio {
        command
            .stderr(Stdio::from(fds.remove(2)))
            .stdout(Stdio::from(fds.remove(1)))
            .stdin(Stdio::from(fds.remove(0)));
        // Keep the originating session/controlling terminal. Only isolate the
        // worker process group; executors may hand foreground to their children.
        command.process_group(0);
    } else {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    }
    unsafe {
        command.pre_exec(move || {
            if !worker && libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if worker {
                let mut mask: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut mask);
                for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                    libc::sigaddset(&mut mask, sig);
                }
                let error = libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
                if error != 0 {
                    return Err(std::io::Error::from_raw_os_error(error));
                }
            }
            if libc::dup2(raw, INTERNAL_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    drop(inherited);
    Ok(child)
}
fn start_or_connect() -> anyhow::Result<(UnixStream, Generation)> {
    let dir = directory()?;
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    private_dir(&dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir.join("start.lock"))?;
    let m = lock.metadata()?;
    ensure!(
        m.is_file() && m.uid() == uid() && m.mode() & 0o7777 == 0o600 && m.nlink() == 1,
        "invalid startup lock"
    );
    lock.lock_exclusive()?;
    if dir.join("generation").exists() {
        let old = load_generation(&dir)?;
        match connect(&dir, &old) {
            Ok(stream) => return Ok((stream, old)),
            Err(e) => {
                ensure!(
                    e.downcast_ref::<std::io::Error>().is_some_and(|e| matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    )),
                    "unsafe host restart refused: {e:#}"
                );
                let path = socket(&dir, &old);
                if path.exists() {
                    fs::remove_file(path)?;
                }
            }
        }
    }
    let mut g = Generation {
        generation: uuid::Uuid::new_v4().to_string(),
        capability: uuid::Uuid::new_v4().to_string(),
        schema: Some(SCHEMA.into()),
        digest: Some(running_digest()?),
        pid: None,
    };
    // Publish capability before exec; the child cannot dispatch without it.
    publish_generation(&dir, &g)?;
    let (mut parent, child_channel) = UnixStream::pair()?;
    let mut child = spawn_internal(&child_channel, None, &std::env::current_exe()?)?;
    drop(child_channel);
    g.pid = Some(child.id());
    publish_generation(&dir, &g)?;
    parent.set_read_timeout(Some(Duration::from_secs(10)))?;
    write_frame(
        &mut parent,
        &Bootstrap {
            directory: dir.clone(),
            capability: g.capability.clone(),
            worker: false,
        },
    )?;
    let ready: bool = read_frame(&mut parent).context("host service failed to start")?;
    ensure!(ready, "host startup rejected");
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok((connect(&dir, &g)?, g))
}
fn publish_generation(dir: &Path, g: &Generation) -> anyhow::Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer(&mut file, g)?;
    file.as_file().sync_all()?;
    file.persist(dir.join("generation"))?;
    File::open(dir)?.sync_all()?;
    Ok(())
}
fn signals(rt: &tokio::runtime::Runtime) -> anyhow::Result<Arc<AtomicI32>> {
    let _enter = rt.enter();
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let latch = Arc::new(AtomicI32::new(0));
    let caught = latch.clone();
    rt.spawn(async move {
        let sig = tokio::select! { _ = interrupt.recv() => libc::SIGINT, _ = terminate.recv() => libc::SIGTERM, _ = hangup.recv() => libc::SIGHUP };
        caught.store(sig, Ordering::SeqCst);
        notify_cancel(sig);
    });
    Ok(latch)
}
pub(crate) fn internal_if_requested() -> anyhow::Result<bool> {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new(INTERNAL)) {
        return Ok(false);
    }
    ensure!(
        std::env::args_os().count() == 2 && unsafe { libc::fcntl(INTERNAL_FD, libc::F_GETFD) } >= 0,
        "internal host mode requires a private inherited capability channel"
    );
    let mut channel = unsafe { UnixStream::from_raw_fd(INTERNAL_FD) };
    same_uid(&channel)?;
    channel.set_read_timeout(Some(Duration::from_secs(10)))?;
    let bootstrap: Bootstrap = read_frame(&mut channel).context("read internal bootstrap")?;
    ensure!(
        bootstrap.directory == directory()?,
        "capability directory mismatch"
    );
    let g = load_generation(&bootstrap.directory)?;
    ensure!(
        g.capability == bootstrap.capability,
        "invalid private host capability"
    );
    ensure!(
        unsafe { libc::fcntl(INTERNAL_FD, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
        "close bootstrap on exec"
    );
    if bootstrap.worker {
        run_worker(channel, bootstrap.directory, g)?;
    } else {
        ensure!(
            g.digest.as_deref() == Some(&running_digest()?) && g.schema.as_deref() == Some(SCHEMA),
            "service executable changed before startup"
        );
        let listener = UnixListener::bind(socket(&bootstrap.directory, &g))?;
        fs::set_permissions(
            socket(&bootstrap.directory, &g),
            fs::Permissions::from_mode(0o600),
        )?;
        write_frame(&mut channel, &true)?;
        drop(channel);
        serve(listener, bootstrap.directory, g)?;
    }
    Ok(true)
}
fn error(request_id: String, code: AgentCtlHostErrorCode, message: String) -> Response {
    Response {
        version: AGENTCTL_HOST_VERSION,
        request_id,
        result: Err(AgentCtlHostError {
            code,
            message: message.chars().take(2048).collect(),
        }),
    }
}
fn code(error: &anyhow::Error) -> AgentCtlHostErrorCode {
    error
        .downcast_ref::<AgentCtlHostError>()
        .map_or(AgentCtlHostErrorCode::Internal, |e| e.code)
}
fn validate(request: &Request, g: &Generation) -> anyhow::Result<()> {
    request.validate()?;
    ensure!(
        request.command.generation == g.generation,
        "host generation mismatch"
    );
    ensure!(
        request.command.executable.digest == g.digest.as_deref().unwrap_or(""),
        "worker binary does not match negotiated listener"
    );
    validate_executable(&request.command.executable)?;
    ensure!(
        request.command.cwd.is_absolute(),
        "host cwd must be absolute"
    );
    for (key, value) in &request.command.environment {
        ensure!(
            !key.is_empty() && !key.contains(&0) && !key.contains(&b'=') && !value.contains(&0),
            "invalid environment entry"
        );
    }
    request
        .command
        .command
        .validate_target(request.target.as_ref(), &request.command.cwd)?;
    Ok(())
}
fn unblock_worker_signals() -> anyhow::Result<()> {
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaddset(&mut mask, sig);
        }
        ensure!(
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &mask, std::ptr::null_mut()) == 0,
            "unblock worker signals"
        );
    }
    Ok(())
}

fn run_worker(mut channel: UnixStream, dir: PathBuf, g: Generation) -> anyhow::Result<()> {
    let request: Request = read_frame(&mut channel).context("read worker request")?;
    validate(&request, &g)?;
    ensure!(
        running_digest()? == request.command.executable.digest,
        "executed worker content differs from negotiated binary"
    );
    super::host_process::become_subreaper()?;
    WORKER_DIRECTORY
        .set(dir.clone())
        .map_err(|_| anyhow::anyhow!("duplicate worker context"))?;
    WORKER_TARGET
        .set(request.target.clone())
        .map_err(|_| anyhow::anyhow!("duplicate worker target"))?;
    let context = request.command;
    std::env::set_current_dir(&context.cwd)?;
    for (key, value) in context.environment {
        unsafe {
            std::env::set_var(
                std::ffi::OsString::from_vec(key),
                std::ffi::OsString::from_vec(value),
            );
        }
    }
    super::terminal::restore_service_context(context.terminal);
    pvisor::diagnostics::init_inherited();
    super::terminal::init_child_context();
    // pre_exec blocks cancellation until handlers exist. Tokio threads inherit
    // that mask too: release it on EVERY runtime thread, or workloads spawned
    // there inherit blocked SIGINT and terminal Ctrl-C never reaches them.
    let signal_gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let thread_gate = signal_gate.clone();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .on_thread_start(move || {
            let (lock, wake) = &*thread_gate;
            let mut ready = lock.lock().unwrap_or_else(|e| e.into_inner());
            while !*ready {
                ready = wake.wait(ready).unwrap_or_else(|e| e.into_inner());
            }
            unblock_worker_signals().expect("unblock runtime worker signals");
        })
        .build()?;
    struct ReleaseSignals(Arc<(Mutex<bool>, std::sync::Condvar)>);
    impl Drop for ReleaseSignals {
        fn drop(&mut self) {
            let (lock, wake) = &*self.0;
            *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
            wake.notify_all();
        }
    }
    // Release before runtime Drop even if handler installation fails.
    let release = ReleaseSignals(signal_gate);
    let token = tokio_util::sync::CancellationToken::new();
    WORKER_CANCEL
        .set(token)
        .map_err(|_| anyhow::anyhow!("duplicate worker cancellation"))?;
    let events = Arc::new(Mutex::new(channel.try_clone()?));
    WORKER_EVENTS
        .set(events.clone())
        .map_err(|_| anyhow::anyhow!("duplicate worker event channel"))?;
    let latch = signals(&rt)?;
    // Signals were blocked by pre_exec. Unblock only after installing handlers
    // and latching cancellation, before readiness and service admission.
    unblock_worker_signals()?;
    drop(release);
    channel.set_write_timeout(Some(GRACE))?;
    let _cancel_endpoint = super::host_cancel::start(&rt, &dir)?;
    write_frame(
        &mut events.lock().unwrap_or_else(|e| e.into_inner()),
        &WorkerEvent::Ready {
            pid: std::process::id() as i32,
        },
    )?;
    WORKER_READY.store(true, Ordering::SeqCst);
    let pending_signal = latch.load(Ordering::SeqCst);
    if pending_signal != 0 {
        notify_cancel(pending_signal);
    }
    let admit: WorkerInstruction = read_frame(&mut channel).context("read worker admission")?;
    let result = if matches!(admit, WorkerInstruction::Admit) {
        (|| {
            check_cancelled()?;
            context
                .command
                .validate_target(request.target.as_ref(), &context.cwd)?;
            if std::env::var("PVISOR_STARTUP_TIMING").as_deref() != Ok("0") {
                pvisor::diagnostics::diagnostic(format_args!(
                    "pvisor-host-request level=info version={} request_id={} frontend_pid={} worker_pid={}",
                    AGENTCTL_HOST_VERSION,
                    serde_json::to_string(&request.request_id)?,
                    context.client_pid,
                    std::process::id()
                ));
            }
            super::host::execute(&rt, context.command)
        })()
    } else {
        Ok(128 + libc::SIGTERM)
    };
    let sig = latch.load(Ordering::SeqCst);
    let response = match result {
        _ if sig != 0 => Response {
            version: AGENTCTL_HOST_VERSION,
            request_id: request.request_id,
            result: Ok(Completion {
                exit_code: 128 + sig,
                cancelled: true,
                signal: Some(sig),
            }),
        },
        Ok(exit_code) => Response {
            version: AGENTCTL_HOST_VERSION,
            request_id: request.request_id,
            result: Ok(Completion {
                exit_code: if sig != 0 { 128 + sig } else { exit_code },
                cancelled: sig != 0 || !matches!(admit, WorkerInstruction::Admit),
                signal: (sig != 0).then_some(sig),
            }),
        },
        Err(e) => error(request.request_id, code(&e), format!("{e:#}")),
    };
    write_frame(
        &mut events.lock().unwrap_or_else(|e| e.into_inner()),
        &WorkerEvent::Complete { response },
    )?;
    // Keep the subreaper root alive until the service has quiesced/killed its
    // descendants. This also covers a frontend dying during completion.
    let _: WorkerInstruction = read_frame(&mut channel).context("read worker finalization")?;
    Ok(())
}
struct Admission(Arc<AtomicUsize>);
impl Drop for Admission {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn serve(listener: UnixListener, dir: PathBuf, g: Generation) -> anyhow::Result<()> {
    let active = Arc::new(AtomicUsize::new(0));
    let g = Arc::new(g);
    for connection in listener.incoming() {
        let mut client = connection?;
        if same_uid(&client).is_err() {
            continue;
        }
        // Bound unauthenticated/pre-admission handshake threads too, not only
        // workers. A stalled same-UID client cannot allocate unbounded threads.
        if active.fetch_add(1, Ordering::SeqCst) >= 64 {
            active.fetch_sub(1, Ordering::SeqCst);
            client.set_write_timeout(Some(Duration::from_millis(100)))?;
            let unavailable: Result<(), AgentCtlHostError> = Err(AgentCtlHostError::new(
                AgentCtlHostErrorCode::Unavailable,
                "host connection admission exhausted; no descriptors or command admitted",
            ));
            let _ = write_frame(&mut client, &unavailable);
            continue;
        }
        let admission = Admission(active.clone());
        let dir = dir.clone();
        let g = g.clone();
        std::thread::spawn(move || {
            let _admission = admission;
            let mut correlation = None;
            let result = (|| -> anyhow::Result<()> {
                client.set_read_timeout(Some(Duration::from_secs(10)))?;
                client.set_write_timeout(Some(Duration::from_secs(10)))?;
                let h: Hello = read_frame(&mut client)?;
                let negotiated = compatible(&h, g.digest.as_deref().unwrap_or(""));
                write_frame(&mut client, &negotiated)?;
                negotiated?;
                // No descriptor or typed mutation has been submitted before
                // exact executable/package/schema agreement.
                let fds = host_fds::receive(&client, 3)?;
                let raw: serde_json::Value = read_frame(&mut client)?;
                let request_id = raw
                    .get("request_id")
                    .and_then(|id| id.as_str())
                    .unwrap_or("")
                    .to_owned();
                let request: Request = match serde_json::from_value(raw) {
                    Ok(request) => request,
                    Err(e) => {
                        return write_frame(
                            &mut client,
                            &ServerEvent::Complete {
                                response: error(
                                    request_id,
                                    AgentCtlHostErrorCode::InvalidRequest,
                                    e.to_string(),
                                ),
                            },
                        );
                    }
                };
                let request_id = request.request_id.clone();
                correlation = Some(request_id.clone());
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                if request.command.client_pid as i32 != peer_pid(&client)? {
                    return write_frame(
                        &mut client,
                        &ServerEvent::Complete {
                            response: error(
                                request_id,
                                AgentCtlHostErrorCode::Unauthorized,
                                "frontend PID must match kernel peer credentials".into(),
                            ),
                        },
                    );
                }
                if let Err(e) = validate(&request, &g) {
                    let kind = e
                        .downcast_ref::<AgentCtlHostError>()
                        .map_or(AgentCtlHostErrorCode::InvalidRequest, |e| e.code);
                    return write_frame(
                        &mut client,
                        &ServerEvent::Complete {
                            response: error(request_id, kind, e.to_string()),
                        },
                    );
                }

                #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                return write_frame(
                    &mut client,
                    &ServerEvent::Complete {
                        response: error(
                            request_id,
                            AgentCtlHostErrorCode::Unsupported,
                            "host worker ownership is unsupported on this platform".into(),
                        ),
                    },
                );
                let (mut authority, worker) = UnixStream::pair()?;
                write_frame(
                    &mut client,
                    &ServerEvent::Ticket {
                        request_id: request_id.clone(),
                    },
                )?;
                host_fds::send(
                    &client,
                    &[
                        fds[0].as_raw_fd(),
                        fds[1].as_raw_fd(),
                        fds[2].as_raw_fd(),
                        worker.as_raw_fd(),
                    ],
                )?;
                drop(fds);
                authority.set_read_timeout(Some(Duration::from_secs(10)))?;
                write_frame(
                    &mut authority,
                    &Bootstrap {
                        directory: dir.clone(),
                        capability: g.capability.clone(),
                        worker: true,
                    },
                )?;
                write_frame(&mut authority, &request)?;
                // Registration happens before admission; the frontend already
                // owns a Child, foreground guard, and subreaper at this point.
                let mut cancelled = None;
                let pid = loop {
                    match read_frame::<ClientControl>(&mut client)
                        .context("read frontend worker registration")?
                    {
                        ClientControl::Started {
                            request_id: id,
                            pid,
                        } if id == request_id => break pid,
                        ClientControl::Cancel {
                            version,
                            request_id: id,
                            signal,
                        } if version == AGENTCTL_HOST_VERSION
                            && id == request_id
                            && [libc::SIGINT, libc::SIGTERM, libc::SIGHUP].contains(&signal) =>
                        {
                            cancelled = Some(signal)
                        }
                        _ => anyhow::bail!("invalid worker registration"),
                    }
                };
                let mut tree = OwnedTree::new(pid, request.command.client_pid as i32, false, 0)?;
                let ready: WorkerEvent =
                    read_frame(&mut authority).context("read worker readiness")?;
                ensure!(
                    matches!(ready, WorkerEvent::Ready { pid: ready } if ready == pid),
                    "worker identity mismatch"
                );
                // Keep the original socket until bootstrap receipt is acknowledged:
                // Darwin can disconnect it while SCM_RIGHTS is still in flight.
                drop(worker);
                // Retain filesystem cleanup ownership independently of worker
                // destructors, which do not run after escalation to SIGKILL.
                let _cancel_endpoint = super::host_cancel::own_endpoint(&dir, pid as u32)?;
                if readable(&client) {
                    cancelled = Some(read_cancel(&mut client, &request_id));
                }
                write_frame(
                    &mut authority,
                    &if cancelled.is_some() {
                        WorkerInstruction::Abort
                    } else {
                        WorkerInstruction::Admit
                    },
                )?;
                authority.set_read_timeout(None)?;
                let mut reader = authority.try_clone()?;
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    loop {
                        let event = read_frame::<WorkerEvent>(&mut reader);
                        let done = !matches!(
                            event,
                            Ok(WorkerEvent::Cancelled { .. } | WorkerEvent::Finalizing)
                        );
                        if tx.send(event).is_err() || done {
                            break;
                        }
                    }
                });
                let mut cancel_deadline = cancelled.map(|_| Instant::now() + GRACE);
                if let Some(sig) = cancelled {
                    tree.signal_root(sig)?;
                }
                let mut response = loop {
                    tree.refresh()?;
                    if let Ok(result) = rx.try_recv() {
                        break match result {
                            Ok(WorkerEvent::Complete { response }) => response,
                            Ok(WorkerEvent::Finalizing) => {
                                cancel_deadline.get_or_insert(Instant::now() + GRACE);
                                continue;
                            }
                            Ok(WorkerEvent::Cancelled { signal }) => {
                                if cancelled.is_none() {
                                    cancelled = Some(signal);
                                    cancel_deadline = Some(Instant::now() + GRACE);
                                }
                                continue;
                            }
                            Ok(_) => error(
                                request_id.clone(),
                                AgentCtlHostErrorCode::Internal,
                                "unexpected worker event".into(),
                            ),
                            Err(_) if cancelled.is_some() => Response {
                                version: AGENTCTL_HOST_VERSION,
                                request_id: request_id.clone(),
                                result: Ok(Completion {
                                    exit_code: 128 + cancelled.unwrap(),
                                    cancelled: true,
                                    signal: cancelled,
                                }),
                            },
                            Err(e) => error(
                                request_id.clone(),
                                AgentCtlHostErrorCode::Unavailable,
                                format!(
                                    "worker response lost: {e}; effects may be ambiguous; not retried"
                                ),
                            ),
                        };
                    }
                    if cancelled.is_none() && readable(&client) {
                        let sig = read_cancel(&mut client, &request_id);
                        cancelled = Some(sig);
                        tree.signal_root(sig)?;
                        cancel_deadline = Some(Instant::now() + GRACE);
                    }
                    if cancel_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        tree.cleanup(true)?;
                        break if let Some(signal) = cancelled {
                            Response {
                                version: AGENTCTL_HOST_VERSION,
                                request_id: request_id.clone(),
                                result: Ok(Completion {
                                    exit_code: 128 + signal,
                                    cancelled: true,
                                    signal: Some(signal),
                                }),
                            }
                        } else {
                            error(request_id.clone(), AgentCtlHostErrorCode::Unavailable,
                                "request teardown exceeded its cleanup deadline; effects may be ambiguous; not retried".into())
                        };
                    }
                    std::thread::sleep(Duration::from_millis(10));
                };
                tree.cleanup(false)?;
                let _ = write_frame(&mut authority, &WorkerInstruction::Finish);
                let deadline = Instant::now() + GRACE;
                while !tree.root_exited() && Instant::now() < deadline {
                    tree.refresh()?;
                    std::thread::sleep(Duration::from_millis(10));
                }
                tree.cleanup(true)?;
                if let Some(sig) = cancelled {
                    response.result = Ok(Completion {
                        exit_code: 128 + sig,
                        cancelled: true,
                        signal: Some(sig),
                    });
                }
                write_frame(&mut client, &ServerEvent::Complete { response })
            })();
            if let Err(failure) = result {
                tracing::warn!("host request failed: {failure:#}");
                if let Some(request_id) = correlation {
                    let _ = write_frame(
                        &mut client,
                        &ServerEvent::Complete {
                            response: error(
                                request_id,
                                code(&failure),
                                format!("{failure:#}; effects may be ambiguous; not retried"),
                            ),
                        },
                    );
                }
            }
        });
    }
    Ok(())
}
fn read_cancel(client: &mut UnixStream, request_id: &str) -> i32 {
    match read_frame::<ClientControl>(client) {
        Ok(ClientControl::Cancel {
            version,
            request_id: id,
            signal,
        }) if version == AGENTCTL_HOST_VERSION
            && id == request_id
            && [libc::SIGINT, libc::SIGTERM, libc::SIGHUP].contains(&signal) =>
        {
            signal
        }
        _ => libc::SIGTERM, // EOF/malformed continuation cannot detach ownership.
    }
}
struct FrontWorker {
    child: Option<std::process::Child>,
    tree: OwnedTree,
    _terminal: Option<TerminalOwner>,
}
impl FrontWorker {
    fn finish(&mut self) -> anyhow::Result<()> {
        self.tree.cleanup(true)?;
        if let Some(mut child) = self.child.take() {
            child.wait()?;
        }
        Ok(())
    }
}
impl Drop for FrontWorker {
    fn drop(&mut self) {
        let cleanup = self.tree.cleanup(true);
        if let Some(mut child) = self.child.take() {
            if cleanup.is_ok() && self.tree.root_exited() {
                let _ = child.wait();
            } else {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
        }
    }
}
pub(crate) fn call(mut command: JobCommand) -> anyhow::Result<i32> {
    // Install and latch signals before autostart, any FD transfer, and admission.
    let rt = tokio::runtime::Runtime::new()?;
    let signal = signals(&rt)?;
    let target = command.pin_target()?;
    let interactive = command.inherits_terminal_input()?;
    if interactive {
        // Reject background interactive launches before service admission/mutation.
        TerminalOwner::check_interactive()?;
    }
    super::host_process::become_subreaper()?;
    let (mut stream, g) = start_or_connect()?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    if g.schema.as_deref() != Some(SCHEMA) {
        anyhow::bail!(
            "legacy Job service listener {:?} must be drained and restarted; no descriptors/command submitted",
            g.pid
        );
    }
    write_frame(&mut stream, &hello()?)?;
    let agreement: Result<(), AgentCtlHostError> = read_frame(&mut stream)
        .context("incompatible listener; no descriptors/command submitted")?;
    agreement?;
    if signal.load(Ordering::SeqCst) != 0 {
        return Ok(128 + signal.load(Ordering::SeqCst));
    }
    let request_id = uuid::Uuid::new_v4().to_string();
    let executable = current_executable()?;
    validate_executable(&executable)?;
    let request = Request {
        version: AGENTCTL_HOST_VERSION,
        request_id: request_id.clone(),
        target,
        command: ContextData {
            client_pid: std::process::id(),
            executable,
            cwd: std::env::current_dir()?,
            terminal: super::terminal::service_context(),
            environment: std::env::vars_os()
                .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
                .collect(),
            generation: g.generation,
            command,
        },
    };
    request.validate()?;
    let _ = encoded(&request)?;
    if signal.load(Ordering::SeqCst) != 0 {
        return Ok(128 + signal.load(Ordering::SeqCst));
    }
    host_fds::send(&stream, &[0, 1, 2])?;
    write_frame(&mut stream, &request)
        .context("request send failed; effects may be ambiguous; not retried")?;
    stream.set_read_timeout(None)?;
    let mut sender = stream.try_clone()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        loop {
            let event = read_frame::<ServerEvent>(&mut stream);
            let event = event.and_then(|event| {
                let fds = if matches!(event, ServerEvent::Ticket { .. }) {
                    host_fds::receive(&stream, 4)?
                } else {
                    Vec::new()
                };
                Ok((event, fds))
            });
            let final_event = !matches!(&event, Ok((ServerEvent::Ticket { .. }, _)));
            if tx.send(event).is_err() || final_event {
                break;
            }
        }
    });
    let mut worker: Option<FrontWorker> = None;
    let mut cancelled = None;
    let mut cancel_deadline = None;
    loop {
        let sig = signal.load(Ordering::SeqCst);
        if sig != 0 && cancelled.is_none() {
            cancelled = Some(sig);
            cancel_deadline = Some(Instant::now() + GRACE * 3);
            write_frame(
                &mut sender,
                &ClientControl::Cancel {
                    version: AGENTCTL_HOST_VERSION,
                    request_id: request_id.clone(),
                    signal: sig,
                },
            )?;
            if let Some(worker) = &worker {
                worker.tree.signal_root(sig)?;
            }
        }
        if let Some(worker) = &mut worker {
            worker.tree.refresh()?;
        }
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(event) => {
                match event.context("host response lost; effect may be ambiguous; not retried")? {
                    (ServerEvent::Ticket { request_id: id }, mut fds) => {
                        ensure!(
                            id == request_id && worker.is_none(),
                            "invalid worker ticket identity"
                        );
                        let channel = UnixStream::from(fds.remove(3));
                        validate_executable(&request.command.executable)?;
                        let child =
                            spawn_internal(&channel, Some(fds), &request.command.executable.path)?;
                        drop(channel);
                        let pid = child.id() as i32;
                        let tree = OwnedTree::new(
                            pid,
                            std::process::id() as i32,
                            true,
                            g.pid.unwrap_or(0) as i32,
                        )?;
                        let mut owned = FrontWorker {
                            child: Some(child),
                            tree,
                            _terminal: None,
                        };
                        if interactive {
                            owned._terminal = TerminalOwner::give_to(pid)?;
                        }
                        write_frame(
                            &mut sender,
                            &ClientControl::Started {
                                request_id: request_id.clone(),
                                pid,
                            },
                        )?;
                        if let Some(sig) = cancelled {
                            owned.tree.signal_root(sig)?;
                        }
                        worker = Some(owned);
                    }
                    (ServerEvent::Complete { response }, _) => {
                        response.validate(&request_id)?;
                        if let Some(mut owned) = worker.take() {
                            owned.finish()?;
                        }
                        return match response.result {
                            Ok(completion) => {
                                if completion.cancelled {
                                    eprintln!(
                                        "pVisor host request cancelled; effects may already have occurred; not retried"
                                    );
                                }
                                Ok(completion.exit_code)
                            }
                            Err(e) => Err(e.into()),
                        };
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(e) => {
                return Err(e)
                    .context("host response channel lost; effects may be ambiguous; not retried");
            }
        }
        if cancel_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            drop(worker);
            anyhow::bail!(
                "host cancellation cleanup exceeded deadline; effects may be ambiguous; not retried"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pvisor_core::host_protocol::AGENTCTL_HOST_MAX_FRAME_BYTES;
    #[cfg(target_os = "linux")]
    #[test]
    fn terminal_signal_is_reported_before_hung_finalization() {
        const NAME: &str = "terminal_signal_is_reported_before_hung_finalization";
        if std::env::var("PVISOR_TERMINAL_HOOK_TEST").as_deref() != Ok(NAME) {
            let module = module_path!().split_once("::").unwrap().1;
            let status = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &format!("{module}::{NAME}"), "--test-threads=1"])
                .env("PVISOR_TERMINAL_HOOK_TEST", NAME)
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        struct HangingTerminalSink(pvisor::trace::Journal);
        #[async_trait::async_trait]
        impl pvisor::EventSink for HangingTerminalSink {
            async fn append(
                &self,
                event: &pvisor_core::event::Event,
            ) -> anyhow::Result<pvisor_core::event::Receipt> {
                if event.name() == "run.failed" {
                    std::future::pending::<()>().await;
                }
                Ok(self.0.append_async(event.clone()).await?)
            }
        }
        let (events, mut receiver) = UnixStream::pair().unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        WORKER_EVENTS.set(Arc::new(Mutex::new(events))).unwrap();
        WORKER_READY.store(true, Ordering::SeqCst);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let runtime = pvisor::PVisor::builder()
                .event_sink(Arc::new(HangingTerminalSink(
                    pvisor::trace::Journal::memory(),
                )))
                .executors(vec![super::super::run::report_terminal(Arc::new(
                    pvisor::ProcessExecutor::default(),
                ))])
                .build();
            let mut spec = pvisor_core::RunSpec::process("terminal-hook", "sh", "/bin/sh");
            let pvisor_core::RunInvocation::Process(process) = &mut spec.invocation;
            process.args = vec!["-c".into(), "kill -INT $$".into()];
            let handle = runtime.run(spec).await.unwrap();
            assert!(matches!(
                read_frame::<WorkerEvent>(&mut receiver).unwrap(),
                WorkerEvent::Cancelled {
                    signal: libc::SIGINT
                }
            ));
            assert!(
                tokio::time::timeout(Duration::from_millis(100), handle.wait())
                    .await
                    .is_err(),
                "test finalization must remain blocked after the signal event"
            );
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ordinary_exit_codes_do_not_arm_signal_cancellation() {
        const NAME: &str = "ordinary_exit_codes_do_not_arm_signal_cancellation";
        if std::env::var("PVISOR_TERMINAL_HOOK_TEST").as_deref() != Ok(NAME) {
            let module = module_path!().split_once("::").unwrap().1;
            assert!(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", &format!("{module}::{NAME}"), "--test-threads=1"])
                    .env("PVISOR_TERMINAL_HOOK_TEST", NAME)
                    .status()
                    .unwrap()
                    .success()
            );
            return;
        }
        let (events, mut receiver) = UnixStream::pair().unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        WORKER_EVENTS.set(Arc::new(Mutex::new(events))).unwrap();
        let cancellation = tokio_util::sync::CancellationToken::new();
        WORKER_CANCEL.set(cancellation.clone()).unwrap();
        WORKER_READY.store(true, Ordering::SeqCst);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            for code in [129, 130, 143] {
                let runtime = pvisor::PVisor::builder().executors(vec![
                    super::super::run::report_terminal(Arc::new(pvisor::ProcessExecutor::default()))
                ]).build();
                let mut spec = pvisor_core::RunSpec::process(format!("exit-{code}"), "sh", "/bin/sh");
                let pvisor_core::RunInvocation::Process(process) = &mut spec.invocation;
                process.args = vec!["-c".into(), format!("exit {code}")];
                let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
                assert_eq!(result.exit_code, Some(code));
                assert_eq!(result.executor_observations.termination_signal, None);
                assert!(!cancellation.is_cancelled());
                let mut byte = [0];
                let error = receiver.read(&mut byte).unwrap_err();
                assert!(matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut), "unexpected worker event after exit {code}");
            }
            for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
                let runtime = pvisor::PVisor::builder().executors(vec![
                    super::super::run::report_terminal(Arc::new(pvisor::ProcessExecutor::default()))
                ]).build();
                let mut spec = pvisor_core::RunSpec::process(format!("signal-{signal}"), "sh", "/bin/sh");
                let pvisor_core::RunInvocation::Process(process) = &mut spec.invocation;
                process.args = vec!["-c".into(), format!("kill -{signal} $$")];
                let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
                assert_eq!(result.executor_observations.termination_signal, Some(signal));
                assert!(matches!(read_frame::<WorkerEvent>(&mut receiver).unwrap(), WorkerEvent::Cancelled { signal: actual } if actual == signal));
                assert!(cancellation.is_cancelled());
            }
        });
    }

    #[test]
    fn compatibility_is_exact_before_descriptor_admission() {
        let mut h = hello().unwrap();
        compatible(&h, &h.digest).unwrap();
        assert_eq!(
            compatible(&h, "in-place-changed-content").unwrap_err().code,
            AgentCtlHostErrorCode::VersionMismatch
        );
        h.schema = "pvisor-job-ticket-2".into();
        assert_eq!(
            compatible(&h, &h.digest).unwrap_err().code,
            AgentCtlHostErrorCode::VersionMismatch
        );
    }
    #[test]
    fn incompatible_listener_closes_before_waiting_for_rights_or_command() {
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(dir.path().join("test.sock")).unwrap();
        let g = Generation {
            generation: uuid::Uuid::new_v4().to_string(),
            capability: uuid::Uuid::new_v4().to_string(),
            schema: Some(SCHEMA.into()),
            digest: Some(running_digest().unwrap()),
            pid: Some(std::process::id()),
        };
        let directory = dir.path().to_owned();
        std::thread::spawn(move || {
            let _ = serve(listener, directory, g);
        });
        let mut client = UnixStream::connect(dir.path().join("test.sock")).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut h = hello().unwrap();
        h.digest = "same-inode-but-different-content".into();
        write_frame(&mut client, &h).unwrap();
        let agreement: Result<(), AgentCtlHostError> = read_frame(&mut client).unwrap();
        assert_eq!(
            agreement.unwrap_err().code,
            AgentCtlHostErrorCode::VersionMismatch
        );
        // Never send rights or a Job command. Refusal must not wait for either.
        let mut byte = [0];
        assert_eq!(client.read(&mut byte).unwrap(), 0);
    }

    #[test]
    fn core_identity_boundaries_and_correlation() {
        let mut request = AgentCtlHostRequest {
            version: 1,
            request_id: "x".repeat(256),
            target: None,
            command: (),
        };
        request.validate().unwrap();
        request.request_id.push('x');
        assert!(request.validate().is_err());
        request.request_id = "bad\nidentity".into();
        assert!(request.validate().is_err());
        let response = error(
            "x".repeat(256),
            AgentCtlHostErrorCode::Conflict,
            "stale".into(),
        );
        response.validate(&"x".repeat(256)).unwrap();
    }
    #[test]
    fn bounded_frames_and_private_paths() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        write_frame(&mut a, &42).unwrap();
        assert_eq!(read_frame::<u32>(&mut b).unwrap(), 42);
        assert!(encoded(&"x".repeat(AGENTCTL_HOST_MAX_FRAME_BYTES)).is_err());
        let temp = tempfile::tempdir().unwrap();
        let dir = fs::canonicalize(temp.path()).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        private_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_dir(&dir).is_err());
        std::os::unix::fs::symlink(&dir, dir.join("link")).unwrap();
        assert!(private_dir(&dir.join("link")).is_err());
    }
}
