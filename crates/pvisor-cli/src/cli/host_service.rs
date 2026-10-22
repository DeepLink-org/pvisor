//! A terminal frontend starts exactly one Job owner over an inherited socketpair.
//! Running commands address that owner's recorded socket; stopped commands run here.
use super::{host::JobCommand, host_process::TerminalOwner};
use anyhow::{Context, ensure};
pub(super) use pvisor::host_transport::read_host_frame_sync as read_frame;
use pvisor::host_transport::{InstanceCommand, write_host_frame_sync as write_frame};
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
        fs::{MetadataExt, OpenOptionsExt},
        net::UnixStream,
        process::CommandExt,
    },
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicI32, Ordering},
    },
    time::{Duration, Instant},
};

const INTERNAL: &str = "--pvisor-internal-owner";
const INTERNAL_FD: i32 = 3;
const GRACE: Duration = Duration::from_secs(2);
// Runtime RAM cleanup allows 10s, and macFUSE graceful unmount allows 30s.
const FINALIZATION_GRACE: Duration = Duration::from_secs(45);
static OWNER_RUN: OnceLock<pvisor::RunControlHandle> = OnceLock::new();
static REQUEST_CANCEL: OnceLock<tokio_util::sync::CancellationToken> = OnceLock::new();
static REQUEST_TARGET: OnceLock<Option<AgentCtlTarget>> = OnceLock::new();
static OWNER_EVENTS: OnceLock<Arc<Mutex<UnixStream>>> = OnceLock::new();
static OWNER_READY: AtomicBool = AtomicBool::new(false);
static SOURCE_LOCK: Mutex<Option<pvisor::JobDirectoryLock>> = Mutex::new(None);
static OWNER_SIGNAL: AtomicI32 = AtomicI32::new(0);
static OWNER_FINALIZING: AtomicBool = AtomicBool::new(false);

pub(super) fn notify_cancel(signal: i32) {
    if OWNER_SIGNAL
        .compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    if let Some(token) = REQUEST_CANCEL.get() {
        token.cancel();
    }
    if OWNER_READY.load(Ordering::SeqCst)
        && let Some(events) = OWNER_EVENTS.get()
    {
        let _ = write_frame(
            &mut events.lock().unwrap_or_else(|e| e.into_inner()),
            &OwnerEvent::Cancelled { signal },
        );
    }
}
pub(super) fn release_source_lock() {
    SOURCE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
}
pub(super) fn observe_run(handle: &pvisor::RunHandle) {
    // An owner executes one Run; this clone observes status without issuing controls.
    let _ = OWNER_RUN.set(handle.controls());
}
pub(super) fn notify_cleanup() {
    OWNER_FINALIZING.store(true, Ordering::SeqCst);
}
pub(super) fn request_cancellation() -> Option<tokio_util::sync::CancellationToken> {
    REQUEST_CANCEL.get().cloned()
}
pub(super) fn check_cancelled() -> anyhow::Result<()> {
    ensure!(
        !REQUEST_CANCEL
            .get()
            .is_some_and(|token| token.is_cancelled()),
        "Job operation cancelled; effects may be uncertain; not retried"
    );
    Ok(())
}
pub(super) fn check_record(record: &pvisor::RunRecord) -> anyhow::Result<()> {
    check_cancelled()?;
    if let Some(Some(target)) = REQUEST_TARGET.get() {
        super::host::check_target(target, record)?;
    }
    Ok(())
}
pub(super) fn vm_control_socket(requested: Option<&Path>) -> anyhow::Result<Option<PathBuf>> {
    requested
        .map(pvisor::host_transport::instance_socket_override)
        .transpose()
}
pub(super) fn reject_guest_exposure(
    paths: impl IntoIterator<Item = PathBuf>,
) -> anyhow::Result<()> {
    let directory = fs::canonicalize("/tmp")?.join(format!("pvisor-host-{}", uid()));
    for path in paths {
        let path = super::run::canonical_candidate(&path)?;
        ensure!(
            !directory.starts_with(&path) && !path.starts_with(&directory),
            "guest filesystem source {} exposes private host authority",
            path.display()
        );
    }
    Ok(())
}
fn uid() -> u32 {
    unsafe { libc::geteuid() }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextData {
    client_pid: u32,
    binary_digest: String,
    cwd: PathBuf,
    environment: Vec<(Vec<u8>, Vec<u8>)>,
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
// Only metadata sampled around a successful content hash is reusable. In
// particular, dev/ino alone misses in-place builds and preserved mtimes.
#[derive(Debug, PartialEq, Eq)]
struct ImageIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    mode: u32,
    uid: u32,
    gid: u32,
}
impl ImageIdentity {
    fn of(m: &fs::Metadata) -> Self {
        Self {
            device: m.dev(),
            inode: m.ino(),
            size: m.len(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
            mode: m.mode(),
            uid: m.uid(),
            gid: m.gid(),
        }
    }
}
struct RunningImage {
    digest: String,
    identity: ImageIdentity,
    // Pin the verified inode for the lifetime of the cached identity.
    _file: File,
}
fn hash_file(mut file: impl Read) -> anyhow::Result<String> {
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
fn running_image() -> anyhow::Result<&'static RunningImage> {
    static IMAGE: OnceLock<RunningImage> = OnceLock::new();
    if let Some(image) = IMAGE.get() {
        return Ok(image);
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
    let mut file = file;
    let identity = ImageIdentity::of(&file.metadata()?);
    let digest = hash_file(&mut file)?;
    ensure!(
        identity == ImageIdentity::of(&file.metadata()?),
        "running executable changed during content verification"
    );
    let _ = IMAGE.set(RunningImage {
        digest,
        identity,
        _file: file,
    });
    Ok(IMAGE.get().expect("running image initialized"))
}
fn running_digest() -> anyhow::Result<String> {
    Ok(running_image()?.digest.clone())
}
fn verified_digest(
    file: &mut File,
    image: &RunningImage,
    before: ImageIdentity,
) -> anyhow::Result<String> {
    let digest = if before == image.identity {
        image.digest.clone()
    } else {
        file.rewind()?;
        hash_file(&mut *file)?
    };
    ensure!(
        before == ImageIdentity::of(&file.metadata()?),
        "owner executable changed during content verification"
    );
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
            "unsafe owner executable ancestor {}",
            parent.display()
        );
    }
    let mut file = OpenOptions::new()
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
        "owner executable ownership/identity mismatch"
    );
    ensure!(
        verified_digest(&mut file, running_image()?, ImageIdentity::of(&m))? == executable.digest,
        "owner executable content changed, including an in-place build"
    );
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
fn unblock_owner_signals() -> anyhow::Result<()> {
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaddset(&mut mask, sig);
        }
        ensure!(
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &mask, std::ptr::null_mut()) == 0,
            "unblock owner signals"
        );
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
#[serde(tag = "instruction", rename_all = "snake_case", deny_unknown_fields)]
enum OwnerInstruction {
    Admit,
    Cancel { signal: i32 },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum OwnerEvent {
    Ready { pid: i32 },
    Cancelled { signal: i32 },
    Complete { response: Response },
}

fn spawn_owner(
    channel: &UnixStream,
    executable: &Executable,
) -> anyhow::Result<std::process::Child> {
    let raw = unsafe { libc::fcntl(channel.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    ensure!(raw >= 0, "duplicate private owner channel");
    let inherited = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut command = Command::new(&executable.path);
    command
        .arg(INTERNAL)
        .env_clear()
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .process_group(0);
    unsafe {
        command.pre_exec(move || {
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                libc::sigaddset(&mut mask, signal);
            }
            let error = libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
            if error != 0 {
                return Err(std::io::Error::from_raw_os_error(error));
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

pub(crate) fn internal_if_requested() -> anyhow::Result<bool> {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new(INTERNAL)) {
        return Ok(false);
    }
    ensure!(
        std::env::args_os().count() == 2 && unsafe { libc::fcntl(INTERNAL_FD, libc::F_GETFD) } >= 0,
        "internal owner mode requires a private inherited socketpair"
    );
    let mut channel = unsafe { UnixStream::from_raw_fd(INTERNAL_FD) };
    pvisor::host_transport::authorize_host_peer_sync(&channel)?;
    channel.set_read_timeout(Some(Duration::from_secs(10)))?;
    channel.set_write_timeout(Some(GRACE))?;
    let request: Request = read_frame(&mut channel)?;
    request.validate()?;
    ensure!(
        running_digest()? == request.command.binary_digest,
        "executed owner differs from its launching frontend"
    );
    ensure!(
        unsafe { libc::fcntl(INTERNAL_FD, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
        "close bootstrap on exec"
    );
    run_owner(channel, request)?;
    Ok(true)
}

fn run_owner(mut channel: UnixStream, request: Request) -> anyhow::Result<()> {
    super::host_process::become_subreaper()?;
    REQUEST_TARGET
        .set(request.target.clone())
        .map_err(|_| anyhow::anyhow!("duplicate owner target"))?;
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
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let thread_gate = gate.clone();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .on_thread_start(move || {
            let (lock, wake) = &*thread_gate;
            let mut ready = lock.lock().unwrap_or_else(|e| e.into_inner());
            while !*ready {
                ready = wake.wait(ready).unwrap_or_else(|e| e.into_inner());
            }
            unblock_owner_signals().expect("unblock owner runtime signals");
        })
        .build()?;
    struct Release(Arc<(Mutex<bool>, std::sync::Condvar)>);
    impl Drop for Release {
        fn drop(&mut self) {
            let (lock, wake) = &*self.0;
            *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
            wake.notify_all();
        }
    }
    let release = Release(gate);
    REQUEST_CANCEL
        .set(tokio_util::sync::CancellationToken::new())
        .map_err(|_| anyhow::anyhow!("duplicate owner cancellation"))?;
    let events = Arc::new(Mutex::new(channel.try_clone()?));
    OWNER_EVENTS
        .set(events.clone())
        .map_err(|_| anyhow::anyhow!("duplicate owner event channel"))?;
    let _signals = signals(&rt)?;
    unblock_owner_signals()?;
    drop(release);
    write_frame(
        &mut events.lock().unwrap_or_else(|e| e.into_inner()),
        &OwnerEvent::Ready {
            pid: std::process::id() as i32,
        },
    )?;
    OWNER_READY.store(true, Ordering::SeqCst);
    let instruction: OwnerInstruction = read_frame(&mut channel)?;
    if let OwnerInstruction::Cancel { signal } = instruction {
        notify_cancel(signal);
    }
    channel.set_read_timeout(None)?;
    let mut controls = channel.try_clone()?;
    std::thread::spawn(move || {
        loop {
            match read_frame::<OwnerInstruction>(&mut controls) {
                Ok(OwnerInstruction::Cancel { signal })
                    if [libc::SIGINT, libc::SIGTERM, libc::SIGHUP].contains(&signal) =>
                {
                    notify_cancel(signal)
                }
                _ => {
                    notify_cancel(libc::SIGTERM);
                    break;
                }
            }
        }
    });
    let finished = Arc::new(AtomicBool::new(false));
    let done = finished.clone();
    let parent = context.client_pid as i32;
    let watchdog = std::thread::spawn(move || watch_owner(parent, done));

    let result = (|| {
        check_cancelled()?;
        context
            .command
            .validate_target(request.target.as_ref(), &context.cwd)?;
        if let Some(record) = context.command.selected_record(&context.cwd)? {
            context.command.preflight(&record)?;
            *SOURCE_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(
                pvisor::JobDirectoryLock::acquire(&record, context.command.operation())?,
            );
        }
        if std::env::var("PVISOR_STARTUP_TIMING").as_deref() != Ok("0") {
            pvisor::diagnostics::diagnostic(format_args!(
                "pvisor-owner-request level=info version={} request_id={} frontend_pid={} owner_pid={}",
                AGENTCTL_HOST_VERSION,
                serde_json::to_string(&request.request_id)?,
                context.client_pid,
                std::process::id()
            ));
        }
        super::host::execute(&rt, context.command)
    })();
    release_source_lock();
    finished.store(true, Ordering::SeqCst);
    let cleanup = watchdog
        .join()
        .map_err(|_| anyhow::anyhow!("owner cleanup panicked"))?;
    let result = result.and_then(|code| cleanup.map(|()| code));
    let signal = OWNER_SIGNAL.load(Ordering::SeqCst);
    let response = AgentCtlHostResponse {
        version: AGENTCTL_HOST_VERSION,
        request_id: request.request_id,
        result: result
            .map(|exit_code| Completion {
                exit_code: if signal != 0 { 128 + signal } else { exit_code },
                cancelled: signal != 0,
                signal: (signal != 0).then_some(signal),
            })
            .map_err(|error| {
                AgentCtlHostError::new(AgentCtlHostErrorCode::Internal, format!("{error:#}"))
            }),
    };
    write_frame(
        &mut events.lock().unwrap_or_else(|e| e.into_inner()),
        &OwnerEvent::Complete { response },
    )?;
    Ok(())
}

fn watch_owner(parent: i32, finished: Arc<AtomicBool>) -> anyhow::Result<()> {
    let mut tree = super::host_process::OwnedTree::owner(parent)?;
    let mut deadline = None;
    let mut finalization = false;
    loop {
        tree.refresh()?;
        if finished.load(Ordering::SeqCst) {
            return tree.cleanup(false);
        }
        if deadline.is_none()
            && (OWNER_SIGNAL.load(Ordering::SeqCst) != 0 || OWNER_FINALIZING.load(Ordering::SeqCst))
        {
            deadline = Some(Instant::now() + GRACE);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            if !finalization
                && OWNER_RUN
                    .get()
                    .is_some_and(|run| run.status().state.is_terminal())
            {
                finalization = true;
                deadline = Some(Instant::now() + FINALIZATION_GRACE);
                continue;
            }
            tree.cleanup(false)?;
            let signal = OWNER_SIGNAL.load(Ordering::SeqCst);
            std::process::exit(128 + if signal == 0 { libc::SIGTERM } else { signal });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct FrontOwner {
    child: Option<std::process::Child>,
    channel: UnixStream,
    _terminal: Option<TerminalOwner>,
}
impl FrontOwner {
    fn finish(&mut self) -> anyhow::Result<()> {
        if let Some(mut child) = self.child.take() {
            child.wait()?;
        }
        Ok(())
    }
}
impl Drop for FrontOwner {
    fn drop(&mut self) {
        let _ = self.channel.shutdown(std::net::Shutdown::Both);
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + GRACE * 2 + FINALIZATION_GRACE;
            while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

fn launch(
    signal: Arc<AtomicI32>,
    command: JobCommand,
    target: Option<AgentCtlTarget>,
) -> anyhow::Result<i32> {
    let interactive = command.inherits_terminal_input()?;
    if interactive {
        TerminalOwner::check_interactive()?;
    }
    let executable = current_executable()?;
    validate_executable(&executable)?;
    let (mut channel, owner_channel) = UnixStream::pair()?;
    let child = spawn_owner(&owner_channel, &executable)?;
    drop(owner_channel);
    let request_id = uuid::Uuid::new_v4().to_string();
    let request = Request {
        version: AGENTCTL_HOST_VERSION,
        request_id: request_id.clone(),
        target,
        command: ContextData {
            client_pid: std::process::id(),
            binary_digest: executable.digest,
            cwd: std::env::current_dir()?,
            environment: std::env::vars_os()
                .map(|(key, value)| (key.as_bytes().to_vec(), value.as_bytes().to_vec()))
                .collect(),
            command,
            terminal: super::terminal::service_context(),
        },
    };
    let mut owned = FrontOwner {
        child: Some(child),
        channel: channel.try_clone()?,
        _terminal: None,
    };
    channel.set_write_timeout(Some(GRACE))?;
    write_frame(&mut channel, &request)?;
    let mut reader = channel.try_clone()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        loop {
            let event = read_frame::<OwnerEvent>(&mut reader);
            let last = event.is_err() || matches!(event, Ok(OwnerEvent::Complete { .. }));
            if tx.send(event).is_err() || last {
                break;
            }
        }
    });
    let mut cancelled = None;
    loop {
        let caught = signal.load(Ordering::SeqCst);
        if caught != 0 && cancelled.is_none() {
            cancelled = Some(caught);
            write_frame(&mut channel, &OwnerInstruction::Cancel { signal: caught })?;
        }
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(Ok(OwnerEvent::Ready { pid })) => {
                ensure!(
                    owned
                        .child
                        .as_ref()
                        .is_some_and(|child| child.id() == pid as u32),
                    "owner readiness PID mismatch"
                );
                if interactive {
                    owned._terminal = TerminalOwner::give_to(pid)?;
                }
                if cancelled.is_none() {
                    write_frame(&mut channel, &OwnerInstruction::Admit)?;
                }
            }
            Ok(Ok(OwnerEvent::Cancelled { signal })) => {
                cancelled = Some(signal);
            }
            Ok(Ok(OwnerEvent::Complete { response })) => {
                response.validate(&request_id)?;
                owned.finish()?;
                let result = match response.result {
                    Ok(result) => result,
                    Err(error) if cancelled.is_some() => {
                        eprintln!("Job cancellation outcome uncertain; not retried: {error}");
                        return Ok(128 + cancelled.unwrap());
                    }
                    Err(error) => return Err(error.into()),
                };
                if result.cancelled {
                    eprintln!("request cancelled");
                }
                return Ok(result.exit_code);
            }
            Ok(Err(error)) => {
                if let Some(signal) = cancelled {
                    eprintln!(
                        "Job cancellation response lost; effects may be uncertain; not retried"
                    );
                    return Ok(128 + signal);
                }
                return Err(error)
                    .context("owner response lost; effects may be uncertain; not retried");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => {
                return Err(error)
                    .context("owner response lost; effects may be uncertain; not retried");
            }
        }
    }
}

pub(crate) fn call(mut command: JobCommand) -> anyhow::Result<i32> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;
    REQUEST_CANCEL
        .set(tokio_util::sync::CancellationToken::new())
        .map_err(|_| anyhow::anyhow!("duplicate frontend cancellation"))?;
    let signal = signals(&rt)?;
    let target = command.pin_target()?;
    REQUEST_TARGET
        .set(target.clone())
        .map_err(|_| anyhow::anyhow!("duplicate frontend target"))?;
    if signal.load(Ordering::SeqCst) != 0 {
        return Ok(128 + signal.load(Ordering::SeqCst));
    }
    let record = command.selected_record(&std::env::current_dir()?)?;
    if let Some(record) = &record {
        check_record(record)?;
    }
    let live = record
        .as_ref()
        .and_then(|record| record.live_control.as_ref())
        .is_some_and(|endpoint| endpoint.is_available());
    if matches!(&command, JobCommand::Resume(_)) && live {
        let record = record.as_ref().unwrap();
        let result = record.live_control.as_ref().unwrap().exchange_checked(
            &record.run_id,
            record
                .attempt_id
                .as_deref()
                .context("live Job has no Attempt identity")?,
            InstanceCommand::Resume {},
            Some(&check_cancelled),
        )?;
        println!("{}", serde_json::to_string(&result)?);
        return Ok(0);
    }
    if let Some(record) = &record {
        command.preflight(record)?;
    }
    if matches!(
        &command,
        JobCommand::Run(_) | JobCommand::Fork(_) | JobCommand::Resume(_)
    ) {
        return launch(signal, command, target);
    }
    if live && matches!(&command, JobCommand::Apply(_) | JobCommand::Drop(_)) {
        anyhow::bail!("Job owner still holds stage; apply/drop refused while it is running");
    }
    let _lock = if !live {
        record
            .as_ref()
            .map(|record| pvisor::JobDirectoryLock::acquire(record, command.operation()))
            .transpose()?
    } else {
        None
    };

    super::host::execute(&rt, command)
}
#[cfg(test)]
mod tests {
    use super::*;
    use pvisor_core::host_protocol::AGENTCTL_HOST_MAX_FRAME_BYTES;

    #[test]
    fn terminal_run_finishes_resource_cleanup_before_owner_escalates() {
        const NAME: &str = "terminal_run_finishes_resource_cleanup_before_owner_escalates";
        if std::env::var("PVISOR_OWNER_CLEANUP_TEST").as_deref() != Ok(NAME) {
            let module = module_path!().split_once("::").unwrap().1;
            assert!(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", &format!("{module}::{NAME}"), "--test-threads=1"])
                    .env("PVISOR_OWNER_CLEANUP_TEST", NAME)
                    .process_group(0)
                    .status()
                    .unwrap()
                    .success()
            );
            return;
        }
        use pvisor::RunExecutor;
        struct SlowRelease;
        impl Drop for SlowRelease {
            fn drop(&mut self) {
                std::thread::sleep(GRACE * 2);
            }
        }
        #[async_trait::async_trait]
        impl RunExecutor for SlowRelease {
            fn descriptor(&self) -> pvisor_core::ExecutorPlan {
                pvisor::ProcessExecutor::default().descriptor()
            }
            fn supports(&self, invocation: &pvisor_core::RunInvocation) -> bool {
                pvisor::ProcessExecutor::default().supports(invocation)
            }
            async fn execute(&self, session: &pvisor::Session) -> pvisor::ExecutorOutput {
                pvisor::ProcessExecutor::default().execute(session).await
            }
        }
        super::super::host_process::become_subreaper().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let visor = pvisor::PVisor::builder()
                .instance_control(false)
                .executors(vec![Arc::new(SlowRelease)])
                .build();
            let mut spec = pvisor_core::RunSpec::process("slow-release", "sh", "/bin/sh");
            let pvisor_core::RunInvocation::Process(process) = &mut spec.invocation;
            process.args = vec!["-c".into(), "exit 0".into()];
            let managed = pvisor::job_service::RuntimeJobService::start_managed(
                &visor,
                spec,
                pvisor::RunConfig::default(),
            )
            .await
            .unwrap();
            observe_run(managed.handle());
            drop(visor);
            OWNER_SIGNAL.store(libc::SIGTERM, Ordering::SeqCst);
            let finished = Arc::new(AtomicBool::new(false));
            let done = finished.clone();
            let parent = unsafe { libc::getppid() };
            let watcher = std::thread::spawn(move || watch_owner(parent, done));
            managed.cancel();
            managed.wait().await.unwrap();
            finished.store(true, Ordering::SeqCst);
            watcher.join().unwrap().unwrap();
        });
    }

    #[test]
    fn verified_image_reuses_hash_but_detects_in_place_and_inode_changes() {
        use std::io::Seek;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image");
        fs::write(&path, b"before").unwrap();
        let file = File::open(&path).unwrap();
        let metadata = file.metadata().unwrap();
        let image = RunningImage {
            digest: hash_file(File::open(&path).unwrap()).unwrap(),
            identity: ImageIdentity::of(&metadata),
            _file: file,
        };
        let mut unchanged = File::open(&path).unwrap();
        let before = ImageIdentity::of(&unchanged.metadata().unwrap());
        assert_eq!(
            verified_digest(&mut unchanged, &image, before).unwrap(),
            image.digest
        );
        assert_eq!(
            unchanged.stream_position().unwrap(),
            0,
            "cache hit read file contents"
        );

        // Same inode, size and restored mtime: ctime must still invalidate the
        // verified identity. Do not trust a timestamp/size-only cache.
        fs::write(&path, b"after!").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(metadata.modified().unwrap()))
            .unwrap();
        let mut changed = File::open(&path).unwrap();
        assert_eq!(changed.metadata().unwrap().ino(), image.identity.inode);
        let before = ImageIdentity::of(&changed.metadata().unwrap());
        assert_ne!(
            verified_digest(&mut changed, &image, before).unwrap(),
            image.digest
        );

        fs::remove_file(&path).unwrap();
        fs::write(&path, b"before").unwrap();
        let mut replaced = File::open(&path).unwrap();
        assert_ne!(replaced.metadata().unwrap().ino(), image.identity.inode);
        let before = ImageIdentity::of(&replaced.metadata().unwrap());
        assert_eq!(
            verified_digest(&mut replaced, &image, before).unwrap(),
            image.digest
        );
        assert_eq!(
            replaced.stream_position().unwrap(),
            6,
            "replacement was not hashed"
        );
    }
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
        OWNER_EVENTS.set(Arc::new(Mutex::new(events))).unwrap();
        OWNER_READY.store(true, Ordering::SeqCst);
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
                read_frame::<OwnerEvent>(&mut receiver).unwrap(),
                OwnerEvent::Cancelled {
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
        OWNER_EVENTS.set(Arc::new(Mutex::new(events))).unwrap();
        let cancellation = tokio_util::sync::CancellationToken::new();
        REQUEST_CANCEL.set(cancellation.clone()).unwrap();
        OWNER_READY.store(true, Ordering::SeqCst);
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
                assert!(matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut), "unexpected owner event after exit {code}");
            }
            for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
                OWNER_SIGNAL.store(0, Ordering::SeqCst);
                let runtime = pvisor::PVisor::builder().executors(vec![
                    super::super::run::report_terminal(Arc::new(pvisor::ProcessExecutor::default()))
                ]).build();
                let mut spec = pvisor_core::RunSpec::process(format!("signal-{signal}"), "sh", "/bin/sh");
                let pvisor_core::RunInvocation::Process(process) = &mut spec.invocation;
                process.args = vec!["-c".into(), format!("kill -{signal} $$")];
                let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
                assert_eq!(result.executor_observations.termination_signal, Some(signal));
                assert!(matches!(read_frame::<OwnerEvent>(&mut receiver).unwrap(), OwnerEvent::Cancelled { signal: actual } if actual == signal));
                assert!(cancellation.is_cancelled());
            }
        });
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
        let response: AgentCtlHostResponse<()> = AgentCtlHostResponse {
            version: 1,
            request_id: "x".repeat(256),
            result: Err(AgentCtlHostError::new(
                AgentCtlHostErrorCode::Conflict,
                "stale",
            )),
        };
        response.validate(&"x".repeat(256)).unwrap();
    }
    #[test]
    fn bounded_frames_and_private_paths() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        write_frame(&mut a, &42).unwrap();
        assert_eq!(read_frame::<u32>(&mut b).unwrap(), 42);
        assert!(write_frame(&mut a, &"x".repeat(AGENTCTL_HOST_MAX_FRAME_BYTES)).is_err());
    }
}
