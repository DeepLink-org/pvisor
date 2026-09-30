//! The container init pipeline.
//!
//! `Create` re-executes the shim binary in internal mode (house "self-exec"
//! pattern, cf. pvisor's INTERNAL_SANDBOX_ARG). The internal process (A)
//! enters the configured namespaces, mounts the rootfs, forks the init
//! process (G), and relays G's readiness back over a pipe. G finishes its
//! setup (procfs, stdio, pivot_root), signals readiness, then blocks until
//! `Start` writes one byte into the start pipe before applying the process
//! identity and exec'ing the workload.
//!
//! File-descriptor inheritance (all pipes are non-CLOEXEC so they survive
//! the exec into A):
//!
//! ```text
//!   pipe        shim                A (internal parent)     G (init)
//!   ready       read end            write end -> relay      closed
//!   start       write end           closed after fork       read end, blocks
//!   fork report closed              read end                write end
//! ```
//! G additionally closes every other inherited fd before exec so the
//! workload sees only its stdio.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use log::{debug, warn};
use serde::{Deserialize, Serialize};

use crate::caps;
use crate::cgroup;
use crate::fifo;
use crate::mount;
use crate::plan::{CgroupPlan, ContainerPlan, IdMappingPlan, NamespaceKind};

/// CLI argument that turns the shim binary into the internal init parent.
pub const INTERNAL_INIT_ARG: &str = "--pvisor-shim-internal-init";
/// Path of the serialized [`ContainerPlan`].
pub const ENV_PLAN: &str = "PVISOR_SHIM_INIT_PLAN";
/// Write end of the ready pipe (A reports to the shim).
pub const ENV_READY_FD: &str = "PVISOR_SHIM_INIT_READY_FD";
/// Read end of the start pipe (G waits for one byte).
pub const ENV_START_FD: &str = "PVISOR_SHIM_INIT_START_FD";
/// Write end of the fork report pipe (G reports setup to A).
pub const ENV_FORK_REPORT_FD: &str = "PVISOR_SHIM_INIT_FORK_REPORT_FD";

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

/// Host-side state kept by the task service between Create and Start.
pub struct InitChild {
    /// PID of the init process (G), set once A reports readiness.
    pub pid: Option<u32>,
    /// Write end of the start pipe; held until Start (or cleanup).
    start_fd: RawFd,
}

impl InitChild {
    /// Signal G to exec; consumes the start pipe write end.
    pub fn start(&mut self) -> Result<()> {
        let byte = b"s";
        let written = unsafe { libc::write(self.start_fd, byte.as_ptr().cast(), byte.len()) };
        self.close_start();
        if written < 0 {
            return Err(std::io::Error::last_os_error())
                .context("write start pipe (init process may have exited)");
        }
        Ok(())
    }

    /// Release the start pipe write end; G sees EOF and exits.
    pub fn close_start(&mut self) {
        if self.start_fd >= 0 {
            unsafe { libc::close(self.start_fd) };
            self.start_fd = -1;
        }
    }
}

impl Drop for InitChild {
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

/// Re-exec the shim binary as the init parent (A) and wait for its relay.
///
/// Blocks until G signals readiness, so call from a blocking context.
pub fn spawn_init_child(plan: &ContainerPlan) -> Result<InitChild> {
    let plan_path = plan.bundle.join("pvisor-plan.json");
    fs::write(
        &plan_path,
        serde_json::to_vec(plan).context("serialize plan")?,
    )
    .with_context(|| format!("write {}", plan_path.display()))?;

    let (ready_r, ready_w) = make_pipe()?;
    let (start_r, start_w) = make_pipe()?;
    let (fork_r, fork_w) = make_pipe()?;

    let exe = std::env::current_exe().context("current exe")?;
    let mut child = Command::new(exe)
        .arg(INTERNAL_INIT_ARG)
        .env(ENV_PLAN, &plan_path)
        .env(ENV_READY_FD, ready_w.to_string())
        .env(ENV_START_FD, start_r.to_string())
        .env(ENV_FORK_REPORT_FD, fork_w.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .context("spawn internal init process")?;

    // A inherited every end; drop the copies this process must not hold.
    for fd in [ready_w, start_r, fork_r, fork_w] {
        unsafe { libc::close(fd) };
    }

    let mut line = String::new();
    {
        let mut reader = BufReader::new(unsafe { fs::File::from_raw_fd(ready_r) });
        reader.read_line(&mut line).context("read init report")?;
    }
    let _ = child.wait();

    match InitReport::parse(&line)? {
        InitReport::Ok { pid } => Ok(InitChild {
            pid: Some(pid),
            start_fd: start_w,
        }),
        InitReport::Error { context, errno } => {
            unsafe { libc::close(start_w) };
            anyhow::bail!("init child setup failed: {context} (errno {errno})");
        }
    }
}

/// Entry point of the internal init parent (A). Returns Ok(false) when the
/// binary was not invoked in internal mode; otherwise never returns.
pub fn run_internal_if_requested() -> Result<bool> {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|arg| arg == INTERNAL_INIT_ARG) {
        return Ok(false);
    }
    let exit_code = match init_parent_main() {
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
                eprintln!("pvisor shim init failed: {error:#}");
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

fn init_parent_main() -> Result<()> {
    let plan_path = std::env::var(ENV_PLAN).context("missing plan path")?;
    let plan: ContainerPlan =
        serde_json::from_slice(&fs::read(&plan_path).with_context(|| format!("read {plan_path}"))?)
            .context("parse plan")?;

    let start_fd = fd_from_env(ENV_START_FD)?;
    let fork_report_fd = fd_from_env(ENV_FORK_REPORT_FD)?;

    // Namespace entry order matters: join existing namespaces (CRI sandbox
    // network et al.) before unsharing the private ones.
    join_namespace_paths(&plan)?;
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
        // G inherits A's fds; it keeps only start_r and fork_w (plus stdio).
        init_process_main(&plan, start_fd, fork_report_fd);
    }
    let g_pid = g_pid as u32;

    // A no longer needs the start pipe at all.
    unsafe { libc::close(start_fd) };

    // Cgroup bookkeeping with host paths still visible.
    if let Some(cgroup_plan) = plan.cgroup.as_ref()
        && let Err(error) = attach_cgroup(cgroup_plan, g_pid)
    {
        warn!("cgroup setup skipped: {error:#}");
    }

    // Relay G's setup report to the shim.
    let mut line = String::new();
    {
        let mut reader = BufReader::new(unsafe { fs::File::from_raw_fd(fork_report_fd) });
        reader.read_line(&mut line).context("read fork report")?;
    }
    let relayed = match InitReport::parse(&line)? {
        InitReport::Ok { .. } => InitReport::Ok { pid: g_pid },
        error @ InitReport::Error { .. } => error,
    };
    let mut ready = ready_pipe_from_env().context("ready pipe")?;
    ready
        .write_all(&relayed.line()?)
        .and_then(|()| ready.flush())
        .context("relay init report")?;
    Ok(())
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
        let name = namespace.kind.proc_ns_name();
        let path_display = path.display().to_string();
        let cpath = std::ffi::CString::new(path_display.as_str())
            .with_context(|| format!("namespace path {path_display} has NUL"))?;
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("open {name} namespace at {path_display}"));
        }
        let ret = unsafe { libc::setns(fd, namespace.kind.clone_flag()) };
        unsafe { libc::close(fd) };
        if ret != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("setns {name} namespace at {path_display}"));
        }
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

/// G: finish setup, report readiness, wait for start, then exec.
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

    // Close everything inherited from the shim except the two pipes G owns.
    close_inherited_fds(&[0, 1, 2, start_fd, fork_report_fd]);

    // procfs must be mounted by a member of the new pid namespace, i.e. G.
    if let Err(error) = mount::mount_proc(&plan.rootfs) {
        fail(format!("{error:#}"));
    }

    // Open IO by host paths while they are still reachable.
    let io_files = match open_stdio(plan) {
        Ok(files) => files,
        Err(error) => fail(format!("{error:#}")),
    };

    if let Err(error) = mount::pivot_root(&plan.rootfs) {
        fail(format!("{error:#}"));
    }
    if plan.root_readonly
        && let Err(error) = mount::remount_root_readonly()
    {
        warn!("root read-only remount failed: {error:#}");
    }

    // Detach into a session/process group so signals addressed to the task
    // reach the workload; terminals also claim a controlling tty.
    unsafe {
        if plan.io.terminal {
            libc::setsid();
            let ret = libc::ioctl(io_files.1.as_raw_fd(), libc::TIOCSCTTY, 0);
            if ret != 0 {
                warn!("TIOCSCTTY failed: {}", std::io::Error::last_os_error());
            }
        } else {
            libc::setpgid(0, 0);
        }
    }

    // Wire the opened fds onto 0/1/2 (dup2 clears CLOEXEC on the targets).
    let (stdin, stdout, stderr) = io_files;
    unsafe {
        libc::dup2(stdin.as_raw_fd(), libc::STDIN_FILENO);
        libc::dup2(stdout.as_raw_fd(), libc::STDOUT_FILENO);
        libc::dup2(stderr.as_raw_fd(), libc::STDERR_FILENO);
    }
    drop(stdin);
    drop(stdout);
    drop(stderr);

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

    if let Err(error) = apply_process_identity(plan) {
        eprintln!("pvisor shim: {error:#}");
        std::process::exit(crate::EXEC_FAILURE_EXIT_CODE);
    }
    if let Err(error) = exec_process(plan) {
        eprintln!("pvisor shim: exec failed: {error:#}");
        std::process::exit(crate::EXEC_FAILURE_EXIT_CODE);
    }
    unreachable!("exec never returns on success")
}

/// Close every open fd except the listed ones (and log failures).
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

type StdioFile = fs::File;

fn open_stdio(plan: &ContainerPlan) -> Result<(StdioFile, StdioFile, StdioFile)> {
    let io = &plan.io;
    if io.terminal {
        let console_socket = io
            .stdout
            .as_deref()
            .context("terminal task without console socket")?;
        let (master, slave) = fifo::open_pty(0, 0)?;
        fifo::send_console_master(console_socket, &master)?;
        drop(master);
        return Ok((slave.try_clone()?, slave.try_clone()?, slave));
    }
    let stdin = fifo::open_stdin(io.stdin.as_deref())?;
    let stdout = fifo::open_output("stdout", io.stdout.as_deref())?;
    let stderr = if io.stderr.as_deref() == io.stdout.as_deref() {
        stdout.try_clone()?
    } else {
        fifo::open_output("stderr", io.stderr.as_deref())?
    };
    Ok((stdin, stdout, stderr))
}

fn apply_process_identity(plan: &ContainerPlan) -> Result<()> {
    let user = &plan.process.user;

    for rlimit in &plan.process.rlimits {
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

    apply_capabilities(&plan.process.capabilities)?;

    if let Some(umask) = user.umask {
        unsafe { libc::umask(umask) };
    }
    if plan.process.no_new_privileges
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

fn exec_process(plan: &ContainerPlan) -> Result<()> {
    let argv: Vec<std::ffi::CString> = plan
        .process
        .argv
        .iter()
        .map(|arg| std::ffi::CString::new(arg.as_str()))
        .collect::<std::result::Result<_, _>>()
        .context("argv contains NUL")?;
    let envp: Vec<std::ffi::CString> = plan
        .process
        .env
        .iter()
        .map(|entry| std::ffi::CString::new(entry.as_str()))
        .collect::<std::result::Result<_, _>>()
        .context("env contains NUL")?;

    let cwd = Path::new(&plan.process.cwd);
    if let Err(error) = std::env::set_current_dir(cwd) {
        fs::create_dir_all(cwd)
            .and_then(|()| std::env::set_current_dir(cwd))
            .with_context(|| format!("enter cwd {} ({error})", cwd.display()))?;
    }

    let program = argv[0].clone();
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|arg| arg.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());
    let mut envp_ptrs: Vec<*const libc::c_char> = envp.iter().map(|entry| entry.as_ptr()).collect();
    envp_ptrs.push(std::ptr::null());
    debug!(
        "executing init process {} in {}",
        program.to_string_lossy(),
        cwd.display()
    );
    if unsafe { libc::execve(program.as_ptr(), argv_ptrs.as_ptr(), envp_ptrs.as_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("execve {}", program.to_string_lossy()));
    }
    Ok(())
}
