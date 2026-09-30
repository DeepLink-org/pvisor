//! The container process pipeline.
//!
//! Both the init process and exec processes follow the same shape (house
//! "self-exec" pattern, cf. pvisor's INTERNAL_SANDBOX_ARG):
//!
//! - **init**: `Create` re-executes the shim binary as the internal parent
//!   (A). A enters the configured namespaces, mounts the rootfs, forks the
//!   init process (G), and relays G's readiness over a pipe. G mounts
//!   procfs, pivots into the container root, wires the inherited IO fds,
//!   signals readiness, and blocks on the start pipe until `Start`.
//! - **exec**: `Exec` re-executes as the exec parent (E). E joins the init
//!   process's namespaces (`/proc/<init-pid>/ns/*`), forks the exec process
//!   (F), which wires the same inherited IO fds and blocks until
//!   `Start(exec_id)`.
//!
//! After the start byte both paths run the shared tail: apply uid/gid/
//! capabilities/rlimits, `chdir`, `execve`.
//!
//! File-descriptor inheritance (pipes are non-CLOEXEC so they survive the
//! exec into A/E):
//!
//! ```text
//!   pipe        shim                A/E (internal parent)   G/F (process)
//!   ready       read end            write end -> relay      closed
//!   start       write end           closed after fork       read end, blocks
//!   fork report closed              read end                write end
//! ```
//! The workload-facing stdio descriptors are opened by the shim
//! ([`crate::fifo::ContainerIo`]) and travel the same way; each child closes
//! every other inherited fd before exec so the workload only sees stdio.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use log::{debug, warn};
use serde::{Deserialize, Serialize};

use crate::caps;
use crate::cgroup;
use crate::mount;
use crate::plan::{CgroupPlan, ContainerPlan, ExecPlan, IdMappingPlan, NamespaceKind, ProcessPlan};

/// CLI argument that turns the shim binary into the internal init parent.
pub const INTERNAL_INIT_ARG: &str = "--pvisor-shim-internal-init";
/// CLI argument that turns the shim binary into the internal exec parent.
pub const INTERNAL_EXEC_ARG: &str = "--pvisor-shim-internal-exec";
/// CLI argument that turns the shim binary into the internal VM runner.
#[cfg(feature = "vm")]
pub const INTERNAL_VM_ARG: &str = "--pvisor-shim-internal-vm";
/// Path of the serialized [`ContainerPlan`].
pub const ENV_PLAN: &str = "PVISOR_SHIM_INIT_PLAN";
/// Path of the serialized [`ExecPlan`].
pub const ENV_EXEC_PLAN: &str = "PVISOR_SHIM_EXEC_PLAN";
/// Write end of the ready pipe (A/E reports to the shim).
pub const ENV_READY_FD: &str = "PVISOR_SHIM_INIT_READY_FD";
/// Read end of the start pipe (G/F waits for one byte).
pub const ENV_START_FD: &str = "PVISOR_SHIM_INIT_START_FD";
/// Write end of the fork report pipe (G/F reports setup to A/E).
pub const ENV_FORK_REPORT_FD: &str = "PVISOR_SHIM_INIT_FORK_REPORT_FD";
/// Read end of the fork report pipe (A/E relays it to the shim).
pub const ENV_FORK_REPORT_READ_FD: &str = "PVISOR_SHIM_FORK_REPORT_READ_FD";
/// CLI argument that turns the shim binary into the pod sandbox holder.
pub const INTERNAL_SANDBOX_ARG: &str = "--pvisor-shim-internal-sandbox";
/// Path of the serialized [`crate::plan::SandboxPlan`].
pub const ENV_SANDBOX_PLAN: &str = "PVISOR_SHIM_SANDBOX_PLAN";
/// Pid of the pod sandbox holder; container runners join its namespaces.
pub const ENV_SANDBOX_PID: &str = "PVISOR_SHIM_SANDBOX_PID";
/// Workload stdio descriptors handed over by the shim.
pub const ENV_STDIO_IN: &str = "PVISOR_SHIM_STDIO_IN";
pub const ENV_STDIO_OUT: &str = "PVISOR_SHIM_STDIO_OUT";
pub const ENV_STDIO_ERR: &str = "PVISOR_SHIM_STDIO_ERR";
pub const ENV_STDIO_TERMINAL: &str = "PVISOR_SHIM_STDIO_TERMINAL";

/// Message exchanged over the ready/fork-report pipes, one JSON line.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum InitReport {
    Ok { pid: u32 },
    Error { context: String, errno: i32 },
}

impl InitReport {
    fn line(&self) -> Result<Vec<u8>> {
        let mut line = serde_json::to_vec(self).context("serialize init report")?;
        line.push(b'\n');
        Ok(line)
    }

    fn parse(line: &str) -> Result<InitReport> {
        serde_json::from_str(line.trim_end()).context("parse init report")
    }
}

/// Host-side state kept by the task service between Create/Exec and Start.
pub struct InternalChild {
    /// PID of the workload process, set once the parent reports readiness.
    pub pid: Option<u32>,
    /// Write end of the start pipe; held until Start (or cleanup).
    start_fd: RawFd,
}

impl InternalChild {
    /// An internal child with nothing left to release; useful as a
    /// placeholder when moving the real one out of a slot.
    pub fn exited() -> Self {
        InternalChild {
            pid: None,
            start_fd: -1,
        }
    }

    /// Signal the child to exec; consumes the start pipe write end.
    pub fn start(&mut self) -> Result<()> {
        let byte = b"s";
        let written = unsafe { libc::write(self.start_fd, byte.as_ptr().cast(), byte.len()) };
        self.close_start();
        if written < 0 {
            return Err(std::io::Error::last_os_error())
                .context("write start pipe (process may have exited)");
        }
        Ok(())
    }

    /// Release the start pipe write end; the child sees EOF and exits.
    pub fn close_start(&mut self) {
        if self.start_fd >= 0 {
            unsafe { libc::close(self.start_fd) };
            self.start_fd = -1;
        }
    }
}

impl Drop for InternalChild {
    fn drop(&mut self) {
        self.close_start();
    }
}

fn make_pipe() -> Result<(RawFd, RawFd)> {
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("pipe");
    }
    Ok((fds[0], fds[1]))
}

/// The stdio descriptors the shim handed over, parsed back in the child.
#[derive(Clone, Copy, Debug)]
pub struct StdioFds {
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
    pub terminal: bool,
}

impl StdioFds {
    fn from_env() -> Result<Self> {
        let stdin = fd_from_env(ENV_STDIO_IN)?;
        let stdout = fd_from_env(ENV_STDIO_OUT)?;
        let stderr = fd_from_env(ENV_STDIO_ERR)?;
        let terminal = std::env::var(ENV_STDIO_TERMINAL).as_deref() == Ok("1");
        Ok(StdioFds {
            stdin,
            stdout,
            stderr,
            terminal,
        })
    }
}

/// Re-exec the shim binary as an internal parent and wait for its relay.
///
/// `stdio` are the shim-opened workload descriptors (passed through env);
/// `plan_bytes` is the serialized plan written to `plan_path`. Blocks until
/// the workload process signals readiness, so call from a blocking context.
pub fn spawn_internal(
    arg: &str,
    plan_path: &Path,
    plan_bytes: &[u8],
    stdio: Option<StdioFds>,
    sandbox_pid: Option<u32>,
) -> Result<InternalChild> {
    spawn_internal_opts(arg, plan_path, plan_bytes, stdio, sandbox_pid, true)
}

/// [`spawn_internal`] with control over waiting for the internal parent to
/// exit: the sandbox holder IS the parent and stays alive, so it must be
/// spawned detached.
pub fn spawn_internal_opts(
    arg: &str,
    plan_path: &Path,
    plan_bytes: &[u8],
    stdio: Option<StdioFds>,
    sandbox_pid: Option<u32>,
    wait_parent_exit: bool,
) -> Result<InternalChild> {
    fs::write(plan_path, plan_bytes).with_context(|| format!("write {}", plan_path.display()))?;

    let (ready_r, ready_w) = make_pipe()?;
    let (start_r, start_w) = make_pipe()?;
    let (fork_r, fork_w) = make_pipe()?;

    let exe = std::env::current_exe().context("current exe")?;
    let mut command = Command::new(exe);
    command
        .arg(arg)
        .env(ENV_READY_FD, ready_w.to_string())
        .env(ENV_START_FD, start_r.to_string())
        .env(ENV_FORK_REPORT_FD, fork_w.to_string())
        .env(ENV_FORK_REPORT_READ_FD, fork_r.to_string());
    let plan_env = if arg == INTERNAL_EXEC_ARG {
        ENV_EXEC_PLAN
    } else if arg == INTERNAL_SANDBOX_ARG {
        ENV_SANDBOX_PLAN
    } else {
        ENV_PLAN
    };
    command.env(plan_env, plan_path);
    if let Some(pid) = sandbox_pid {
        command.env(ENV_SANDBOX_PID, pid.to_string());
    }
    if let Some(stdio) = stdio {
        command
            .env(ENV_STDIO_IN, stdio.stdin.to_string())
            .env(ENV_STDIO_OUT, stdio.stdout.to_string())
            .env(ENV_STDIO_ERR, stdio.stderr.to_string())
            .env(ENV_STDIO_TERMINAL, if stdio.terminal { "1" } else { "0" });
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .context("spawn internal process")?;

    // The internal parent inherited every end; drop the copies this
    // process must not hold.
    for fd in [ready_w, start_r, fork_r, fork_w] {
        unsafe { libc::close(fd) };
    }

    let mut line = String::new();
    {
        let mut reader = BufReader::new(unsafe { fs::File::from_raw_fd(ready_r) });
        reader.read_line(&mut line).context("read init report")?;
    }
    if wait_parent_exit {
        let _ = child.wait();
    } else {
        // Detached: the parent stays alive; dropping the handle leaks it
        // into init (the shim is a subreaper, so reaping still works).
        std::mem::forget(child);
    }

    match InitReport::parse(&line)? {
        InitReport::Ok { pid } => Ok(InternalChild {
            pid: Some(pid),
            start_fd: start_w,
        }),
        InitReport::Error { context, errno } => {
            unsafe { libc::close(start_w) };
            anyhow::bail!("internal process setup failed: {context} (errno {errno})");
        }
    }
}

/// Entry point of the internal parents. Returns Ok(false) when the binary
/// was not invoked in internal mode; otherwise never returns.
pub fn run_internal_if_requested() -> Result<bool> {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(feature = "vm")]
    if args.iter().any(|arg| arg == INTERNAL_VM_ARG) {
        return run_vm_runner_requested();
    }
    if args.iter().any(|arg| arg == INTERNAL_SANDBOX_ARG) {
        return run_sandbox_holder_requested();
    }
    let (mode_arg, main) = if args.iter().any(|arg| arg == INTERNAL_INIT_ARG) {
        (INTERNAL_INIT_ARG, init_parent_main as fn() -> Result<()>)
    } else if args.iter().any(|arg| arg == INTERNAL_EXEC_ARG) {
        (INTERNAL_EXEC_ARG, exec_parent_main as fn() -> Result<()>)
    } else {
        return Ok(false);
    };
    let exit_code = match main() {
        Ok(()) => 0,
        Err(error) => {
            let report = InitReport::Error {
                context: format!("{error:#}"),
                errno: 1,
            };
            let mut delivered = false;
            if let Some(mut file) = ready_pipe_from_env() {
                delivered = file
                    .write_all(&report.line().unwrap_or_default())
                    .and_then(|()| file.flush())
                    .is_ok();
            }
            if !delivered {
                eprintln!("pvisor shim internal ({mode_arg}) failed: {error:#}");
            }
            1
        }
    };
    std::process::exit(exit_code);
}

fn fd_from_env(name: &str) -> Result<RawFd> {
    std::env::var(name)
        .with_context(|| format!("missing {name}"))?
        .parse()
        .with_context(|| format!("parse {name}"))
}

fn ready_pipe_from_env() -> Option<fs::File> {
    fd_from_env(ENV_READY_FD)
        .ok()
        .map(|fd| unsafe { fs::File::from_raw_fd(fd) })
}

fn read_plan<T: for<'de> Deserialize<'de>>(env: &str) -> Result<T> {
    let plan_path = std::env::var(env).with_context(|| format!("missing {env}"))?;
    serde_json::from_slice(&fs::read(&plan_path).with_context(|| format!("read {plan_path}"))?)
        .with_context(|| format!("parse {plan_path}"))
}

fn init_parent_main() -> Result<()> {
    let plan: ContainerPlan = read_plan(ENV_PLAN)?;
    let start_fd = fd_from_env(ENV_START_FD)?;
    let fork_report_fd = fd_from_env(ENV_FORK_REPORT_FD)?;

    // Namespace entry order matters: join existing namespaces (CRI sandbox
    // network et al.) before unsharing the private ones.
    join_namespace_paths(&plan)?;
    // Containers of a pod join the sandbox holder's shared namespaces for
    // anything their own spec does not configure explicitly.
    if let Some(sandbox_pid) = sandbox_pid_from_env() {
        join_sandbox_namespaces(&plan, sandbox_pid);
    }
    let unshare_flags = collect_unshare_flags(&plan);
    if unshare_flags != 0 && unsafe { libc::unshare(unshare_flags) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context(format!("unshare(0x{unshare_flags:x})"));
    }
    if plan.has_new_user_namespace() {
        setup_user_namespace(&plan.uid_mappings, &plan.gid_mappings)?;
    }
    mount::make_mounts_private()?;
    mount::apply_mounts(&plan.rootfs_mounts, &plan.rootfs)?;
    mount::apply_mounts(&plan.mounts, &plan.rootfs)?;
    if plan.has_new_uts_namespace()
        && let Some(hostname) = plan.hostname.as_deref()
        && unsafe { libc::sethostname(hostname.as_ptr().cast(), hostname.len()) } != 0
    {
        return Err(std::io::Error::last_os_error()).context(format!("sethostname {hostname}"));
    }

    let g_pid = unsafe { libc::fork() };
    if g_pid < 0 {
        return Err(std::io::Error::last_os_error()).context("fork init process");
    }
    if g_pid == 0 {
        init_process_main(&plan, start_fd, fork_report_fd);
    }
    let g_pid = g_pid as u32;

    // A no longer needs the start pipe or the report write end (G holds
    // the only copy, so its exit closes the pipe).
    unsafe { libc::close(start_fd) };
    unsafe { libc::close(fork_report_fd) };

    // Cgroup bookkeeping with host paths still visible.
    if let Some(cgroup_plan) = plan.cgroup.as_ref()
        && let Err(error) = attach_cgroup(cgroup_plan, g_pid)
    {
        warn!("cgroup setup skipped: {error:#}");
    }

    relay_fork_report(fork_report_fd, g_pid)
}

/// VM runner: wait for Start, then boot the VM and exit with its code.
/// Returns Ok(false) only if the process was not invoked in VM mode.
#[cfg(feature = "vm")]
fn run_vm_runner_requested() -> Result<bool> {
    let exit_code = match vm_runner_main() {
        Ok(code) => code,
        Err(error) => {
            // Best effort: surface the failure through the ready pipe so
            // Create fails cleanly instead of hanging.
            let report = InitReport::Error {
                context: format!("{error:#}"),
                errno: 1,
            };
            let mut delivered = false;
            if let Some(mut file) = ready_pipe_from_env() {
                delivered = file
                    .write_all(&report.line().unwrap_or_default())
                    .and_then(|()| file.flush())
                    .is_ok();
            }
            if !delivered {
                eprintln!("pvisor shim VM runner failed: {error:#}");
            }
            1
        }
    };
    std::process::exit(exit_code);
}

/// The VM is the isolation boundary: no container namespaces here. A
/// private mount namespace keeps the snapshotter materialization off the
/// host while the in-process virtio-fs still serves it to the guest.
#[cfg(feature = "vm")]
fn vm_runner_main() -> Result<i32> {
    let plan: ContainerPlan = read_plan(ENV_PLAN)?;
    let start_fd = fd_from_env(ENV_START_FD)?;

    if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
        return Err(std::io::Error::last_os_error()).context("unshare(CLONE_NEWNS)");
    }
    mount::make_mounts_private()?;
    // Spec mounts (proc/sysfs/...) are guest-kernel business in a VM; only
    // the snapshotter rootfs entries need materializing.
    mount::apply_mounts(&plan.rootfs_mounts, &plan.rootfs)?;

    // Readiness: this process is the task pid from containerd's point of
    // view, and it stays alive for the whole VM lifetime.
    {
        let mut ready = ready_pipe_from_env().context("ready pipe")?;
        let message = InitReport::Ok {
            pid: std::process::id(),
        };
        ready
            .write_all(&message.line()?)
            .and_then(|()| ready.flush())
            .context("report VM readiness")?;
    }

    // Wait for Start (one byte). EOF means the shim gave up on this task.
    let mut byte = [0u8; 1];
    let read = unsafe { libc::read(start_fd, byte.as_mut_ptr().cast(), 1) };
    unsafe { libc::close(start_fd) };
    if read <= 0 {
        return Ok(255);
    }

    // The virtio-console host side is wired to this process's stdio.
    let stdio = StdioFds::from_env()?;
    unsafe {
        libc::dup2(stdio.stdin, libc::STDIN_FILENO);
        libc::dup2(stdio.stdout, libc::STDOUT_FILENO);
        libc::dup2(stdio.stderr, libc::STDERR_FILENO);
    }

    crate::vm::boot_vm(&plan)
}

/// Sandbox holder entry; never returns when invoked in holder mode.
fn run_sandbox_holder_requested() -> Result<bool> {
    let exit_code = match sandbox_holder_main() {
        Ok(()) => 0,
        Err(error) => {
            let report = InitReport::Error {
                context: format!("{error:#}"),
                errno: 1,
            };
            let mut delivered = false;
            if let Some(mut file) = ready_pipe_from_env() {
                delivered = file
                    .write_all(&report.line().unwrap_or_default())
                    .and_then(|()| file.flush())
                    .is_ok();
            }
            if !delivered {
                eprintln!("pvisor shim sandbox holder failed: {error:#}");
            }
            1
        }
    };
    std::process::exit(exit_code);
}

/// The sandbox holder replaces CRI's pause container: it owns the pod
/// namespaces (uts/ipc always new, network joined or new, pid new only for
/// shareProcessNamespace pods) and lives until ShutdownSandbox.
fn sandbox_holder_main() -> Result<()> {
    let plan: crate::plan::SandboxPlan = read_plan(ENV_SANDBOX_PLAN)?;
    let start_fd = fd_from_env(ENV_START_FD)?;

    if let Some(netns) = plan.netns_path.as_deref() {
        setns_by_path(netns, NamespaceKind::Network).context("join sandbox network namespace")?;
    }
    let mut flags = libc::CLONE_NEWUTS | libc::CLONE_NEWIPC;
    if plan.share_pid_namespace {
        flags |= libc::CLONE_NEWPID;
    }
    if unsafe { libc::unshare(flags) } != 0 {
        return Err(std::io::Error::last_os_error()).context("unshare sandbox namespaces");
    }
    if let Some(hostname) = plan.hostname.as_deref()
        && unsafe { libc::sethostname(hostname.as_ptr().cast(), hostname.len()) } != 0
    {
        return Err(std::io::Error::last_os_error()).context(format!("sethostname {hostname}"));
    }

    // A pid namespace only exists while a member runs; with pid sharing the
    // joinable owner is a sleeper child inside the namespace.
    let mut reported_pid = std::process::id();
    if plan.share_pid_namespace {
        let sleeper = unsafe { libc::fork() };
        if sleeper < 0 {
            return Err(std::io::Error::last_os_error()).context("fork sandbox sleeper");
        }
        if sleeper == 0 {
            pause_forever();
        }
        reported_pid = sleeper as u32;
    }

    {
        let mut ready = ready_pipe_from_env().context("ready pipe")?;
        let message = InitReport::Ok { pid: reported_pid };
        ready
            .write_all(&message.line()?)
            .and_then(|()| ready.flush())
            .context("report sandbox readiness")?;
    }

    let mut byte = [0u8; 1];
    let read = unsafe { libc::read(start_fd, byte.as_mut_ptr().cast(), 1) };
    unsafe { libc::close(start_fd) };
    if read <= 0 {
        return Ok(());
    }
    pause_forever()
}

/// Sleep until a signal arrives; used by the sandbox holder and sleeper.
fn pause_forever() -> ! {
    loop {
        // Signal-driven exit: any delivered signal terminates the default
        // disposition, so the sleep only ever returns on EINTR.
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// E: join the init process's namespaces, then fork the exec process.
fn exec_parent_main() -> Result<()> {
    let plan: ExecPlan = read_plan(ENV_EXEC_PLAN)?;
    let start_fd = fd_from_env(ENV_START_FD)?;
    let fork_report_fd = fd_from_env(ENV_FORK_REPORT_FD)?;

    join_init_namespaces(plan.init_pid)?;

    let f_pid = unsafe { libc::fork() };
    if f_pid < 0 {
        return Err(std::io::Error::last_os_error()).context("fork exec process");
    }
    if f_pid == 0 {
        exec_process_main(&plan, start_fd, fork_report_fd);
    }
    let f_pid = f_pid as u32;

    // E no longer needs the start pipe or the report write end (F holds
    // the only copy).
    unsafe { libc::close(start_fd) };
    unsafe { libc::close(fork_report_fd) };
    relay_fork_report(fork_report_fd, f_pid)
}

/// Read the child's fork report and relay it to the shim's ready pipe.
fn relay_fork_report(_write_end: RawFd, child_pid: u32) -> Result<()> {
    // Read from the pipe's read end; the env var carries it explicitly
    // because fd numbers are all the parent has after the self-exec.
    let read_end = fd_from_env(ENV_FORK_REPORT_READ_FD).context("fork report read end")?;
    let mut line = String::new();
    {
        let mut reader = BufReader::new(unsafe { fs::File::from_raw_fd(read_end) });
        reader.read_line(&mut line).context("read fork report")?;
    }
    let relayed = match InitReport::parse(&line)? {
        InitReport::Ok { .. } => InitReport::Ok { pid: child_pid },
        error @ InitReport::Error { .. } => error,
    };
    let mut ready = ready_pipe_from_env().context("ready pipe")?;
    ready
        .write_all(&relayed.line()?)
        .and_then(|()| ready.flush())
        .context("relay fork report")?;
    Ok(())
}

/// The kinds a pod shares through the sandbox holder.
const SANDBOX_SHARED_KINDS: [NamespaceKind; 3] = [
    NamespaceKind::Uts,
    NamespaceKind::Ipc,
    NamespaceKind::Network,
];

fn sandbox_pid_from_env() -> Option<u32> {
    std::env::var(ENV_SANDBOX_PID).ok()?.parse().ok()
}

/// Join the holder's shared namespaces for kinds the container spec leaves
/// unconfigured; failures fall back to a fresh namespace (logged).
fn join_sandbox_namespaces(plan: &ContainerPlan, sandbox_pid: u32) {
    for kind in SANDBOX_SHARED_KINDS {
        if plan.has_namespace(kind) {
            continue;
        }
        let path = format!("/proc/{sandbox_pid}/ns/{}", kind.proc_ns_name());
        match setns_by_path(&path, kind) {
            Ok(()) => {}
            Err(error) => warn!(
                "cannot join sandbox {} namespace of pid {sandbox_pid}: {error:#}",
                kind.proc_ns_name()
            ),
        }
    }
}

fn collect_unshare_flags(plan: &ContainerPlan) -> libc::c_int {
    let mut flags = libc::CLONE_NEWNS;
    for namespace in &plan.namespaces {
        if namespace.path.is_some() {
            continue;
        }
        match namespace.kind {
            // CLONE_NEWNS is always part of the unshare call above.
            NamespaceKind::Mount => {}
            other => flags |= other.clone_flag(),
        }
    }
    flags
}

fn join_namespace_paths(plan: &ContainerPlan) -> Result<()> {
    for namespace in &plan.namespaces {
        let Some(path) = namespace.path.as_deref() else {
            continue;
        };
        setns_by_path(&path.display().to_string(), namespace.kind)?;
    }
    Ok(())
}

/// Join every namespace of the init process we can address, in the order
/// the kernel expects (user namespace first, pid last — pid membership only
/// applies to children, which is why the exec process forks afterwards).
fn join_init_namespaces(init_pid: u32) -> Result<()> {
    let order = [
        NamespaceKind::User,
        NamespaceKind::Ipc,
        NamespaceKind::Uts,
        NamespaceKind::Network,
        NamespaceKind::Mount,
        NamespaceKind::Pid,
    ];
    let mut joined = 0;
    for kind in order {
        let path = format!("/proc/{init_pid}/ns/{}", kind.proc_ns_name());
        if setns_by_path(&path, kind).is_ok() {
            joined += 1;
        } else {
            warn!(
                "exec cannot join {} namespace of pid {init_pid}",
                kind.proc_ns_name()
            );
        }
    }
    if joined == 0 {
        anyhow::bail!("could not join any namespace of pid {init_pid}");
    }
    Ok(())
}

fn setns_by_path(path: &str, kind: NamespaceKind) -> Result<()> {
    let name = kind.proc_ns_name();
    let cpath =
        std::ffi::CString::new(path).with_context(|| format!("namespace path {path} has NUL"))?;
    let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("open {name} namespace at {path}"));
    }
    let ret = unsafe { libc::setns(fd, kind.clone_flag()) };
    unsafe { libc::close(fd) };
    if ret != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("setns {name} namespace at {path}"));
    }
    Ok(())
}

fn setup_user_namespace(
    uid_mappings: &[IdMappingPlan],
    gid_mappings: &[IdMappingPlan],
) -> Result<()> {
    write_id_map("setgroups", "deny")?;
    if uid_mappings.is_empty() {
        write_id_map("uid_map", &format!("0 {} 1", unsafe { libc::getuid() }))?;
    } else {
        write_id_map(
            "uid_map",
            &uid_mappings
                .iter()
                .map(IdMappingPlan::render)
                .collect::<Vec<_>>()
                .join("\n"),
        )?;
    }
    if gid_mappings.is_empty() {
        write_id_map("gid_map", &format!("0 {} 1", unsafe { libc::getgid() }))?;
    } else {
        write_id_map(
            "gid_map",
            &gid_mappings
                .iter()
                .map(IdMappingPlan::render)
                .collect::<Vec<_>>()
                .join("\n"),
        )?;
    }
    Ok(())
}

fn write_id_map(file: &str, content: &str) -> Result<()> {
    let path = format!("/proc/self/{file}");
    fs::write(&path, content).with_context(|| format!("write {path}"))
}

fn attach_cgroup(plan: &CgroupPlan, pid: u32) -> Result<()> {
    let Some(dir) = cgroup::cgroup_dir(plan) else {
        return Ok(());
    };
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    for (file, value) in cgroup::control_files(plan) {
        fs::write(dir.join(file), value)
            .with_context(|| format!("write {file} in {}", dir.display()))?;
    }
    fs::write(dir.join("cgroup.procs"), pid.to_string())
        .with_context(|| format!("attach {pid} to {}", dir.display()))?;
    Ok(())
}

/// G: finish the container setup, report readiness, wait for start, exec.
///
/// Runs in the forked child of A; never returns.
fn init_process_main(plan: &ContainerPlan, start_fd: RawFd, fork_report_fd: RawFd) -> ! {
    let fail = |context: String| -> ! {
        let report = InitReport::Error { context, errno: 1 };
        let mut file = unsafe { fs::File::from_raw_fd(fork_report_fd) };
        let _ = file.write_all(&report.line().unwrap_or_default());
        let _ = file.flush();
        std::process::exit(1);
    };

    let stdio = StdioFds::from_env().unwrap_or_else(|error| fail(format!("{error:#}")));
    close_inherited_fds(&[
        0,
        1,
        2,
        start_fd,
        fork_report_fd,
        stdio.stdin,
        stdio.stdout,
        stdio.stderr,
    ]);

    // procfs must be mounted by a member of the new pid namespace, i.e. G.
    if let Err(error) = mount::mount_proc(&plan.rootfs) {
        fail(format!("{error:#}"));
    }
    if let Err(error) = mount::pivot_root(&plan.rootfs) {
        fail(format!("{error:#}"));
    }
    if plan.root_readonly
        && let Err(error) = mount::remount_root_readonly()
    {
        warn!("root read-only remount failed: {error:#}");
    }

    finish_process(&plan.process, stdio, start_fd, fork_report_fd)
}

/// F: wire the inherited IO, report readiness, wait for start, exec.
///
/// Runs in the forked child of E, already inside the container namespaces;
/// never returns.
fn exec_process_main(plan: &ExecPlan, start_fd: RawFd, fork_report_fd: RawFd) -> ! {
    let fail = |context: String| -> ! {
        let report = InitReport::Error { context, errno: 1 };
        let mut file = unsafe { fs::File::from_raw_fd(fork_report_fd) };
        let _ = file.write_all(&report.line().unwrap_or_default());
        let _ = file.flush();
        std::process::exit(1);
    };

    let stdio = StdioFds::from_env().unwrap_or_else(|error| fail(format!("{error:#}")));
    close_inherited_fds(&[
        0,
        1,
        2,
        start_fd,
        fork_report_fd,
        stdio.stdin,
        stdio.stdout,
        stdio.stderr,
    ]);

    finish_process(&plan.process, stdio, start_fd, fork_report_fd)
}

/// Shared tail of init and exec processes: stdio, session setup, the
/// start-gate, process identity, and exec.
fn finish_process(
    process: &ProcessPlan,
    stdio: StdioFds,
    start_fd: RawFd,
    fork_report_fd: RawFd,
) -> ! {
    // Detach into a session/process group so signals addressed to the task
    // reach the workload; terminals also claim a controlling tty.
    unsafe {
        if stdio.terminal {
            libc::setsid();
            let ret = libc::ioctl(stdio.stdin, libc::TIOCSCTTY, 0);
            if ret != 0 {
                warn!("TIOCSCTTY failed: {}", std::io::Error::last_os_error());
            }
        } else {
            libc::setpgid(0, 0);
        }
        libc::dup2(stdio.stdin, libc::STDIN_FILENO);
        libc::dup2(stdio.stdout, libc::STDOUT_FILENO);
        libc::dup2(stdio.stderr, libc::STDERR_FILENO);
    }

    // Signal readiness, then stop holding the report pipe open.
    {
        let message = InitReport::Ok {
            pid: std::process::id(),
        };
        let mut file = unsafe { fs::File::from_raw_fd(fork_report_fd) };
        let _ = file.write_all(&message.line().unwrap_or_default());
        let _ = file.flush();
    }

    // Wait for Start (one byte). EOF means the shim gave up on this task.
    let mut byte = [0u8; 1];
    let read = unsafe { libc::read(start_fd, byte.as_mut_ptr().cast(), 1) };
    unsafe { libc::close(start_fd) };
    if read <= 0 {
        std::process::exit(255);
    }

    if let Err(error) = apply_process_identity(process) {
        eprintln!("pvisor shim: {error:#}");
        std::process::exit(crate::EXEC_FAILURE_EXIT_CODE);
    }
    if let Err(error) = exec_process(process) {
        eprintln!("pvisor shim: exec failed: {error:#}");
        std::process::exit(crate::EXEC_FAILURE_EXIT_CODE);
    }
    unreachable!("exec never returns on success")
}

/// Close every open fd except the listed ones.
fn close_inherited_fds(keep: &[RawFd]) {
    let Ok(entries) = fs::read_dir("/proc/self/fd") else {
        warn!("cannot enumerate /proc/self/fd to close inherited fds");
        return;
    };
    // Collect first: closing the directory fd mid-iteration would break it.
    let closable: Vec<RawFd> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| name.parse::<RawFd>().ok())
        .filter(|fd| *fd > 2 && !keep.contains(fd))
        .collect();
    for fd in closable {
        unsafe { libc::close(fd) };
    }
}

fn apply_process_identity(process: &ProcessPlan) -> Result<()> {
    let user = &process.user;

    for rlimit in &process.rlimits {
        apply_rlimit(rlimit.typ.as_str(), rlimit.soft, rlimit.hard)?;
    }

    // SAFETY: identity syscalls in the forked, single-threaded child.
    unsafe {
        if user.additional_gids.is_empty() {
            libc::setgroups(0, std::ptr::null());
        } else {
            libc::setgroups(user.additional_gids.len(), user.additional_gids.as_ptr());
        }
        libc::setresgid(user.gid, user.gid, user.gid);
        libc::setresuid(user.uid, user.uid, user.uid);
    }

    apply_capabilities(&process.capabilities)?;

    if let Some(umask) = user.umask {
        unsafe { libc::umask(umask) };
    }
    if process.no_new_privileges
        && unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0
    {
        return Err(std::io::Error::last_os_error()).context("PR_SET_NO_NEW_PRIVS");
    }
    Ok(())
}

fn apply_rlimit(typ: &str, soft: u64, hard: u64) -> Result<()> {
    let resource = match typ {
        "RLIMIT_AS" => libc::RLIMIT_AS,
        "RLIMIT_CORE" => libc::RLIMIT_CORE,
        "RLIMIT_CPU" => libc::RLIMIT_CPU,
        "RLIMIT_DATA" => libc::RLIMIT_DATA,
        "RLIMIT_FSIZE" => libc::RLIMIT_FSIZE,
        "RLIMIT_LOCKS" => libc::RLIMIT_LOCKS,
        "RLIMIT_MEMLOCK" => libc::RLIMIT_MEMLOCK,
        "RLIMIT_MSGQUEUE" => libc::RLIMIT_MSGQUEUE,
        "RLIMIT_NICE" => libc::RLIMIT_NICE,
        "RLIMIT_NOFILE" => libc::RLIMIT_NOFILE,
        "RLIMIT_NPROC" => libc::RLIMIT_NPROC,
        "RLIMIT_RSS" => libc::RLIMIT_RSS,
        "RLIMIT_RTPRIO" => libc::RLIMIT_RTPRIO,
        "RLIMIT_RTTIME" => libc::RLIMIT_RTTIME,
        "RLIMIT_SIGPENDING" => libc::RLIMIT_SIGPENDING,
        "RLIMIT_STACK" => libc::RLIMIT_STACK,
        other => anyhow::bail!("unsupported rlimit {other}"),
    };
    let limit = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    if unsafe { libc::setrlimit(resource, &limit) } != 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("setrlimit {typ}"));
    }
    Ok(())
}

fn apply_capabilities(plan: &crate::plan::CapabilityPlan) -> Result<()> {
    let bounding = caps::mask_from_names(&plan.bounding).context("bounding capabilities")?;
    let effective = caps::mask_from_names(&plan.effective).context("effective capabilities")?;
    let permitted = caps::mask_from_names(&plan.permitted).context("permitted capabilities")?;
    let inheritable =
        caps::mask_from_names(&plan.inheritable).context("inheritable capabilities")?;
    let ambient = caps::mask_from_names(&plan.ambient).context("ambient capabilities")?;

    // Drop everything not kept in the bounding set first.
    for bit in 0..41u64 {
        if bounding & (1 << bit) == 0 {
            let ret = unsafe { libc::prctl(libc::PR_CAPBSET_DROP, bit as libc::c_ulong, 0, 0, 0) };
            if ret != 0 {
                let error = std::io::Error::last_os_error();
                // EINVAL means the capability does not exist on this kernel.
                if error.raw_os_error() != Some(libc::EINVAL) {
                    warn!("PR_CAPBSET_DROP {bit} failed: {error}");
                }
            }
        }
    }

    // Ambient capabilities must be cleared before capset drops them.
    if unsafe { libc::prctl(libc::PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0, 0) } != 0 {
        warn!(
            "PR_CAP_AMBIENT_CLEAR_ALL failed: {}",
            std::io::Error::last_os_error()
        );
    }

    // `capset(2)` argument layout (kernel uapi); libc does not expose it.
    #[repr(C)]
    struct CapHeader {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct CapData {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;

    let header = CapHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let mut data = [CapData::default(), CapData::default()];
    data[0].effective = (effective & 0xffff_ffff) as u32;
    data[0].permitted = (permitted & 0xffff_ffff) as u32;
    data[0].inheritable = (inheritable & 0xffff_ffff) as u32;
    data[1].effective = (effective >> 32) as u32;
    data[1].permitted = (permitted >> 32) as u32;
    data[1].inheritable = (inheritable >> 32) as u32;
    if unsafe { libc::syscall(libc::SYS_capset, &header as *const CapHeader, data.as_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error()).context("capset");
    }

    // Raise ambient set last; each raise needs the capability in both the
    // permitted and inheritable sets.
    for bit in 0..41u64 {
        if ambient & (1 << bit) != 0 {
            let ret =
                unsafe { libc::prctl(libc::PR_CAP_AMBIENT_RAISE, bit as libc::c_ulong, 0, 0, 0) };
            if ret != 0 {
                warn!("PR_CAP_AMBIENT_RAISE {bit} failed");
            }
        }
    }
    Ok(())
}

/// Resolve `argv[0]` the way runc does: keep paths as-is, look bare names
/// up in PATH (from the process env), fall back to the name itself.
fn resolve_program(argv0: &std::ffi::CString, env: &[String]) -> std::ffi::CString {
    let raw = argv0.to_string_lossy();
    if raw.contains('/') || raw.is_empty() {
        return argv0.clone();
    }
    let default_path = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
    let path = env
        .iter()
        .find_map(|entry| entry.strip_prefix("PATH="))
        .unwrap_or(default_path);
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let Ok(candidate) = std::ffi::CString::new(format!("{dir}/{raw}")) else {
            continue;
        };
        if unsafe { libc::access(candidate.as_ptr(), libc::X_OK) } == 0 {
            return candidate;
        }
    }
    argv0.clone()
}

fn exec_process(process: &ProcessPlan) -> Result<()> {
    let argv: Vec<std::ffi::CString> = process
        .argv
        .iter()
        .map(|arg| std::ffi::CString::new(arg.as_str()))
        .collect::<std::result::Result<_, _>>()
        .context("argv contains NUL")?;
    let envp: Vec<std::ffi::CString> = process
        .env
        .iter()
        .map(|entry| std::ffi::CString::new(entry.as_str()))
        .collect::<std::result::Result<_, _>>()
        .context("env contains NUL")?;

    let cwd = Path::new(&process.cwd);
    if let Err(error) = std::env::set_current_dir(cwd) {
        fs::create_dir_all(cwd)
            .and_then(|()| std::env::set_current_dir(cwd))
            .with_context(|| format!("enter cwd {} ({error})", cwd.display()))?;
    }

    // OCI runtimes resolve a bare argv[0] against PATH from the process
    // environment (ctr and CRI both send names like "echo").
    let program = resolve_program(&argv[0], &process.env);
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|arg| arg.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());
    let mut envp_ptrs: Vec<*const libc::c_char> = envp.iter().map(|entry| entry.as_ptr()).collect();
    envp_ptrs.push(std::ptr::null());
    debug!(
        "executing process {} in {}",
        program.to_string_lossy(),
        cwd.display()
    );
    if unsafe { libc::execve(program.as_ptr(), argv_ptrs.as_ptr(), envp_ptrs.as_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("execve {}", program.to_string_lossy()));
    }
    Ok(())
}
