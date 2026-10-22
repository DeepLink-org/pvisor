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
/// CLI argument that turns the shim binary into a VM port-bridge proxy:
/// join the pod network namespace, listen on the bridged port, and forward
/// every connection to the VM's vsock-backed unix socket (VM tier only).
#[cfg(feature = "vm")]
pub const INTERNAL_BRIDGE_ARG: &str = "--pvisor-shim-internal-bridge";
/// Bridge proxy: namespace path to join (pod netns).
#[cfg(feature = "vm")]
pub const ENV_BRIDGE_NETNS: &str = "PVISOR_SHIM_BRIDGE_NETNS";
/// Bridge proxy: unix socket tunneled into the guest by libkrun.
#[cfg(feature = "vm")]
pub const ENV_BRIDGE_SOCKET: &str = "PVISOR_SHIM_BRIDGE_SOCKET";
/// Bridge proxy: TCP port to listen on inside the pod netns.
#[cfg(feature = "vm")]
pub const ENV_BRIDGE_PORT: &str = "PVISOR_SHIM_BRIDGE_PORT";
/// Path of the serialized [`crate::plan::SandboxPlan`].
pub const ENV_SANDBOX_PLAN: &str = "PVISOR_SHIM_SANDBOX_PLAN";
/// Pid of the pod sandbox holder; container runners join its namespaces.
pub const ENV_SANDBOX_PID: &str = "PVISOR_SHIM_SANDBOX_PID";
/// Workload stdio descriptors handed over by the shim.
pub const ENV_STDIO_IN: &str = "PVISOR_SHIM_STDIO_IN";
pub const ENV_STDIO_OUT: &str = "PVISOR_SHIM_STDIO_OUT";
pub const ENV_STDIO_ERR: &str = "PVISOR_SHIM_STDIO_ERR";
pub const ENV_STDIO_TERMINAL: &str = "PVISOR_SHIM_STDIO_TERMINAL";
/// VM runner only: descriptor of the guest-side egress stream owned by the
/// shim's `VmNetworkAttachment`. Absent when the task runs without egress.
pub const ENV_VM_NET_FD: &str = "PVISOR_SHIM_VM_NET_FD";
/// Injected-sibling descriptors handed to the internal init parent (see
/// [`SpawnExtras`]).
pub const ENV_INJECT_FDS: &str = "PVISOR_SHIM_INJECT_FDS";

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
    /// Processes blocking on the start pipe (init child plus one sibling
    /// per injected payload). `Start` writes exactly one byte per waiter.
    start_waiters: usize,
}

impl InternalChild {
    /// An internal child with nothing left to release; useful as a
    /// placeholder when moving the real one out of a slot.
    pub fn exited() -> Self {
        InternalChild {
            pid: None,
            start_fd: -1,
            start_waiters: 1,
        }
    }

    /// Signal the children to exec; consumes the start pipe write end.
    pub fn start(&mut self) -> Result<()> {
        let bytes = vec![b's'; self.start_waiters.max(1)];
        let written = unsafe { libc::write(self.start_fd, bytes.as_ptr().cast(), bytes.len()) };
        self.close_start();
        if written < 0 {
            return Err(std::io::Error::last_os_error())
                .context("write start pipe (process may have exited)");
        }
        anyhow::ensure!(
            written as usize == bytes.len(),
            "short write on start pipe: {} of {}",
            written,
            bytes.len()
        );
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

/// Mark a descriptor close-on-exec. Ends the shim keeps for itself (the
/// ready-report read end and the start-pipe write end) must never survive
/// into the internal parent: a long-lived runner holding the start write
/// end would block the EOF that tells it the shim is gone.
fn set_cloexec(fd: RawFd) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
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

/// Optional descriptors handed to an internal parent beyond the plan file
/// and the workload stdio. Field presence has meaning beyond the
/// descriptors themselves: injection descriptors also set the number of
/// processes waiting on the start pipe.
#[derive(Default)]
pub struct SpawnExtras {
    /// VM runner only: descriptor of the guest-side egress stream owned by
    /// the shim's `VmNetworkAttachment`. Absent when the task runs without
    /// egress.
    pub vm_net_fd: Option<i32>,
    /// Injected-sibling descriptors (`[null, log_0, log_1, ...]`), one per
    /// payload plus the leading null descriptor. The list length is also
    /// the number of start-gate waiters (init child plus siblings), so
    /// `Start` writes one byte per entry and every waiter wakes.
    pub inject_fds: Option<Vec<RawFd>>,
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
    inject_fds: Option<Vec<RawFd>>,
) -> Result<InternalChild> {
    spawn_internal_opts(
        arg,
        plan_path,
        plan_bytes,
        stdio,
        sandbox_pid,
        true,
        SpawnExtras {
            vm_net_fd: None,
            inject_fds,
        },
    )
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
    extras: SpawnExtras,
) -> Result<InternalChild> {
    fs::write(plan_path, plan_bytes).with_context(|| format!("write {}", plan_path.display()))?;

    let (ready_r, ready_w) = make_pipe()?;
    let (start_r, start_w) = make_pipe()?;
    let (fork_r, fork_w) = make_pipe()?;
    // One start byte per blocked process: the init child plus one injected
    // sibling per descriptor handed over (null descriptor included).
    let start_waiters = extras.inject_fds.as_ref().map_or(1, Vec::len);
    // Parent-only ends. The exec'd internal parent must inherit exactly the
    // env-documented descriptors (ready_w, start_r, fork_w, fork_r); keeping
    // the start write end would silence the shim-exit EOF for VM runners,
    // and the ready read end would let the child consume its own report.
    set_cloexec(ready_r);
    set_cloexec(start_w);

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
    if let Some(fd) = extras.vm_net_fd {
        command.env(ENV_VM_NET_FD, fd.to_string());
    }
    if let Some(fds) = &extras.inject_fds {
        let rendered = fds
            .iter()
            .map(|fd| fd.to_string())
            .collect::<Vec<_>>()
            .join(",");
        command.env(ENV_INJECT_FDS, rendered);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .context("spawn internal process")?;

    // Drop this process's copies of the child-side ends (CLOEXEC already
    // kept the parent-only ends out of the child).
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
            start_waiters,
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
    #[cfg(feature = "vm")]
    if args.iter().any(|arg| arg == INTERNAL_BRIDGE_ARG) {
        return bridge_proxy_main().map(|()| true);
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
        join_sandbox_namespaces(&plan, sandbox_pid)?;
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

    // The init child forks FIRST: A unshared the pid namespace, so its first
    // child is PID 1 of the container — that must be the workload (native
    // PID 1 contract; a PID 1 sibling exit would zap the whole namespace).
    // Injected payload siblings fork afterwards as PID 2+: each repeats the
    // namespace and pivot setup in its own mount-namespace copy and blocks
    // on the shared start pipe. A fork failure kills the earlier children
    // and fails Create: an explicitly requested payload must never be
    // silently skipped.
    let g_pid = unsafe { libc::fork() };
    if g_pid < 0 {
        return Err(std::io::Error::last_os_error()).context("fork init process");
    }
    if g_pid == 0 {
        init_process_main(&plan, start_fd, fork_report_fd);
    }
    let g_pid = g_pid as u32;

    let sibling_pids = match fork_injection_siblings(&plan, start_fd) {
        Ok(pids) => pids,
        Err(error) => {
            unsafe {
                libc::kill(g_pid as i32, libc::SIGKILL);
                libc::waitpid(g_pid as i32, std::ptr::null_mut(), 0);
            }
            return Err(error);
        }
    };

    // A no longer needs the start pipe or the report write end (G and the
    // siblings hold the only remaining copies).
    unsafe { libc::close(start_fd) };
    unsafe { libc::close(fork_report_fd) };

    // Cgroup bookkeeping with host paths still visible.
    if let Some(cgroup_plan) = plan.cgroup.as_ref() {
        if let Err(error) = attach_cgroup(cgroup_plan, g_pid) {
            kill_children(&sibling_pids);
            unsafe {
                libc::kill(g_pid as i32, libc::SIGKILL);
                libc::waitpid(g_pid as i32, std::ptr::null_mut(), 0);
            }
            return Err(error).context("install requested cgroup limits");
        }
        for pid in &sibling_pids {
            if let Err(error) = attach_cgroup(cgroup_plan, *pid) {
                // ESRCH: the sibling failed its own setup and exited; its
                // failure is on its log descriptor (advisory, never fatal).
                // Any other cgroup error is a real enforcement problem.
                let gone = error
                    .chain()
                    .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
                    .any(|io_error| io_error.raw_os_error() == Some(libc::ESRCH));
                if gone {
                    eprintln!(
                        "pvisor shim: injected sibling {pid} exited before cgroup attach: {error:#}"
                    );
                } else {
                    kill_children(&sibling_pids);
                    unsafe {
                        libc::kill(g_pid as i32, libc::SIGKILL);
                        libc::waitpid(g_pid as i32, std::ptr::null_mut(), 0);
                    }
                    return Err(error).context("attach injected sibling to cgroup");
                }
            }
        }
    }

    // Device rules need a cgroup for the BPF filter; without one runc
    // semantics apply and enforcement is skipped.
    if !plan.devices.is_empty()
        && let Some(cgroup_plan) = plan.cgroup.as_ref()
        && let Some(dir) = crate::cgroup::cgroup_dir(cgroup_plan)
        && let Err(error) = crate::cgroup::device_bpf::attach_device_filter(
            &dir,
            &crate::plan::with_default_devices(&plan.devices),
        )
    {
        kill_children(&sibling_pids);
        unsafe {
            libc::kill(g_pid as i32, libc::SIGKILL);
            libc::waitpid(g_pid as i32, std::ptr::null_mut(), 0);
        }
        return Err(error).context("attach device filter");
    }

    relay_fork_report(fork_report_fd, g_pid)
}

/// Kill and reap the injected sibling processes (Create-time failure path).
fn kill_children(pids: &[u32]) {
    for pid in pids {
        unsafe {
            libc::kill(*pid as i32, libc::SIGKILL);
            libc::waitpid(*pid as i32, std::ptr::null_mut(), 0);
        }
    }
}

/// Parse the injection descriptor list from the environment. The descriptor
/// order matches the plan's injection order after the leading null entry.
fn inject_fds_from_env(count: usize) -> Result<Vec<RawFd>> {
    let raw =
        std::env::var(ENV_INJECT_FDS).context("injections without handed-over descriptors")?;
    let fds = raw
        .split(',')
        .map(|entry| entry.trim().parse::<RawFd>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("parse injection descriptors")?;
    anyhow::ensure!(
        fds.len() == count + 1,
        "expected {} injection descriptors (null plus one per payload), got {}",
        count + 1,
        fds.len()
    );
    Ok(fds)
}

/// Fork one sibling per RUNNING payload (stage-only payloads never spawn);
/// child processes never return.
fn fork_injection_siblings(plan: &ContainerPlan, start_fd: RawFd) -> Result<Vec<u32>> {
    let mut pids = Vec::new();
    let running: Vec<usize> = plan
        .injections
        .iter()
        .enumerate()
        .filter(|(_, injection)| injection.run)
        .map(|(index, _)| index)
        .collect();
    if running.is_empty() {
        return Ok(pids);
    }
    let fds = inject_fds_from_env(running.len())?;
    for (log_index, index) in running.into_iter().enumerate() {
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            let error = std::io::Error::last_os_error();
            kill_children(&pids);
            return Err(error).context("fork injected sibling");
        }
        if pid == 0 {
            injected_sibling_main(plan, index, start_fd, &fds, log_index);
        }
        pids.push(pid as u32);
    }
    // A keeps no descriptor ends; the children hold the surviving copies.
    for fd in &fds {
        unsafe { libc::close(*fd) };
    }
    Ok(pids)
}

/// One injected payload process: repeat the init child's namespace/pivot
/// dance in a private mount-namespace copy, wire stdio to the null and log
/// descriptors (the workload's FIFOs stay untouched), wait for Start on the
/// shared pipe, then apply the workload identity and exec the payload argv.
///
/// Advisory by contract: after the start gate this process is never fatal
/// to the task. The task exit status is always the init child's. Failures
/// before stdio is rewired reach the shim journal (inherited stderr) and
/// the payload's log file; later ones only the log file.
fn injected_sibling_main(
    plan: &ContainerPlan,
    index: usize,
    start_fd: RawFd,
    fds: &[RawFd],
    log_index: usize,
) -> ! {
    let injection = &plan.injections[index];
    let null_fd = fds[0];
    let log_fd = fds[1 + log_index];
    let note = |message: &str| {
        let line = format!("pvisor injected sibling {}: {message}\n", injection.name);
        eprint!("{line}");
        unsafe {
            libc::write(log_fd, line.as_ptr().cast(), line.len());
        }
    };
    let fail = |message: String| -> ! {
        note(&message);
        std::process::exit(125);
    };

    close_inherited_fds(&[start_fd, null_fd, log_fd]);

    if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
        fail(format!(
            "unshare mount namespace: {}",
            std::io::Error::last_os_error()
        ));
    }
    if let Err(error) = mount::make_mounts_private() {
        fail(format!("{error:#}"));
    }
    if plan
        .namespaces
        .iter()
        .any(|ns| ns.kind == NamespaceKind::Cgroup && ns.path.is_none())
        && unsafe { libc::unshare(libc::CLONE_NEWCGROUP) } != 0
    {
        fail(format!(
            "unshare cgroup namespace: {}",
            std::io::Error::last_os_error()
        ));
    }
    if let Err(error) = mount::mount_proc(&plan.rootfs) {
        fail(format!("{error:#}"));
    }
    if let Err(error) = mount::pivot_root(&plan.rootfs) {
        fail(format!("{error:#}"));
    }
    // CRI mounts a fresh empty tmpfs over /dev; runc always provisions the
    // minimal device nodes there, and workload supervisors (OpenSandbox's
    // egress among them) open /dev/null during startup. Provisioning is a
    // no-op when the image already provides the nodes.
    if let Err(error) = mount::ensure_minimal_dev_nodes() {
        fail(format!("provision /dev nodes failed: {error:#}"));
    }
    if let Err(error) = mount::apply_masked_paths(&plan.masked_paths) {
        fail(format!("masked paths failed: {error:#}"));
    }
    if let Err(error) = mount::apply_readonly_paths(&plan.readonly_paths) {
        fail(format!("read-only paths failed: {error:#}"));
    }
    if let Err(error) = mount::apply_sysctls(&plan.sysctls) {
        fail(format!("sysctls failed: {error:#}"));
    }
    if plan.root_readonly
        && let Err(error) = mount::remount_root_readonly()
    {
        fail(format!("root read-only remount failed: {error:#}"));
    }

    // stdin/stdout detach to /dev/null, stderr to the host-side log file;
    // the workload's task IO is never shared with an injected process.
    unsafe {
        libc::dup2(null_fd, libc::STDIN_FILENO);
        libc::dup2(null_fd, libc::STDOUT_FILENO);
        libc::dup2(log_fd, libc::STDERR_FILENO);
    }
    if let Err(error) = std::env::set_current_dir("/") {
        fail(format!("chdir /: {error}"));
    }

    // Same gate and EOF semantics as the init child: one byte per waiter
    // arrives with Start; EOF means the shim gave up on this task.
    let mut byte = [0u8; 1];
    let read = unsafe { libc::read(start_fd, byte.as_mut_ptr().cast(), 1) };
    unsafe { libc::close(start_fd) };
    if read <= 0 {
        std::process::exit(255);
    }

    // The payload runs with the workload's identity (uid/gid, groups,
    // capabilities, rlimits): the shim knows nothing about payload needs,
    // and "same as the workload" is the one consistent choice.
    if let Err(error) = apply_process_identity(&plan.process) {
        fail(format!("identity failed: {error:#}"));
    }

    let argv: Vec<std::ffi::CString> = injection
        .argv
        .iter()
        .map(|arg| std::ffi::CString::new(arg.as_str()))
        .collect::<std::result::Result<_, _>>()
        .unwrap_or_else(|_| fail("payload argv contains NUL".into()));
    let env = crate::inject::merged_env(&plan.process.env, &injection.env);
    let envp: Vec<std::ffi::CString> = env
        .iter()
        .map(|entry| std::ffi::CString::new(entry.as_str()))
        .collect::<std::result::Result<_, _>>()
        .unwrap_or_else(|_| fail("payload environment contains NUL".into()));
    let program = resolve_program(&argv[0], &env);
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|arg| arg.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());
    let mut envp_ptrs: Vec<*const libc::c_char> = envp.iter().map(|entry| entry.as_ptr()).collect();
    envp_ptrs.push(std::ptr::null());
    if unsafe { libc::execve(program.as_ptr(), argv_ptrs.as_ptr(), envp_ptrs.as_ptr()) } < 0 {
        fail(format!(
            "execve {}: {}",
            program.to_string_lossy(),
            std::io::Error::last_os_error()
        ));
    }
    unreachable!("exec never returns on success")
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

    // VM port bridges come up before the VM boots: the pod-network listener
    // must answer readiness probes the moment the guest service does.
    // A missing network namespace (plain ctr runs) rejects the bridge
    // instead of silently listening on the host network.
    spawn_bridge_proxies(&plan)?;

    // Task stdio travels the named console ports bound to the task's own
    // descriptors (passed to boot_vm explicitly), so this process's stderr
    // is free for VMM diagnostics: the `io.pvisor.vm.log` annotation (a
    // log filter or "1") redirects it to <bundle>/pvisor-vm.log.
    let stdio = StdioFds::from_env()?;
    unsafe {
        libc::dup2(stdio.stdin, libc::STDIN_FILENO);
        libc::dup2(stdio.stdout, libc::STDOUT_FILENO);
        libc::dup2(stdio.stderr, libc::STDERR_FILENO);
    }
    if let Some(filter) = plan.annotations.get("io.pvisor.vm.log").map(|value| {
        if value.is_empty() || value == "1" {
            "debug".to_string()
        } else {
            value.clone()
        }
    }) {
        let path = plan.bundle.join("pvisor-vm.log");
        if let Ok(file) = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
        {
            use std::os::fd::AsRawFd;
            unsafe {
                libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO);
            }
            use pvisor_vm::api::RuntimeSupport;
            pvisor_vm::api::VmPlatform::init_logging(&filter);
            eprintln!("pvisor VM runner logging to {}", path.display());
        }
    }

    // Egress: gated tasks wait for the OpenSandbox sidecar's policy and
    // compile the overlaynet attachment here (the shim cannot — the sidecar
    // container is created only after the workload starts, and blocking
    // Create would deadlock kubelet). Everything else uses the
    // shim-started attachment inherited via ENV_VM_NET_FD.
    #[cfg(feature = "vm")]
    let gated_attachment = match plan.egress_gate.as_ref() {
        Some(gate) => match wait_for_egress_rules(gate, std::time::Duration::from_secs(3)) {
            Ok(Some(rules)) => {
                match crate::network::VmNetworkAttachment::from_egress_rules(&rules, &plan.id) {
                    Ok(attachment) => Some(attachment),
                    Err(error) => {
                        eprintln!(
                            "pvisor shim: egress policy compile failed; \
                             booting without network (deny all): {error:#}"
                        );
                        None
                    }
                }
            }
            Ok(None) => {
                eprintln!(
                    "pvisor shim: egress policy did not arrive before the deadline; \
                     booting without network (deny all)"
                );
                None
            }
            Err(error) => {
                eprintln!("pvisor shim: egress gate unreadable: {error:#}");
                None
            }
        },
        None => None,
    };
    #[cfg(feature = "vm")]
    let network = if let Some(attachment) = gated_attachment.as_ref() {
        attachment.guest_stream().ok()
    } else if plan.vm_network_plan().map_err(anyhow::Error::msg)?.mode
        != crate::plan::VmNetworkMode::Off
    {
        let fd = fd_from_env(ENV_VM_NET_FD)?;
        Some(unsafe { crate::network::stream_from_fd(fd) })
    } else {
        None
    };

    // The task's stdio descriptors reach the VMM explicitly, so redirecting
    // the runner's stderr to the vm.log file below cannot capture workload
    // output anymore.
    let stdio = StdioFds::from_env()
        .ok()
        .map(|fds| (fds.stdin, fds.stdout, fds.stderr));

    let result = crate::vm::boot_vm(&plan, network, stdio);
    // Belt and braces next to the delete-path unlink: a task whose delete
    // never arrives (shim crash, force teardown) still leaves no bridge
    // sockets behind once its runner exits.
    for injection in &plan.injections {
        if let Some(port) = injection.bridge {
            let _ = std::fs::remove_file(crate::agent::bridge_socket_path(&plan.id, port));
        }
    }
    if let Some(gate) = &plan.egress_gate {
        let _ = std::fs::remove_file(gate);
    }
    result
}

/// Poll the egress gate file until the sidecar's policy arrives.
/// `Ok(None)` on timeout — callers boot deny-all in that case.
#[cfg(feature = "vm")]
fn wait_for_egress_rules(
    gate: &Path,
    timeout: std::time::Duration,
) -> Result<Option<crate::network::EgressRules>> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match std::fs::read_to_string(gate) {
            Ok(contents) => {
                let rules: crate::network::EgressRules =
                    serde_json::from_str(&contents).context("parse egress rules")?;
                return Ok(Some(rules));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if std::time::Instant::now() >= deadline {
                    return Ok(None);
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(error) => return Err(error).with_context(|| format!("read {}", gate.display())),
        }
    }
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

    // Join the init's host cgroup before entering its mount namespace.
    let membership = fs::read_to_string(format!("/proc/{}/cgroup", plan.init_pid))
        .context("read init cgroup")?;
    let path = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .context("init has no unified cgroup")?;
    let relative = std::path::Path::new(path.trim_start_matches('/'));
    anyhow::ensure!(
        relative
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_))),
        "invalid init cgroup path"
    );
    fs::write(
        std::path::Path::new(cgroup::UNIFIED_MOUNT)
            .join(relative)
            .join("cgroup.procs"),
        std::process::id().to_string(),
    )
    .context("join init cgroup")?;
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
/// unconfigured. Failure aborts startup rather than retaining host membership.
fn join_sandbox_namespaces(plan: &ContainerPlan, sandbox_pid: u32) -> Result<()> {
    for kind in SANDBOX_SHARED_KINDS {
        if plan.has_namespace(kind) {
            continue;
        }
        let path = format!("/proc/{sandbox_pid}/ns/{}", kind.proc_ns_name());
        setns_by_path(&path, kind)?;
    }
    Ok(())
}

fn collect_unshare_flags(plan: &ContainerPlan) -> libc::c_int {
    let mut flags = if plan
        .namespaces
        .iter()
        .any(|namespace| namespace.kind == NamespaceKind::Mount && namespace.path.is_some())
    {
        0
    } else {
        libc::CLONE_NEWNS
    };
    for namespace in &plan.namespaces {
        if namespace.path.is_some() {
            continue;
        }
        match namespace.kind {
            // CLONE_NEWNS is always part of the unshare call above.
            NamespaceKind::Mount => {}
            // The cgroup namespace is unshared by the init child (G): the
            // parent must keep the host cgroup view to install limits and
            // the device filter.
            NamespaceKind::Cgroup => {}
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
///
/// Every descriptor is opened while still in the host mount namespace:
/// after joining the container's mounts, `/proc` is the container's own
/// procfs and host-pid (and non-member `/proc/self`) paths stop resolving.
fn join_init_namespaces(init_pid: u32) -> Result<()> {
    let order = [
        NamespaceKind::User,
        NamespaceKind::Ipc,
        NamespaceKind::Uts,
        NamespaceKind::Network,
        NamespaceKind::Mount,
        NamespaceKind::Pid,
    ];
    struct OpenNs {
        kind: NamespaceKind,
        target_fd: RawFd,
        self_fd: RawFd,
    }
    let mut opened = Vec::new();
    for kind in order {
        let name = kind.proc_ns_name();
        let path = format!("/proc/{init_pid}/ns/{name}");
        let target_fd = open_namespace(&path, name)?;
        let self_fd = open_namespace(&format!("/proc/self/ns/{name}"), name)?;
        opened.push(OpenNs {
            kind,
            target_fd,
            self_fd,
        });
    }
    let mut result = Ok(());
    for ns in &opened {
        if let Err(error) = setns_opened(ns.target_fd, ns.self_fd, ns.kind) {
            result = Err(error);
            break;
        }
    }
    for ns in &opened {
        unsafe {
            libc::close(ns.target_fd);
            libc::close(ns.self_fd);
        }
    }
    result
}

/// Open a namespace symlink as a descriptor for a later `setns`.
fn open_namespace(path: &str, name: &str) -> Result<RawFd> {
    let cpath =
        std::ffi::CString::new(path).with_context(|| format!("namespace path {path} has NUL"))?;
    let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("open {name} namespace at {path}"));
    }
    Ok(fd)
}

/// `setns` on an already-opened descriptor, comparing identity against the
/// process's own namespace first: joining the current user namespace is
/// rejected by Linux and must not hide failures for other kinds.
fn setns_opened(target_fd: RawFd, self_fd: RawFd, kind: NamespaceKind) -> Result<()> {
    let name = kind.proc_ns_name();
    if ns_identity(target_fd).is_some_and(|target| ns_identity(self_fd) == Some(target)) {
        return Ok(());
    }
    let ret = unsafe { libc::setns(target_fd, kind.clone_flag()) };
    if ret != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("setns {name} namespace"));
    }
    Ok(())
}

/// Namespace inode identity of an open descriptor (`(st_dev, st_ino)`),
/// compared without touching `/proc`: after joining foreign mounts the
/// procfs of the process's own pid namespace no longer resolves.
fn ns_identity(fd: RawFd) -> Option<(u64, u64)> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut stat) } != 0 {
        return None;
    }
    Some((stat.st_dev, stat.st_ino))
}

fn setns_by_path(path: &str, kind: NamespaceKind) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let name = kind.proc_ns_name();
    let current = fs::metadata(format!("/proc/self/ns/{name}"))?;
    let target = fs::metadata(path)?;
    // Joining the current user namespace is rejected by Linux; it already
    // satisfies the request and must not hide failures for other namespaces.
    if current.dev() == target.dev() && current.ino() == target.ino() {
        return Ok(());
    }
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
        anyhow::ensure!(
            cgroup::control_files(plan).is_empty(),
            "cgroup limits require a cgroup path"
        );
        return Ok(());
    };
    anyhow::ensure!(
        dir.strip_prefix(cgroup::UNIFIED_MOUNT)?
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_))),
        "invalid cgroup path"
    );
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    // Controller files only exist in a child once the parent enables the
    // controllers (runc does this through the systemd driver; standalone we
    // write the parent's subtree_control directly). Without it, writes to
    // cpu.max and friends fail: the files are absent.
    let controllers: Vec<&str> = cgroup::control_files(plan)
        .iter()
        .map(|(file, _)| file.split('.').next().unwrap_or(file))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if !controllers.is_empty() {
        let parent = dir.parent().context("cgroup path has no parent")?;
        fs::write(
            parent.join("cgroup.subtree_control"),
            controllers
                .iter()
                .map(|name| format!("+{name}"))
                .collect::<Vec<_>>()
                .join(" "),
        )
        .with_context(|| format!("enable controllers on {}", parent.display()))?;
    }
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
    if plan.synthetic {
        synthetic_init_main(start_fd, fork_report_fd);
    }
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

    // G needs its own mount namespace: pivot_root must not change the root
    // for A (which shares this namespace after fork), or the parent's
    // cgroup and BPF setup loses the host view.
    if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
        fail(format!(
            "unshare mount namespace: {}",
            std::io::Error::last_os_error()
        ));
    }
    if let Err(error) = mount::make_mounts_private() {
        fail(format!("{error:#}"));
    }

    // The cgroup namespace is unshared here (after the parent installed
    // limits in the host view): the remapped root only affects this
    // process's own view.
    if plan
        .namespaces
        .iter()
        .any(|ns| ns.kind == NamespaceKind::Cgroup && ns.path.is_none())
        && unsafe { libc::unshare(libc::CLONE_NEWCGROUP) } != 0
    {
        fail(format!(
            "unshare cgroup namespace: {}",
            std::io::Error::last_os_error()
        ));
    }

    // procfs must be mounted by a member of the new pid namespace, i.e. G.
    if let Err(error) = mount::mount_proc(&plan.rootfs) {
        fail(format!("{error:#}"));
    }
    if let Err(error) = mount::pivot_root(&plan.rootfs) {
        fail(format!("{error:#}"));
    }
    // CRI mounts a fresh empty tmpfs over /dev; runc always provisions the
    // minimal device nodes there, and workload supervisors (OpenSandbox's
    // egress among them) open /dev/null during startup. Provisioning is a
    // no-op when the image already provides the nodes.
    if let Err(error) = mount::ensure_minimal_dev_nodes() {
        fail(format!("provision /dev nodes failed: {error:#}"));
    }
    if let Err(error) = mount::apply_masked_paths(&plan.masked_paths) {
        fail(format!("masked paths failed: {error:#}"));
    }
    if let Err(error) = mount::apply_readonly_paths(&plan.readonly_paths) {
        fail(format!("read-only paths failed: {error:#}"));
    }
    if let Err(error) = mount::apply_sysctls(&plan.sysctls) {
        fail(format!("sysctls failed: {error:#}"));
    }
    if plan.root_readonly
        && let Err(error) = mount::remount_root_readonly()
    {
        fail(format!("root read-only remount failed: {error:#}"));
    }

    finish_process(&plan.process, stdio, start_fd, fork_report_fd)
}

/// Synthetic task: report readiness, wait for Start, exit 0 — nothing
/// executes. The task's purpose (the execd-installer's emptyDir copy) is
/// already served by injection, so this only satisfies the container
/// lifecycle the orchestrator watches. Never returns.
fn synthetic_init_main(start_fd: RawFd, fork_report_fd: RawFd) -> ! {
    {
        let message = InitReport::Ok {
            pid: std::process::id(),
        };
        let mut file = unsafe { fs::File::from_raw_fd(fork_report_fd) };
        let _ = file.write_all(&message.line().unwrap_or_default());
        let _ = file.flush();
    }
    // Same gate semantics as any task: one byte arrives with Start, EOF
    // means the shim gave up. Either way this task exits 0 — there is
    // nothing to interrupt.
    let mut byte = [0u8; 1];
    let _ = unsafe { libc::read(start_fd, byte.as_mut_ptr().cast(), 1) };
    unsafe { libc::close(start_fd) };
    std::process::exit(0);
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

    // Applied while still root: lowering the score (negative values)
    // needs CAP_SYS_RESOURCE, which a uid switch would drop.
    if let Some(adj) = process.oom_score_adj {
        fs::write("/proc/self/oom_score_adj", adj.to_string())
            .with_context(|| format!("write oom_score_adj {adj}"))?;
    }

    // Rootless user namespaces explicitly disable setgroups before writing
    // gid_map. They cannot request supplementary groups; do not mistake that
    // kernel restriction for a failed identity transition.
    let setgroups_denied =
        fs::read_to_string("/proc/self/setgroups").is_ok_and(|value| value.trim() == "deny");
    anyhow::ensure!(
        !setgroups_denied || user.additional_gids.is_empty(),
        "supplementary groups are disabled in this user namespace"
    );
    // Bounding-set changes need CAP_SETPCAP, before changing uid can drop it.
    drop_bounding_capabilities(&process.capabilities)?;
    // SAFETY: identity syscalls in the forked, single-threaded child.
    unsafe {
        if !setgroups_denied
            && libc::setgroups(user.additional_gids.len(), user.additional_gids.as_ptr()) != 0
        {
            return Err(std::io::Error::last_os_error()).context("setgroups");
        }
        if libc::setresgid(user.gid, user.gid, user.gid) != 0 {
            return Err(std::io::Error::last_os_error()).context("setresgid");
        }
        if libc::setresuid(user.uid, user.uid, user.uid) != 0 {
            return Err(std::io::Error::last_os_error()).context("setresuid");
        }
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

fn drop_bounding_capabilities(plan: &crate::plan::CapabilityPlan) -> Result<()> {
    let bounding = caps::mask_from_names(&plan.bounding).context("bounding capabilities")?;
    // Include kernel capabilities beyond the named ones in our current table.
    for bit in 0..64u64 {
        if bounding & (1 << bit) != 0 {
            continue;
        }
        let present = unsafe { libc::prctl(libc::PR_CAPBSET_READ, bit as libc::c_ulong, 0, 0, 0) };
        if present == 0 {
            continue;
        }
        if present < 0 {
            let error = std::io::Error::last_os_error();
            // EINVAL means the capability does not exist on this kernel.
            if error.raw_os_error() == Some(libc::EINVAL) {
                continue;
            }
            return Err(error).with_context(|| format!("PR_CAPBSET_READ {bit}"));
        }
        if unsafe { libc::prctl(libc::PR_CAPBSET_DROP, bit as libc::c_ulong, 0, 0, 0) } != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("PR_CAPBSET_DROP {bit}"));
        }
    }
    Ok(())
}

fn apply_capabilities(plan: &crate::plan::CapabilityPlan) -> Result<()> {
    let effective = caps::mask_from_names(&plan.effective).context("effective capabilities")?;
    let permitted = caps::mask_from_names(&plan.permitted).context("permitted capabilities")?;
    let inheritable =
        caps::mask_from_names(&plan.inheritable).context("inheritable capabilities")?;
    let ambient = caps::mask_from_names(&plan.ambient).context("ambient capabilities")?;

    // Ambient capabilities must be cleared before capset drops them.
    if unsafe {
        libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL,
            0,
            0,
            0,
        )
    } != 0
    {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINVAL) || ambient != 0 {
            return Err(error).context("clear ambient capabilities");
        }
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
            let ret = unsafe {
                libc::prctl(
                    libc::PR_CAP_AMBIENT,
                    libc::PR_CAP_AMBIENT_RAISE,
                    bit as libc::c_ulong,
                    0,
                    0,
                )
            };
            if ret != 0 {
                return Err(std::io::Error::last_os_error())
                    .context(format!("raise ambient capability {bit}"));
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

/// Spawn one detached bridge proxy per VM payload bridge. The proxies are
/// re-execs of this binary (see [`INTERNAL_BRIDGE_ARG`]) that join the task's
/// network namespace; they die with the runner via `PR_SET_PDEATHSIG`.
#[cfg(feature = "vm")]
fn spawn_bridge_proxies(plan: &ContainerPlan) -> Result<()> {
    let bridges: Vec<u16> = plan
        .injections
        .iter()
        .filter_map(|injection| injection.bridge)
        .collect();
    if bridges.is_empty() {
        return Ok(());
    }
    let netns = plan
        .namespaces
        .iter()
        .find(|ns| ns.kind == NamespaceKind::Network)
        .and_then(|ns| ns.path.clone())
        .context("VM port bridges need a network namespace to join (CRI pods provide one)")?;
    for port in bridges {
        let socket = crate::agent::bridge_socket_path(&plan.id, port);
        let exe = std::env::current_exe().context("current exe")?;
        let mut command = Command::new(exe);
        command
            .arg(INTERNAL_BRIDGE_ARG)
            .env(ENV_BRIDGE_NETNS, &netns)
            .env(ENV_BRIDGE_SOCKET, &socket)
            .env(ENV_BRIDGE_PORT, port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
            .spawn()
            .with_context(|| format!("spawn bridge proxy for port {port}"))?;
    }
    Ok(())
}

/// Bridge proxy entry: join the pod netns, listen on the bridged port, and
/// pump every TCP connection to the vsock-backed unix socket. Never returns
/// while the parent (VM runner) lives; `PR_SET_PDEATHSIG` reaps it after.
#[cfg(feature = "vm")]
fn bridge_proxy_main() -> Result<()> {
    let netns = std::env::var(ENV_BRIDGE_NETNS).context("bridge netns")?;
    let socket = std::env::var(ENV_BRIDGE_SOCKET).context("bridge socket")?;
    let port: u16 = std::env::var(ENV_BRIDGE_PORT)
        .context("bridge port")?
        .parse()
        .context("parse bridge port")?;

    unsafe {
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
            return Err(std::io::Error::last_os_error()).context("PR_SET_PDEATHSIG");
        }
    }
    setns_by_path(&netns, NamespaceKind::Network).context("join pod network namespace")?;

    // Init-container tasks in the same pod bridge the same port before the
    // main container starts; retry until their proxy exits instead of dying
    // on the transient EADDRINUSE.
    let listener = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match std::net::TcpListener::bind(("0.0.0.0", port)) {
                Ok(listener) => break listener,
                Err(error)
                    if error.kind() == std::io::ErrorKind::AddrInUse
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("bridge listen 0.0.0.0:{port}"));
                }
            }
        }
    };
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("bridge accept"),
        };
        let socket = socket.clone();
        std::thread::spawn(move || {
            if let Err(error) = pump_bridge(stream, &socket) {
                log::debug!("bridge connection ended: {error:#}");
            }
        });
    }
    Ok(())
}

/// Copy bytes both ways between one accepted TCP connection and the
/// vsock-backed unix socket: a single unix connection is a single guest
/// vsock connection, so both directions must share it.
#[cfg(feature = "vm")]
fn pump_bridge(stream: std::net::TcpStream, socket: &str) -> Result<()> {
    use std::net::Shutdown;

    let mut unix = std::os::unix::net::UnixStream::connect(socket)
        .with_context(|| format!("bridge connect {socket}"))?;
    let mut unix_write = unix.try_clone().context("clone bridge unix stream")?;
    let mut tcp_write = stream.try_clone().context("clone bridge tcp stream")?;
    let _ = stream.set_nodelay(true);
    let up = std::thread::spawn(move || {
        // tcp → unix (request direction); half-close afterwards so guest
        // protocols that rely on EOF terminate promptly.
        let mut stream = stream;
        let _ = std::io::copy(&mut stream, &mut unix_write);
        let _ = unix_write.shutdown(Shutdown::Write);
    });
    // unix → tcp (response direction).
    let _ = std::io::copy(&mut unix, &mut tcp_write);
    let _ = up.join();
    let _ = tcp_write.shutdown(Shutdown::Both);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{IoPlan, NamespacePlan};

    #[test]
    fn start_writes_one_byte_per_waiter() {
        // Injected siblings share the init child's start pipe; every waiter
        // consumes exactly one byte, so Start must write one per waiter and
        // closing the write end must still deliver EOF to the rest.
        let mut fds = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let mut child = InternalChild {
            pid: None,
            start_fd: fds[1],
            start_waiters: 3,
        };
        child.start().unwrap();
        let mut buf = [0u8; 8];
        let read = unsafe { libc::read(fds[0], buf.as_mut_ptr().cast(), 8) };
        assert_eq!(read, 3, "one byte per waiter");
        assert!(buf[..3].iter().all(|byte| *byte == b's'));
        // The write end is consumed; a second read reports EOF.
        let read = unsafe { libc::read(fds[0], buf.as_mut_ptr().cast(), 8) };
        assert_eq!(read, 0);
        unsafe { libc::close(fds[0]) };
    }

    #[test]
    fn requested_limits_cannot_succeed_without_a_cgroup_path() {
        let plan = CgroupPlan {
            path: None,
            pids_max: Some("1".into()),
            ..Default::default()
        };
        assert!(attach_cgroup(&plan, 1).is_err());
    }

    #[test]
    fn joining_a_mount_namespace_does_not_immediately_unshare_it() {
        let spec = serde_json::from_str(r#"{"ociVersion":"1.0.2","process":{"args":["true"],"cwd":"/","user":{"uid":0,"gid":0}}}"#).unwrap();
        let mut plan = crate::plan::build_plan(
            &spec,
            "test",
            Path::new("/bundle"),
            Vec::new(),
            IoPlan::default(),
        )
        .unwrap();
        assert_ne!(collect_unshare_flags(&plan) & libc::CLONE_NEWNS, 0);
        plan.namespaces.push(NamespacePlan {
            kind: NamespaceKind::Mount,
            path: Some("/proc/self/ns/mnt".into()),
        });
        assert_eq!(collect_unshare_flags(&plan) & libc::CLONE_NEWNS, 0);
    }

    #[test]
    fn same_namespace_is_satisfied_but_missing_namespace_fails() {
        setns_by_path("/proc/self/ns/user", NamespaceKind::User).unwrap();
        assert!(join_init_namespaces(u32::MAX).is_err());
    }

    #[test]
    fn failed_identity_syscall_is_not_ignored() {
        const CHILD_KEY: &str = "PVISOR_TEST_IDENTITY_CHILD";
        if std::env::var_os(CHILD_KEY).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "child::tests::failed_identity_syscall_is_not_ignored",
                    "--nocapture",
                ])
                .env(CHILD_KEY, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        // Isolate capability changes in a subprocess, even when tests run as root.
        apply_capabilities(&crate::plan::CapabilityPlan::default()).unwrap();
        let has_bounding_caps = (0..64).any(|bit| unsafe {
            libc::prctl(libc::PR_CAPBSET_READ, bit as libc::c_ulong, 0, 0, 0) == 1
        });
        let dropped = drop_bounding_capabilities(&crate::plan::CapabilityPlan::default());
        if has_bounding_caps {
            // Effective CAP_SETPCAP is now gone, even if the test started as root.
            assert!(dropped.unwrap_err().to_string().contains("PR_CAPBSET_DROP"));
        } else {
            // Already-absent capabilities need no privileged drop syscall.
            dropped.unwrap();
        }
        let spec = serde_json::from_str(r#"{"ociVersion":"1.0.2","process":{"args":["true"],"cwd":"/","user":{"uid":0,"gid":0}}}"#).unwrap();
        let mut plan = crate::plan::build_plan(
            &spec,
            "test",
            Path::new("/bundle"),
            Vec::new(),
            IoPlan::default(),
        )
        .unwrap();
        plan.process.user.gid = unsafe { libc::getegid() }.wrapping_add(1);
        assert!(apply_process_identity(&plan.process).is_err());
    }
}
