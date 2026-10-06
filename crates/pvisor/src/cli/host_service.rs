//! Persistent host Job service and a separate synchronous FD-bearing framing.
//!
//! The ancillary marker precedes a bounded length-prefixed JSON frame. Workload
//! bytes travel only over inherited descriptors, never through JSON or a PTY.
use super::host::JobCommand;
use anyhow::{Context, ensure};
use fs2::FileExt;
use pvisor_core::host_protocol::{
    AGENTCTL_HOST_MAX_FRAME_BYTES, AGENTCTL_HOST_VERSION, AgentCtlHostError, AgentCtlHostErrorCode,
    AgentCtlHostRequest, AgentCtlHostResponse,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::os::{
    fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const INTERNAL: &str = "--pvisor-internal-host";
const INTERNAL_FD: RawFd = 3;
static WORKER_DIRECTORY: OnceLock<PathBuf> = OnceLock::new();

/// Reuse the existing VM lower-view exclusion for the entire host service
/// directory, not just the individual Attempt socket.
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
            "custom VM control socket must be directly in the private host service directory {} to keep the service guest-inaccessible",
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
struct Generation {
    generation: String,
    capability: String,
}
#[derive(Debug, Serialize, Deserialize)]
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
struct Executable {
    path: PathBuf,
    device: u64,
    inode: u64,
}
fn current_executable() -> anyhow::Result<Executable> {
    let path = fs::canonicalize(std::env::current_exe()?)?;
    let metadata = fs::symlink_metadata(&path)?;
    Ok(Executable {
        path,
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}
fn validate_executable(executable: &Executable) -> anyhow::Result<()> {
    // Ownership of the final inode alone is insufficient if another UID can
    // replace it through a writable ancestor between validation and self-exec.
    for parent in executable.path.ancestors().skip(1) {
        let m = fs::symlink_metadata(parent)?;
        let protected_shared_directory = m.uid() == 0 && m.mode() & libc::S_ISVTX as u32 != 0;
        ensure!(
            m.is_dir()
                && [0, uid()].contains(&m.uid())
                && (m.mode() & 0o022 == 0 || protected_shared_directory),
            "unsafe host worker executable ancestor {}",
            parent.display()
        );
    }
    let m = fs::symlink_metadata(&executable.path)?;
    ensure!(
        executable.path.is_absolute()
            && fs::canonicalize(&executable.path)? == executable.path
            && m.is_file()
            && [0, uid()].contains(&m.uid())
            && m.mode() & 0o022 == 0
            && m.mode() & 0o111 != 0
            && m.dev() == executable.device
            && m.ino() == executable.inode,
        "host worker executable identity/ownership mismatch"
    );
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
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
    Cancel {
        version: u32,
        request_id: String,
        signal: i32,
    },
}

struct Worker(std::process::Child);
impl Drop for Worker {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            // Even a transport error must leave a reaper, without killing a
            // mutation whose acceptance/effect may already be ambiguous.
            let pid = self.0.id() as i32;
            std::thread::spawn(move || {
                loop {
                    let result = unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) };
                    if result >= 0
                        || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                    {
                        break;
                    }
                }
            });
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Bootstrap {
    directory: PathBuf,
    capability: String,
    worker: bool,
}

fn uid() -> u32 {
    unsafe { libc::geteuid() }
}
fn directory() -> anyhow::Result<PathBuf> {
    // A fixed short, non-workspace path fits sockaddr_un on Linux and macOS.
    Ok(fs::canonicalize("/tmp")?.join(format!("pvisor-host-{}", uid())))
}
fn private_dir(path: &Path) -> anyhow::Result<()> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && !m.file_type().is_symlink() && m.uid() == uid() && m.mode() & 0o7777 == 0o700,
        "host service directory must be same-UID, non-symlink and 0700"
    );
    ensure!(
        fs::canonicalize(path)? == path,
        "host service path contains symlinks"
    );
    Ok(())
}
fn private_file(path: &Path) -> anyhow::Result<File> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let m = f.metadata()?;
    ensure!(
        m.is_file() && m.uid() == uid() && m.mode() & 0o7777 == 0o600 && m.nlink() == 1,
        "invalid private host service file"
    );
    Ok(f)
}
fn load_generation(dir: &Path) -> anyhow::Result<Generation> {
    private_dir(dir)?;
    let f = private_file(&dir.join("generation"))?;
    ensure!(f.metadata()?.len() <= 4096, "oversized host generation");
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
fn connect(dir: &Path, g: &Generation) -> anyhow::Result<UnixStream> {
    private_dir(dir)?;
    let path = socket(dir, g);
    let m = fs::symlink_metadata(&path)?;
    ensure!(
        m.file_type().is_socket() && m.uid() == uid() && m.mode() & 0o7777 == 0o600,
        "invalid host service socket"
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
fn same_uid(stream: &UnixStream) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
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
            "peer credential query failed"
        );
        ensure!(cred.uid == uid(), "unauthorized host peer");
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    anyhow::bail!("host peer authentication is unsupported on this platform");
    #[cfg(target_os = "macos")]
    {
        let (mut peer, mut group) = (0, 0);
        ensure!(
            unsafe { libc::getpeereid(stream.as_raw_fd(), &mut peer, &mut group) } == 0
                && peer == uid(),
            "unauthorized host peer"
        );
    }
    Ok(())
}
fn read_frame<T: DeserializeOwned>(stream: &mut UnixStream) -> anyhow::Result<T> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    ensure!(
        length > 0 && length <= AGENTCTL_HOST_MAX_FRAME_BYTES,
        "invalid host frame length"
    );
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
fn write_frame<T: Serialize>(stream: &mut UnixStream, value: &T) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() <= AGENTCTL_HOST_MAX_FRAME_BYTES,
        "host frame exceeds limit"
    );
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}

fn send_fds(stream: &UnixStream, fds: &[RawFd; 3]) -> anyhow::Result<()> {
    let mut marker = [0x46u8];
    let mut iov = libc::iovec {
        iov_base: marker.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 16];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = unsafe { libc::CMSG_SPACE(std::mem::size_of_val(fds) as u32) } as _;
    unsafe {
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as u32) as _;
        std::ptr::copy_nonoverlapping(
            fds.as_ptr().cast::<u8>(),
            libc::CMSG_DATA(c),
            std::mem::size_of_val(fds),
        );
        ensure!(
            libc::sendmsg(stream.as_raw_fd(), &msg, 0) == 1,
            "send host stdio descriptors: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}
fn receive_fds(stream: &UnixStream) -> anyhow::Result<Vec<OwnedFd>> {
    let mut marker = [0u8];
    let mut iov = libc::iovec {
        iov_base: marker.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 16];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = std::mem::size_of_val(&control) as _;
    let mut fds = Vec::new();
    unsafe {
        let n = libc::recvmsg(stream.as_raw_fd(), &mut msg, 0);
        ensure!(n == 1, "receive host stdio descriptors");
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let len = (*c).cmsg_len as usize;
                let header = libc::CMSG_LEN(0) as usize;
                ensure!(len >= header, "invalid ancillary header");
                for i in 0..(len - header) / std::mem::size_of::<RawFd>() {
                    let fd = std::ptr::read_unaligned(libc::CMSG_DATA(c).cast::<RawFd>().add(i));
                    let owned = OwnedFd::from_raw_fd(fd);
                    ensure!(
                        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) == 0,
                        "set stdio close-on-exec"
                    );
                    fds.push(owned);
                }
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
    }
    ensure!(
        msg.msg_flags & libc::MSG_CTRUNC == 0 && marker[0] == 0x46 && fds.len() == 3,
        "exactly three host stdio descriptors required"
    );
    Ok(fds)
}

fn spawn_internal(
    dir: &Path,
    g: &Generation,
    stdio: Option<Vec<OwnedFd>>,
    executable: Option<&Path>,
) -> anyhow::Result<(std::process::Child, UnixStream)> {
    let (parent, child) = UnixStream::pair()?;
    // Allocate above stdio and the reserved bootstrap descriptor to avoid dup2
    // collisions with Command's stdio preparation.
    let inherited = unsafe { libc::fcntl(child.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    ensure!(inherited >= 0, "duplicate internal descriptor");
    let inherited = unsafe { OwnedFd::from_raw_fd(inherited) };
    let raw = inherited.as_raw_fd();
    let worker = stdio.is_some();
    let executable = executable
        .map(Path::to_owned)
        .map(Ok)
        .unwrap_or_else(std::env::current_exe)?;
    let mut command = Command::new(executable);
    command.arg(INTERNAL).env_clear();
    if let Some(mut fds) = stdio {
        command
            .stderr(Stdio::from(fds.remove(2)))
            .stdout(Stdio::from(fds.remove(1)))
            .stdin(Stdio::from(fds.remove(0)));
    } else {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    }
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::dup2(raw, INTERNAL_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let process = command.spawn()?;
    drop(inherited);
    drop(child);
    let mut parent = parent;
    parent.set_read_timeout(Some(Duration::from_secs(10)))?;
    write_frame(
        &mut parent,
        &Bootstrap {
            directory: dir.to_owned(),
            capability: g.capability.clone(),
            worker,
        },
    )?;
    Ok((process, parent))
}
fn start_or_connect() -> anyhow::Result<(UnixStream, Generation)> {
    let dir = directory()?;
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    private_dir(&dir)?;
    let lock_path = dir.join("start.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&lock_path)?;
    let metadata = lock.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == uid()
            && metadata.mode() & 0o7777 == 0o600
            && metadata.nlink() == 1,
        "invalid host startup lock"
    );
    lock.lock_exclusive()?;
    if dir.join("generation").exists() {
        let old = load_generation(&dir)?;
        match connect(&dir, &old) {
            Ok(stream) => return Ok((stream, old)),
            Err(error) => {
                let recoverable = error.downcast_ref::<std::io::Error>().is_some_and(|e| {
                    matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    )
                });
                ensure!(recoverable, "refusing unsafe host restart: {error:#}");
                let path = socket(&dir, &old);
                if path.exists() {
                    fs::remove_file(path)?;
                }
            }
        }
    }
    let g = Generation {
        generation: uuid::Uuid::new_v4().to_string(),
        capability: uuid::Uuid::new_v4().to_string(),
    };
    let mut file = tempfile::NamedTempFile::new_in(&dir)?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer(&mut file, &g)?;
    file.as_file().sync_all()?;
    file.persist(dir.join("generation"))?;
    let (mut process, mut bootstrap) = spawn_internal(&dir, &g, None, None)?;
    let ready: bool = read_frame(&mut bootstrap).context("host service failed to start")?;
    ensure!(ready, "host service startup rejected");
    // Detached owner: reap if it unexpectedly exits, without tying its lifetime
    // to any Job or the autostarting CLI.
    std::thread::spawn(move || {
        let _ = process.wait();
    });
    Ok((connect(&dir, &g)?, g))
}

pub(crate) fn internal_if_requested() -> anyhow::Result<bool> {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new(INTERNAL)) {
        return Ok(false);
    }
    ensure!(
        std::env::args_os().count() == 2,
        "invalid internal host invocation"
    );
    ensure!(
        unsafe { libc::fcntl(INTERNAL_FD, libc::F_GETFD) } >= 0,
        "internal host mode requires a private inherited capability channel"
    );
    let mut channel = unsafe { UnixStream::from_raw_fd(INTERNAL_FD) };
    same_uid(&channel)?;
    channel.set_read_timeout(Some(Duration::from_secs(10)))?;
    let bootstrap: Bootstrap = read_frame(&mut channel)?;
    ensure!(
        bootstrap.directory == directory()?,
        "internal capability directory mismatch"
    );
    let g = load_generation(&bootstrap.directory)?;
    ensure!(
        g.capability == bootstrap.capability,
        "invalid private host capability"
    );
    // Never leak the trusted bootstrap channel into an executor/workload.
    ensure!(
        unsafe { libc::fcntl(INTERNAL_FD, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
        "close bootstrap on exec"
    );
    if bootstrap.worker {
        WORKER_DIRECTORY
            .set(bootstrap.directory)
            .map_err(|_| anyhow::anyhow!("worker context already initialized"))?;
        let request: Request = read_frame(&mut channel)?;
        let response = execute_request(request, &g);
        write_frame(&mut channel, &response)?;
    } else {
        let path = socket(&bootstrap.directory, &g);
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
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
            message: message.chars().take(1024).collect(),
        }),
    }
}
fn validate(request: &Request, g: &Generation) -> anyhow::Result<()> {
    ensure!(
        request.version == AGENTCTL_HOST_VERSION,
        "unsupported host protocol version"
    );
    ensure!(
        !request.request_id.is_empty() && request.request_id.len() <= 128,
        "invalid request identity"
    );
    ensure!(
        request.command.generation == g.generation,
        "host generation mismatch"
    );
    ensure!(
        request.target.is_none(),
        "Job selectors belong to the typed command; explicit target is unsupported"
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
    Ok(())
}
fn execute_request(request: Request, g: &Generation) -> Response {
    if let Err(e) = validate(&request, g) {
        return error(
            request.request_id,
            AgentCtlHostErrorCode::InvalidRequest,
            e.to_string(),
        );
    }
    let request_id = request.request_id;
    let context = request.command;
    let result = (|| {
        std::env::set_current_dir(&context.cwd)?;
        // Internal dispatch occurs before threads, terminal context or Tokio.
        for (key, value) in context.environment {
            unsafe {
                std::env::set_var(
                    std::ffi::OsString::from_vec(key),
                    std::ffi::OsString::from_vec(value),
                );
            }
        }
        super::terminal::restore_service_context(context.terminal);
        crate::diagnostics::init_inherited();
        super::terminal::init_child_context();
        if std::env::var("PVISOR_STARTUP_TIMING").as_deref() != Ok("0") {
            crate::diagnostics::diagnostic(format_args!(
                "pvisor-host-request level=info version={} request_id={} frontend_pid={} worker_pid={}",
                AGENTCTL_HOST_VERSION,
                serde_json::to_string(&request_id)?,
                context.client_pid,
                std::process::id()
            ));
        }
        super::host::execute(context.command)
    })();
    match result {
        Ok(exit_code) => Response {
            version: AGENTCTL_HOST_VERSION,
            request_id,
            result: Ok(Completion {
                exit_code,
                cancelled: false,
                signal: None,
            }),
        },
        Err(e) => error(
            request_id,
            AgentCtlHostErrorCode::Internal,
            format!("{e:#}"),
        ),
    }
}
fn disconnected(stream: &UnixStream) -> bool {
    let mut fd = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe {
        libc::poll(&mut fd, 1, 0);
    }
    if fd.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
        return true;
    }
    if fd.revents & libc::POLLIN != 0 {
        let mut byte = [0u8];
        return unsafe {
            libc::recv(
                stream.as_raw_fd(),
                byte.as_mut_ptr().cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        } == 0;
    }
    false
}
fn cancellation_signal(client: &mut UnixStream, request_id: &str) -> Option<i32> {
    if disconnected(client) {
        return Some(libc::SIGTERM);
    }
    let mut fd = libc::pollfd {
        fd: client.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe {
        libc::poll(&mut fd, 1, 0);
    }
    if fd.revents & libc::POLLIN == 0 {
        return None;
    }
    match read_frame::<ClientControl>(client) {
        Ok(ClientControl::Cancel {
            version,
            request_id: id,
            signal,
        }) if version == AGENTCTL_HOST_VERSION
            && id == request_id
            && [libc::SIGINT, libc::SIGTERM].contains(&signal) =>
        {
            Some(signal)
        }
        // A malformed continuation is not permission to detach a running Job.
        _ => Some(libc::SIGTERM),
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
        if active.fetch_add(1, Ordering::SeqCst) >= 64 {
            active.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let active = active.clone();
        let dir = dir.clone();
        let g = g.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                client.set_read_timeout(Some(Duration::from_secs(10)))?;
                client.set_write_timeout(Some(Duration::from_secs(10)))?;
                let fds = receive_fds(&client)?;
                let raw: serde_json::Value = match read_frame(&mut client) {
                    Ok(raw) => raw,
                    Err(e) => {
                        return write_frame(
                            &mut client,
                            &error(
                                String::new(),
                                AgentCtlHostErrorCode::InvalidRequest,
                                format!("invalid host request frame: {e:#}"),
                            ),
                        );
                    }
                };
                let request_id = raw
                    .get("request_id")
                    .and_then(|id| id.as_str())
                    .filter(|id| id.len() <= 128)
                    .unwrap_or("")
                    .to_owned();
                let request: Request = match serde_json::from_value(raw) {
                    Ok(request) => request,
                    Err(e) => {
                        return write_frame(
                            &mut client,
                            &error(
                                request_id,
                                AgentCtlHostErrorCode::InvalidRequest,
                                format!("invalid typed Job request: {e}"),
                            ),
                        );
                    }
                };
                let request_id = request.request_id.clone();
                if let Err(e) = validate(&request, &g) {
                    let code = if request.version != AGENTCTL_HOST_VERSION {
                        AgentCtlHostErrorCode::VersionMismatch
                    } else if request.command.generation != g.generation {
                        AgentCtlHostErrorCode::Conflict
                    } else if request.target.is_some() {
                        AgentCtlHostErrorCode::Unsupported
                    } else {
                        AgentCtlHostErrorCode::InvalidRequest
                    };
                    return write_frame(&mut client, &error(request_id, code, e.to_string()));
                }
                let (worker, mut channel) = match spawn_internal(
                    &dir,
                    &g,
                    Some(fds),
                    Some(&request.command.executable.path),
                ) {
                    Ok(worker) => worker,
                    Err(e) => {
                        return write_frame(
                            &mut client,
                            &error(
                                request_id,
                                AgentCtlHostErrorCode::Unavailable,
                                format!("spawn host worker: {e:#}"),
                            ),
                        );
                    }
                };
                let mut worker = Worker(worker);
                if let Err(e) = write_frame(&mut channel, &request) {
                    return write_frame(
                        &mut client,
                        &error(
                            request_id,
                            AgentCtlHostErrorCode::Unavailable,
                            format!(
                                "worker request send failed: {e:#}; effect may be ambiguous; not retried"
                            ),
                        ),
                    );
                }
                channel.set_read_timeout(None)?;
                let response_reader =
                    std::thread::spawn(move || read_frame::<Response>(&mut channel));
                let mut cancel_signal = None;
                loop {
                    if let Some(status) = worker.0.try_wait()? {
                        let response = match response_reader
                            .join()
                            .map_err(|_| anyhow::anyhow!("host worker response reader panicked"))?
                        {
                            Ok(mut response) => {
                                if let Ok(completion) = &mut response.result {
                                    completion.cancelled = cancel_signal.is_some();
                                    if let Some(signal) = cancel_signal {
                                        completion.signal = Some(signal);
                                        completion.exit_code = 128 + signal;
                                    }
                                }
                                response
                            }
                            Err(e) => {
                                use std::os::unix::process::ExitStatusExt;
                                if let Some(signal) = status.signal() {
                                    Response {
                                        version: AGENTCTL_HOST_VERSION,
                                        request_id,
                                        result: Ok(Completion {
                                            exit_code: 128 + signal,
                                            cancelled: cancel_signal.is_some(),
                                            signal: Some(signal),
                                        }),
                                    }
                                } else {
                                    error(
                                        request_id,
                                        AgentCtlHostErrorCode::Unavailable,
                                        format!(
                                            "worker exited without response ({status}): {e}; operation may have taken effect; do not retry automatically"
                                        ),
                                    )
                                }
                            }
                        };
                        return write_frame(&mut client, &response);
                    }
                    if cancel_signal.is_none() {
                        if let Some(signal) = cancellation_signal(&mut client, &request_id) {
                            cancel_signal = Some(signal);
                            unsafe {
                                libc::kill(worker.0.id() as i32, signal);
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            })();
            if let Err(error) = result {
                tracing::warn!("host request failed: {error:#}");
            }
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Ok(())
}

pub(crate) fn call(command: JobCommand) -> anyhow::Result<i32> {
    let (mut stream, g) = start_or_connect()?;
    let request_id = uuid::Uuid::new_v4().to_string();
    let request = Request {
        version: AGENTCTL_HOST_VERSION,
        request_id: request_id.clone(),
        target: None,
        command: ContextData {
            client_pid: std::process::id(),
            executable: current_executable()?,
            cwd: std::env::current_dir()?,
            terminal: super::terminal::service_context(),
            environment: std::env::vars_os()
                .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
                .collect(),
            generation: g.generation,
            command,
        },
    };
    // Serialize and size-check before handing off anything that can mutate state.
    ensure!(
        serde_json::to_vec(&request)?.len() <= AGENTCTL_HOST_MAX_FRAME_BYTES,
        "host request exceeds frame limit"
    );
    send_fds(&stream, &[0, 1, 2])?;
    write_frame(&mut stream, &request)
        .context("host request send failed; effect may be ambiguous; not retried")?;
    let mut cancellation = stream.try_clone()?;
    cancellation.set_write_timeout(Some(Duration::from_secs(10)))?;
    let rt = tokio::runtime::Runtime::new()?;
    let response = rt.block_on(async move {
        let mut result = tokio::task::spawn_blocking(move || read_frame::<Response>(&mut stream));
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            response = &mut result => Ok::<_, anyhow::Error>(response??),
            _ = tokio::signal::ctrl_c() => {
                            write_frame(&mut cancellation, &ClientControl::Cancel { version: AGENTCTL_HOST_VERSION, request_id: request.request_id.clone(), signal: libc::SIGINT })?;
                            Ok(result.await??)
                        },
            _ = terminate.recv() => {
                            write_frame(&mut cancellation, &ClientControl::Cancel { version: AGENTCTL_HOST_VERSION, request_id: request.request_id.clone(), signal: libc::SIGTERM })?;
                            Ok(result.await??)
                        },
        }
    }).context("host response unavailable; operation may have taken effect; not retried")?;
    ensure!(
        response.version == AGENTCTL_HOST_VERSION && response.request_id == request_id,
        "host response identity/version mismatch; not retried"
    );
    match response.result {
        Ok(completion) => {
            if completion.cancelled {
                eprintln!(
                    "pVisor host request cancelled; effects may already have occurred; not retried"
                );
            }
            Ok(completion.exit_code)
        }
        Err(e) => anyhow::bail!("host service {:?}: {}", e.code, e.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descriptor_handoff_and_bounded_frames() {
        let (a, mut b) = UnixStream::pair().unwrap();
        let input = File::open("/dev/null").unwrap();
        send_fds(&a, &[input.as_raw_fd(); 3]).unwrap();
        let fds = receive_fds(&b).unwrap();
        assert_eq!(fds.len(), 3);
        assert!(
            fds.iter()
                .all(
                    |fd| unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC
                        != 0
                )
        );
        let mut a = a;
        write_frame(&mut a, &vec!["typed", "request"]).unwrap();
        assert_eq!(
            read_frame::<Vec<String>>(&mut b).unwrap(),
            ["typed", "request"]
        );
        a.write_all(&((AGENTCTL_HOST_MAX_FRAME_BYTES + 1) as u32).to_be_bytes())
            .unwrap();
        assert!(read_frame::<serde_json::Value>(&mut b).is_err());
    }
    #[test]
    fn request_version_generation_and_environment_are_fenced() {
        let g = Generation {
            generation: uuid::Uuid::new_v4().to_string(),
            capability: uuid::Uuid::new_v4().to_string(),
        };
        let mut request = Request {
            version: AGENTCTL_HOST_VERSION,
            request_id: "one".into(),
            target: None,
            command: ContextData {
                client_pid: std::process::id(),
                executable: current_executable().unwrap(),
                cwd: PathBuf::from("/tmp"),
                environment: vec![],
                generation: g.generation.clone(),
                terminal: None,
                command: JobCommand::Drop(super::super::runtime::SelectArgs {
                    selector: PathBuf::from("last"),
                    output_dir: PathBuf::from(".pvisor/capture"),
                }),
            },
        };
        validate(&request, &g).unwrap();
        request.version += 1;
        assert!(validate(&request, &g).is_err());
        request.version = AGENTCTL_HOST_VERSION;
        request.command.generation = "stale".into();
        assert!(validate(&request, &g).is_err());
        request.command.generation = g.generation.clone();
        request
            .command
            .environment
            .push((b"BAD=KEY".to_vec(), b"value".to_vec()));
        assert!(validate(&request, &g).is_err());
    }

    #[test]
    fn missing_ancillary_descriptors_are_rejected() {
        let (mut a, b) = UnixStream::pair().unwrap();
        a.write_all(&[0x46]).unwrap();
        assert!(receive_fds(&b).is_err());
    }

    #[test]
    fn rejects_symlink_and_public_state() {
        let temp = tempfile::tempdir().unwrap();
        let dir = fs::canonicalize(temp.path()).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        private_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_dir(&dir).is_err());
        let link = dir.join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(private_dir(&link).is_err());
    }
}
