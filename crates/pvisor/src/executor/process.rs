#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::executor::sandbox::{INTERNAL_SANDBOX_ARG, NetworkIsolation};
#[cfg(target_os = "macos")]
use crate::executor::sandbox::{
    MACOS_SANDBOX_EXEC, SEATBELT_ATTESTATION, SeatbeltPlan, seatbelt_profile,
    seatbelt_profile_with_reads,
};
#[cfg(target_os = "linux")]
use crate::executor::sandbox::{ROOTLESS_ATTESTATION, SandboxPlan, landlock_runtime_available};
use crate::executor::sandbox::{SANDBOX_ARG0_ENV, SANDBOX_PLAN_ENV, SANDBOX_SETUP_FAILED_WARNING};
use crate::executor::{Captured, ExecutorOutput, RunExecutor, Session, SessionEnd as End, stdio};
use crate::session::lifecycle::terminate_process_tree;
use async_trait::async_trait;
use pvisor_core::{
    CapabilityDimension, CapabilityEnforcementEvidence, CapabilityEnforcementPlan, ExecutorKind,
    ExecutorObservations, ExecutorPlan, IsolationKind, ProcessInvocation, ProcessOutput,
    ResourceLimits, RunFailure, RunFailureKind, RunInvocation, RunSpec, RunState, StdioMode,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use pvisor_core::{FilesystemAccess, NetworkCapability};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::Path;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::{Command as StdCommand, Stdio};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn check_read_only_grants(spec: &RunSpec, cwd: &Path, writable: &[PathBuf]) -> std::io::Result<()> {
    for grant in spec
        .capabilities
        .filesystem
        .iter()
        .filter(|g| g.access == FilesystemAccess::Read)
    {
        let path = cwd.join(&grant.path).canonicalize()?;
        for writable in writable {
            let writable = writable.canonicalize()?;
            if path.starts_with(&writable) || writable.starts_with(&path) {
                return Err(std::io::Error::other(format!(
                    "read-only share overlaps a writable runtime path: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
struct ResourceCgroup {
    path: PathBuf,
}

#[cfg(target_os = "linux")]
fn validate_cgroup_relative_path(relative: &Path) -> std::io::Result<()> {
    if relative
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        Ok(())
    } else {
        Err(std::io::Error::other("unsafe cgroup v2 membership path"))
    }
}

#[cfg(target_os = "linux")]
impl ResourceCgroup {
    fn prepare(limits: &ResourceLimits) -> std::io::Result<Option<Self>> {
        if limits.memory_bytes.is_none() && limits.processes.is_none() {
            return Ok(None);
        }
        let membership = std::fs::read_to_string("/proc/self/cgroup")?;
        let relative = membership
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .ok_or_else(|| std::io::Error::other("unified cgroup v2 membership is unavailable"))?;
        let relative = Path::new(relative.trim_start_matches('/'));
        validate_cgroup_relative_path(relative)?;
        let parent = Path::new("/sys/fs/cgroup").join(relative);
        let path = parent.join(format!("pvisor-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&path)?;
        let configure = (|| {
            if let Some(bytes) = limits.memory_bytes {
                std::fs::write(path.join("memory.max"), bytes.to_string())?;
            }
            if let Some(processes) = limits.processes {
                std::fs::write(path.join("pids.max"), processes.to_string())?;
            }
            Ok::<_, std::io::Error>(())
        })();
        if let Err(error) = configure {
            let _ = std::fs::remove_dir(&path);
            return Err(error);
        }
        Ok(Some(Self { path }))
    }

    fn install(&self, command: &mut Command) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;

        let membership = std::fs::OpenOptions::new()
            .write(true)
            .open(self.path.join("cgroup.procs"))?;
        // SAFETY: the pre-exec hook performs one async-signal-safe write to a
        // cgroup.procs file opened by the parent. Writing `0` moves the calling
        // child into the prepared cgroup before Agent code executes.
        unsafe {
            command.as_std_mut().pre_exec(move || {
                let fd = membership.as_raw_fd();
                let moved = libc::write(fd, b"0".as_ptr().cast(), 1);
                if moved == 1 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl Drop for ResourceCgroup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.path);
    }
}

#[cfg(unix)]
pub(super) struct ForegroundProcessGroup {
    terminal_fd: libc::c_int,
    original_pgrp: libc::pid_t,
}

#[cfg(unix)]
impl ForegroundProcessGroup {
    pub(super) fn give_to(
        child: &Child,
        invocation: &ProcessInvocation,
    ) -> std::io::Result<Option<Self>> {
        if invocation.stdin != StdioMode::Inherit
            || unsafe { libc::isatty(libc::STDIN_FILENO) } != 1
        {
            return Ok(None);
        }
        let Some(pid) = child.id() else {
            return Ok(None);
        };
        let terminal_fd = libc::STDIN_FILENO;
        let Some(original_pgrp) = controlling_foreground_pgrp(terminal_fd)? else {
            return Ok(None);
        };
        set_terminal_pgrp(terminal_fd, pid as libc::pid_t)?;
        // The child may have attempted a terminal read between spawn and
        // tcsetpgrp and received SIGTTIN. Resume its whole process group.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGCONT);
        }
        Ok(Some(Self {
            terminal_fd,
            original_pgrp,
        }))
    }
}

#[cfg(unix)]
fn controlling_foreground_pgrp(fd: libc::c_int) -> std::io::Result<Option<libc::pid_t>> {
    let foreground = unsafe { libc::tcgetpgrp(fd) };
    if foreground < 0 {
        let error = std::io::Error::last_os_error();
        // A PTY can be inherited without being this session's controlling
        // terminal. It remains usable for I/O, but has no group to hand off.
        if error.raw_os_error() == Some(libc::ENOTTY) {
            return Ok(None);
        }
        return Err(error);
    }
    if foreground != unsafe { libc::getpgrp() } {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "cannot hand off a terminal from a background process group",
        ));
    }
    Ok(Some(foreground))
}

#[cfg(unix)]
impl Drop for ForegroundProcessGroup {
    fn drop(&mut self) {
        let _ = set_terminal_pgrp(self.terminal_fd, self.original_pgrp);
    }
}

/// Change the terminal foreground group without letting a background caller
/// stop itself with SIGTTOU. Signal masking is thread-local and restored before
/// returning, so it is safe inside the multi-threaded Tokio runtime.
#[cfg(unix)]
fn set_terminal_pgrp(fd: libc::c_int, pgrp: libc::pid_t) -> std::io::Result<()> {
    unsafe {
        let mut blocked: libc::sigset_t = std::mem::zeroed();
        let mut previous: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut blocked);
        libc::sigaddset(&mut blocked, libc::SIGTTOU);
        let mask_error = libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous);
        if mask_error != 0 {
            return Err(std::io::Error::from_raw_os_error(mask_error));
        }
        let result = libc::tcsetpgrp(fd, pgrp);
        let error = (result != 0).then(std::io::Error::last_os_error);
        libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProcessExecutor {
    /// `Some` selects the platform sandbox launcher. The normal library
    /// default intentionally remains the compatibility host process.
    sandbox_launcher: Option<PathBuf>,
}

struct PreparedCommand {
    command: Command,
    resources: SandboxResources,
}

enum SandboxResources {
    None,
    #[cfg(target_os = "linux")]
    Linux {
        root: PathBuf,
        attestation: tempfile::NamedTempFile,
        controls: CapabilityEnforcementPlan,
        plan: tempfile::NamedTempFile,
    },
    #[cfg(target_os = "macos")]
    MacOS {
        scratch: tempfile::TempDir,
        attestation: tempfile::NamedTempFile,
        controls: CapabilityEnforcementPlan,
    },
}

impl SandboxResources {
    fn none() -> Self {
        Self::None
    }

    #[cfg(target_os = "linux")]
    fn create() -> std::io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            ".pvisor-rootfs-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        let attestation = tempfile::Builder::new()
            .prefix("pvisor-rootless-attestation-")
            .tempfile()?;
        let plan = tempfile::Builder::new()
            .prefix("pvisor-rootless-plan-")
            .tempfile()?;
        Ok(Self::Linux {
            root: path,
            attestation,
            controls: Default::default(),
            plan,
        })
    }

    #[cfg(target_os = "linux")]
    fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Linux { root, .. } => Some(root),
            Self::None => None,
        }
    }

    #[cfg(target_os = "linux")]
    fn attestation_path(&self) -> Option<&Path> {
        match self {
            Self::Linux { attestation, .. } => Some(attestation.path()),
            Self::None => None,
        }
    }

    #[cfg(target_os = "linux")]
    fn write_plan(&mut self, encoded: &[u8]) -> std::io::Result<&Path> {
        use std::io::{Seek, Write};

        let Self::Linux { plan, .. } = self else {
            unreachable!("Linux sandbox resources required")
        };
        let file = plan.as_file_mut();
        file.rewind()?;
        file.set_len(0)?;
        file.write_all(encoded)?;
        file.sync_all()?;
        Ok(plan.path())
    }

    #[cfg(target_os = "macos")]
    fn create() -> std::io::Result<Self> {
        let scratch = tempfile::Builder::new()
            .prefix("pvisor-seatbelt-scratch-")
            .tempdir()?;
        let attestation = tempfile::Builder::new()
            .prefix("pvisor-seatbelt-attestation-")
            .tempfile()?;
        Ok(Self::MacOS {
            scratch,
            attestation,
            controls: Default::default(),
        })
    }

    #[cfg(target_os = "macos")]
    fn scratch_path(&self) -> Option<&Path> {
        match self {
            Self::MacOS { scratch, .. } => Some(scratch.path()),
            Self::None => None,
        }
    }

    #[cfg(target_os = "macos")]
    fn attestation_path(&self) -> Option<&Path> {
        match self {
            Self::MacOS { attestation, .. } => Some(attestation.path()),
            Self::None => None,
        }
    }

    fn observed_controls(&mut self) -> Option<CapabilityEnforcementEvidence> {
        if !self.setup_attested() {
            return None;
        }
        let controls = match self {
            Self::None => return Some(Default::default()),
            #[cfg(target_os = "linux")]
            Self::Linux { controls, .. } => controls,
            #[cfg(target_os = "macos")]
            Self::MacOS { controls, .. } => controls,
        };
        let mut observed = CapabilityEnforcementEvidence::default();
        for (dimension, control) in &controls.dimensions {
            for mechanism in &control.mechanisms {
                observed.record(
                    *dimension,
                    pvisor_core::EnforcementLevel::Enforced,
                    mechanism,
                );
            }
        }
        Some(observed)
    }

    #[cfg(target_os = "macos")]
    fn setup_attested(&mut self) -> bool {
        use std::io::{Read, Seek};

        let Self::MacOS { attestation, .. } = self else {
            return true;
        };
        let file = attestation.as_file_mut();
        if file.rewind().is_err() {
            return false;
        }
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).is_ok() && contents == SEATBELT_ATTESTATION
    }

    #[cfg(target_os = "linux")]
    fn setup_attested(&mut self) -> bool {
        use std::io::{Read, Seek};

        let Self::Linux { attestation, .. } = self else {
            return true;
        };
        let file = attestation.as_file_mut();
        if file.rewind().is_err() {
            return false;
        }
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).is_ok() && contents == ROOTLESS_ATTESTATION
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn setup_attested(&mut self) -> bool {
        true
    }
}

impl Drop for SandboxResources {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        if let Self::Linux { root: path, .. } = self {
            // Never recurse over a security-sensitive path.  A successful
            // launcher leaves an empty mountpoint; a non-empty directory is
            // retained for diagnosis instead of being removed destructively.
            let _ = std::fs::remove_dir(path);
        }
    }
}

async fn read_limited<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
    stop: tokio_util::sync::CancellationToken,
) -> std::io::Result<Captured> {
    let mut retained = Vec::with_capacity(limit.min(8192));
    let mut buf = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = tokio::select! {
            biased;
            _ = stop.cancelled() => {
                truncated = true;
                break;
            }
            read = reader.read(&mut buf) => read?,
        };
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        let keep = remaining.min(read);
        retained.extend_from_slice(&buf[..keep]);
        truncated |= keep < read;
    }
    Ok(Captured {
        text: String::from_utf8_lossy(&retained).into_owned(),
        truncated,
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn network_isolation(spec: &RunSpec) -> std::io::Result<NetworkIsolation> {
    if crate::executor::sandbox::sandbox_required(spec) {
        #[cfg(target_os = "macos")]
        {
            let proxy = spec
                .metadata
                .get(crate::executor::sandbox::SANDBOX_PROXY_KEY)
                .and_then(serde_json::Value::as_str)
                .map(str::parse::<std::net::SocketAddr>)
                .transpose()
                .map_err(std::io::Error::other)?;
            if proxy.is_none() && !matches!(spec.capabilities.network, NetworkCapability::Deny) {
                return Err(std::io::Error::other(
                    "required sandbox needs a supervisor-owned proxy",
                ));
            }
            return Ok(NetworkIsolation::ProxyOnly(proxy));
        }
        #[cfg(target_os = "linux")]
        if !matches!(spec.capabilities.network, NetworkCapability::Deny) {
            // Host selective egress is cooperative: clients such as ZCode use
            // the supervisor-owned loopback proxy, while direct sockets remain
            // outside the proxy boundary. Strict policy rejects this evidence;
            // VM or deny-all is required for non-bypassable egress.
            return Ok(NetworkIsolation::Ambient);
        }
    }
    if matches!(spec.capabilities.network, NetworkCapability::Deny) {
        Ok(NetworkIsolation::LoopbackOnly)
    } else {
        Ok(NetworkIsolation::Ambient)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn filesystem_isolation(spec: &RunSpec) -> bool {
    // Filesystem isolation defaults to restricted unless the marker is "host".
    // CLI runs set it explicitly, independently of network policy.
    !matches!(
        spec.metadata
            .get("pvisor.filesystem.mode")
            .and_then(serde_json::Value::as_str),
        Some("host")
    )
}

fn resolve_host_program(program: &str) -> std::path::PathBuf {
    if program.contains(std::path::MAIN_SEPARATOR) {
        return program.into();
    }
    let Some(path) = std::env::var_os("PATH") else {
        return program.into();
    };
    std::env::split_paths(&path)
        .map(|directory| directory.join(program))
        .find(|candidate| is_executable(candidate))
        .unwrap_or_else(|| program.into())
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

impl ProcessExecutor {
    /// Build a Linux rootless executor using `launcher` for the trusted
    /// namespace/Landlock setup stage.
    ///
    /// The launcher must dispatch [`crate::executor::sandbox::run_internal_if_requested`]
    /// before starting threads or an async runtime.  The `pvisor` binary is the
    /// canonical launcher and uses this path automatically for default host Runs.
    #[cfg(target_os = "linux")]
    pub fn rootless_with_launcher(launcher: impl Into<PathBuf>) -> std::io::Result<Self> {
        let launcher = launcher.into().canonicalize()?;
        if !is_executable(&launcher) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "rootless sandbox launcher is not executable: {}",
                    launcher.display()
                ),
            ));
        }
        Ok(Self {
            sandbox_launcher: Some(launcher),
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn rootless_with_launcher(launcher: impl Into<PathBuf>) -> std::io::Result<Self> {
        let _ = launcher.into();
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the rootless local process executor is only available on Linux",
        ))
    }

    /// Build a macOS executor that installs a generated Seatbelt profile
    /// before entering the hidden launcher and executing Agent code.
    #[cfg(target_os = "macos")]
    pub fn seatbelt_with_launcher(launcher: impl Into<PathBuf>) -> std::io::Result<Self> {
        let launcher = launcher.into().canonicalize()?;
        if !is_executable(&launcher) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "Seatbelt sandbox launcher is not executable: {}",
                    launcher.display()
                ),
            ));
        }
        if !is_executable(Path::new(MACOS_SANDBOX_EXEC)) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("required Seatbelt launcher is unavailable: {MACOS_SANDBOX_EXEC}"),
            ));
        }
        Ok(Self {
            sandbox_launcher: Some(launcher),
        })
    }

    #[cfg(not(target_os = "macos"))]
    pub fn seatbelt_with_launcher(launcher: impl Into<PathBuf>) -> std::io::Result<Self> {
        let _ = launcher.into();
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the Seatbelt local process sandbox is only available on macOS",
        ))
    }

    pub fn is_rootless(&self) -> bool {
        cfg!(target_os = "linux") && self.sandbox_launcher.is_some()
    }

    pub fn is_seatbelt(&self) -> bool {
        cfg!(target_os = "macos") && self.sandbox_launcher.is_some()
    }

    pub fn is_sandboxed(&self) -> bool {
        self.sandbox_launcher.is_some()
    }

    fn spawn_command(
        &self,
        spec: &RunSpec,
        invocation: &ProcessInvocation,
    ) -> std::io::Result<PreparedCommand> {
        // Resolve a bare command against the host PATH before changing cwd to
        // an OverlayFS merged root. The executable belongs to the host-process
        // executor and need not exist inside the projected lower filesystem.
        let program = resolve_host_program(&invocation.program);
        if crate::executor::sandbox::sandbox_required(spec) && !self.is_sandboxed() {
            return Err(std::io::Error::other(
                "required sandbox cannot use an unsandboxed process executor",
            ));
        }
        let (mut command, sandbox_plan, resources) = if let Some(launcher) = &self.sandbox_launcher
        {
            platform_launcher_command(launcher, spec, invocation, &program)?
        } else {
            let mut command = Command::new(program);
            command.args(&invocation.args);
            (command, None, SandboxResources::none())
        };
        command
            .stdin(stdio(invocation.stdin))
            .stdout(stdio(invocation.stdout))
            .stderr(stdio(invocation.stderr))
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
            let mut limits = spec.runtime.resource_limits.clone();
            // RLIMIT_NPROC must be applied after the rootless launcher has
            // created its private PID namespace and reaper. Applying it to
            // the launcher itself can make setup fail with EAGAIN when the
            // host user already has more processes than the requested cap.
            if sandbox_plan.is_some() {
                limits.processes = None;
            }
            install_resource_limit_hook(&mut command, limits);
        }
        if let Some(cwd) = &invocation.cwd {
            command.current_dir(cwd);
        }
        if !invocation.inherit_env {
            command.env_clear();
        }
        command.envs(&invocation.env);
        // This is a reserved supervisor-to-launcher capability. Apply it last
        // so an untrusted Run environment cannot remove or replace the policy.
        if sandbox_plan.is_some() {
            // The launcher canonicalizes the executable so it can project the
            // real inode into a synthetic root. Preserve the caller's original
            // argv[0] separately: Alpine's /bin commands are often BusyBox
            // symlinks, and BusyBox chooses its applet from that basename.
            command.env(SANDBOX_ARG0_ENV, &invocation.program);
        }
        if let Some(sandbox_plan) = sandbox_plan {
            command.env(SANDBOX_PLAN_ENV, sandbox_plan);
        }
        #[cfg(target_os = "macos")]
        if let Some(scratch) = resources.scratch_path() {
            // A Run-owned temporary directory avoids granting the Agent the
            // shared /tmp or per-user Darwin temporary hierarchy.
            command.env("TMPDIR", scratch);
            if crate::executor::sandbox::sandbox_required(spec) {
                command.env("HOME", scratch);
            }
        }
        Ok(PreparedCommand { command, resources })
    }
}

/// Probe the namespace primitives used by the default Linux launcher without
/// mutating the pVisor process itself.  A short-lived `unshare` child keeps the
/// probe safe in a multithreaded Tokio process and distinguishes an unavailable
/// host capability from a later Agent failure.
#[cfg(target_os = "linux")]
pub fn rootless_runtime_available(preserve_host_filesystem: bool) -> bool {
    let probe = |args: &[&str]| {
        StdCommand::new("unshare")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    if preserve_host_filesystem {
        // Network-only Runs still use a user namespace so the Agent cannot
        // create another namespace and escape deny-all. Landlock is omitted
        // at runtime, so it must not be a prerequisite for this probe.
        probe(["--user", "--mount", "--net", "--fork", "true"].as_slice())
    } else {
        landlock_runtime_available()
            && probe(["--user", "--mount", "--pid", "--fork", "true"].as_slice())
    }
}

#[cfg(unix)]
fn install_resource_limit_hook(command: &mut Command, limits: ResourceLimits) {
    if limits.is_empty() {
        return;
    }
    use std::os::unix::process::CommandExt;
    // SAFETY: the hook only invokes async-signal-safe getrlimit/setrlimit calls
    // and does not allocate or acquire locks between fork and exec.
    unsafe {
        command
            .as_std_mut()
            .pre_exec(move || apply_resource_limits(&limits));
    }
}

#[cfg(unix)]
fn apply_resource_limits(limits: &ResourceLimits) -> std::io::Result<()> {
    macro_rules! set_limit {
        ($resource:expr, $value:expr) => {{
            let mut current = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if unsafe { libc::getrlimit($resource, &mut current) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let requested = $value as libc::rlim_t;
            let effective = requested.min(current.rlim_max);
            let limit = libc::rlimit {
                rlim_cur: effective,
                rlim_max: effective,
            };
            if unsafe { libc::setrlimit($resource, &limit) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }};
    }

    #[cfg(not(target_os = "macos"))]
    if let Some(bytes) = limits.memory_bytes {
        set_limit!(libc::RLIMIT_AS, bytes);
    }
    if let Some(processes) = limits.processes {
        set_limit!(libc::RLIMIT_NPROC, processes);
    }
    if let Some(milliseconds) = limits.cpu_time_ms {
        let seconds = milliseconds
            .saturating_add(999)
            .checked_div(1_000)
            .unwrap_or(0)
            .max(1);
        set_limit!(libc::RLIMIT_CPU, seconds);
    }
    if let Some(open_files) = limits.open_files {
        set_limit!(libc::RLIMIT_NOFILE, open_files);
    }
    if let Some(bytes) = limits.file_size_bytes {
        set_limit!(libc::RLIMIT_FSIZE, bytes);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn platform_launcher_command(
    launcher: &Path,
    spec: &RunSpec,
    invocation: &ProcessInvocation,
    program: &Path,
) -> std::io::Result<(Command, Option<String>, SandboxResources)> {
    let program = program.canonicalize().map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("resolve Agent executable {}: {error}", program.display()),
        )
    })?;
    let mut sandbox_root = SandboxResources::create()?;
    let network = network_isolation(spec)?;
    let filesystem_isolated = filesystem_isolation(spec);
    let plan = rootless_plan(
        spec,
        invocation,
        &program,
        sandbox_root
            .path()
            .expect("created sandbox root")
            .to_owned(),
        sandbox_root
            .attestation_path()
            .expect("created rootless attestation")
            .to_owned(),
        network,
        filesystem_isolated,
    )?;
    if let SandboxResources::Linux { controls, .. } = &mut sandbox_root {
        if plan.filesystem_isolated {
            *controls = controls
                .clone()
                .planned(CapabilityDimension::FilesystemRead, "linux-synthetic-root")
                .planned(CapabilityDimension::FilesystemWrite, "linux-synthetic-root");
        }
        if plan.network.is_loopback_only() {
            *controls = controls
                .clone()
                .planned(CapabilityDimension::Network, "linux-network-namespace");
        }
    }
    let encoded = serde_json::to_vec(&plan).map_err(std::io::Error::other)?;
    let plan_path = sandbox_root
        .write_plan(&encoded)?
        .to_string_lossy()
        .into_owned();
    let mut command = Command::new(launcher);
    command
        .arg(INTERNAL_SANDBOX_ARG)
        .arg("--")
        .arg(&program)
        .args(&invocation.args);
    Ok((command, Some(plan_path), sandbox_root))
}

#[cfg(target_os = "macos")]
fn platform_launcher_command(
    launcher: &Path,
    spec: &RunSpec,
    invocation: &ProcessInvocation,
    program: &Path,
) -> std::io::Result<(Command, Option<String>, SandboxResources)> {
    let program = program.canonicalize().map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("resolve Agent executable {}: {error}", program.display()),
        )
    })?;
    let mut resources = SandboxResources::create()?;
    let filesystem_isolated = filesystem_isolation(spec);
    let cwd = invocation
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let cwd = cwd.canonicalize()?;
    let restrict_reads = crate::executor::sandbox::sandbox_required(spec);
    let mut readable_paths = vec![program.clone(), launcher.canonicalize()?];
    let mut writable_paths = vec![
        cwd.clone(),
        resources
            .scratch_path()
            .expect("created Seatbelt scratch directory")
            .to_owned(),
        resources
            .attestation_path()
            .expect("created Seatbelt attestation")
            .to_owned(),
    ];
    let staged = spec
        .metadata
        .get("pvisor.runtime.implant")
        .and_then(|implant| implant.get("overlay_merged"))
        .and_then(serde_json::Value::as_str)
        .is_some();
    if !restrict_reads && !staged {
        // Ordinary runs write through to their projected state directories.
        // Granting HOME for a staged run would also expose its original workspace.
        writable_paths.extend(projected_state_roots(invocation));
    }
    for path in ["/dev/null", "/dev/zero", "/dev/tty", "/dev/fd"] {
        push_existing(&mut writable_paths, Path::new(path));
    }
    if restrict_reads {
        // Runtime locations are readable (never writable) in the required profile.
        for path in [
            "/System/Library",
            "/System/Cryptexes/OS",
            "/usr",
            "/bin",
            "/sbin",
            "/Library/Apple",
            "/Library/Developer",
            "/Library/Frameworks",
            "/opt/homebrew",
            "/private/var/db/dyld",
            "/private/var/db/timezone",
            "/private/etc/localtime",
            "/private/etc/hosts",
            "/private/etc/resolv.conf",
            "/private/etc/services",
            "/private/etc/protocols",
            "/private/etc/ssl",
            "/dev/urandom",
            "/dev/random",
        ] {
            push_existing(&mut readable_paths, Path::new(path));
        }
    }
    if filesystem_isolated || restrict_reads {
        for capability in &spec.capabilities.filesystem {
            let path = PathBuf::from(&capability.path);
            let path = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            if !path.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "filesystem capability path does not exist: {}",
                        path.display()
                    ),
                ));
            }
            match capability.access {
                // Read grants only matter once reads are restricted; ambient
                // reads stay available when only writes are isolated.
                FilesystemAccess::Read if restrict_reads => readable_paths.push(path),
                FilesystemAccess::Read => {}
                FilesystemAccess::ReadWrite => writable_paths.push(path),
            }
        }
    }

    check_read_only_grants(spec, &cwd, &writable_paths)?;

    let network = network_isolation(spec)?;
    let (allowed_unix_sockets, local_socket_roots) = if network.is_loopback_only() {
        (
            invocation
                .env
                .get(crate::AGENTCTL_ENDPOINT_ENV)
                .map(PathBuf::from)
                .filter(|path| path.exists())
                .into_iter()
                .collect::<Vec<_>>(),
            vec![
                cwd,
                resources
                    .scratch_path()
                    .expect("created Seatbelt scratch directory")
                    .to_owned(),
            ],
        )
    } else {
        (Vec::new(), Vec::new())
    };
    // Socket metadata must be readable, but its containing directory is not shared.
    readable_paths.extend(allowed_unix_sockets.iter().cloned());
    let (profile, parameters) = if restrict_reads {
        seatbelt_profile_with_reads(
            &writable_paths,
            Some(&readable_paths),
            &allowed_unix_sockets,
            &local_socket_roots,
            network,
            filesystem_isolated,
        )?
    } else {
        seatbelt_profile(
            &writable_paths,
            &allowed_unix_sockets,
            &local_socket_roots,
            network,
            filesystem_isolated,
        )?
    };
    let plan = SeatbeltPlan {
        restrict_reads,
        attestation: resources
            .attestation_path()
            .expect("created Seatbelt attestation")
            .to_owned(),
        network,
        filesystem_isolated,
    };
    if let SandboxResources::MacOS { controls, .. } = &mut resources {
        if plan.filesystem_isolated {
            *controls = controls.clone().planned(
                CapabilityDimension::FilesystemWrite,
                "macos-seatbelt-write-policy",
            );
        }
        if plan.restrict_reads {
            *controls = controls.clone().planned(
                CapabilityDimension::FilesystemRead,
                "macos-seatbelt-read-policy",
            );
        }
        if plan.network.is_loopback_only() {
            *controls = controls.clone().planned(
                CapabilityDimension::Network,
                "macos-seatbelt-network-policy",
            );
        }
    }
    let encoded = serde_json::to_string(&plan).map_err(std::io::Error::other)?;

    let mut command = Command::new(MACOS_SANDBOX_EXEC);
    command.arg("-p").arg(profile);
    for (key, path) in parameters {
        let path = path.to_str().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Seatbelt parameter path is not valid UTF-8: {}",
                    path.display()
                ),
            )
        })?;
        command.arg(format!("-D{key}={path}"));
    }
    command
        .arg("--")
        .arg(launcher)
        .arg(INTERNAL_SANDBOX_ARG)
        .arg("--")
        .arg(program)
        .args(&invocation.args);
    Ok((command, Some(encoded), resources))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_launcher_command(
    _launcher: &std::path::Path,
    _spec: &RunSpec,
    _invocation: &ProcessInvocation,
    _program: &std::path::Path,
) -> std::io::Result<(Command, Option<String>, SandboxResources)> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the local process sandbox is not available on this platform",
    ))
}

#[cfg(target_os = "linux")]
fn rootless_plan(
    spec: &RunSpec,
    invocation: &ProcessInvocation,
    program: &Path,
    root: PathBuf,
    attestation: PathBuf,
    network: NetworkIsolation,
    filesystem_isolated: bool,
) -> std::io::Result<SandboxPlan> {
    let cwd = invocation
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let cwd = cwd.canonicalize()?;
    let mut read_only = Vec::new();
    let mut read_write = vec![cwd.clone()];
    // The PID-namespace init mounts a fresh procfs here. Grant read access to
    // that mountpoint, rather than exposing the host's procfs in the chroot.
    read_only.push(PathBuf::from("/proc"));
    // Interactive programs resolve their inherited terminal through
    // /dev/pts/<n>; the synthetic /dev tree does not otherwise contain it.
    push_existing(&mut read_only, Path::new("/dev/pts"));
    let hidden_paths = spec
        .metadata
        .get(crate::executor::sandbox::SANDBOX_HIDDEN_PATHS_KEY)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(PathBuf::from)
        .collect::<Vec<_>>();

    // A normal Run writes through the projected home and XDG roots. A safe
    // Run mounts private copy-on-write views of them at the same paths, so
    // programs launched later from a shell get the same protection as the
    // initial executable.
    let safe = crate::executor::sandbox::sandbox_required(spec);
    let mut staged_roots = Vec::new();
    for path in projected_state_roots(invocation) {
        if safe {
            if path == Path::new("/") {
                return Err(std::io::Error::other("safe state root cannot be /"));
            }
            staged_roots.push(path);
        } else {
            read_write.push(path);
        }
    }
    staged_roots.sort_unstable_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });
    staged_roots.dedup();
    let mut minimal_roots = Vec::<PathBuf>::new();
    for path in staged_roots {
        if !minimal_roots.iter().any(|root| path.starts_with(root)) {
            minimal_roots.push(path);
        }
    }
    read_write.extend(minimal_roots.iter().cloned());
    // Keep the original project path visible as cwd so tools retain stable
    // workspace identity. The trusted implant records the FUSE merged path,
    // which is bind-mounted over that original path inside the sandbox.
    let staged_workspace_source = safe
        .then(|| spec.metadata.get("pvisor.runtime.implant"))
        .flatten()
        .and_then(|implant| implant.get("overlay_merged"))
        .and_then(serde_json::Value::as_str)
        .and_then(|path| Path::new(path).canonicalize().ok());
    let staged_workspace = safe
        .then(|| spec.metadata.get("pvisor.workspace"))
        .flatten()
        .and_then(serde_json::Value::as_str)
        .and_then(|path| Path::new(path).canonicalize().ok())
        .filter(|path| {
            staged_workspace_source
                .as_ref()
                .is_some_and(|source| source != path)
                && path.is_dir()
        });
    if let (Some(source), Some(path)) = (&staged_workspace_source, &staged_workspace) {
        read_write.push(source.clone());
        read_write.push(path.clone());
    }
    if !safe {
        // The synthetic root itself is writable, and projected host paths
        // retain their ordinary lower filesystem write semantics.
        read_write.push(PathBuf::from("/"));
    }
    let graphical_display = project_graphical_session(invocation, &mut read_only, &mut read_write);

    // A broad but immutable OS runtime keeps arbitrary local executables and
    // dynamic language runtimes working while excluding user data by default.
    for path in ["/bin", "/sbin", "/usr", "/lib", "/lib64", "/etc"] {
        let path = PathBuf::from(path);
        if path.exists() {
            // Preserve compatibility aliases such as /bin and /lib64 inside
            // the synthetic root. Canonicalizing them would project only
            // /usr/bin or /usr/lib and break ELF interpreter paths.
            read_only.push(path);
        }
    }
    // On systemd-resolved hosts this follows /etc/resolv.conf into /run,
    // whose containing hierarchy is intentionally not otherwise projected.
    push_existing(&mut read_only, Path::new("/etc/resolv.conf"));
    read_only.push(program.to_path_buf());
    // Application bundles commonly keep private ELF dependencies beside the
    // launcher (for example ZCode's libffmpeg.so). Projecting the executable
    // alone lets the ELF loader find the program but not its bundle-local
    // shared libraries. Project the directory entries individually so known
    // Chromium SUID helpers are absent from the synthetic root. Their presence
    // inside a rootless user namespace is unusable and makes Chromium abort
    // instead of selecting its user-namespace sandbox fallback.
    if let Some(parent) = program.parent() {
        push_runtime_directory(&mut read_only, parent, &hidden_paths);
    }
    for path in [
        "/dev/null",
        "/dev/zero",
        "/dev/full",
        "/dev/random",
        "/dev/urandom",
        "/dev/tty",
    ] {
        push_existing(&mut read_write, Path::new(path));
    }
    let project_render_nodes = graphical_display
        && !spec
            .metadata
            .get(crate::executor::sandbox::SANDBOX_NO_GPU_KEY)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
    if project_render_nodes && let Ok(devices) = std::fs::read_dir("/dev/dri") {
        for device in devices.flatten().map(|entry| entry.path()) {
            if device
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("renderD"))
            {
                push_existing(&mut read_write, &device);
            }
        }
    }

    // The Run-scoped AgentCtl and an explicitly supplied SSH agent are
    // capabilities represented by their exact socket inode, not by /tmp.
    // Merely inheriting the host environment must not project signing
    // authority into a safe Run.
    for key in [crate::AGENTCTL_ENDPOINT_ENV, "SSH_AUTH_SOCK"] {
        if let Some(path) = invocation.env.get(key) {
            push_existing(&mut read_write, Path::new(path));
        }
    }

    if filesystem_isolated {
        for capability in &spec.capabilities.filesystem {
            let path = PathBuf::from(&capability.path);
            let path = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            if !path.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "filesystem capability path does not exist: {}",
                        path.display()
                    ),
                ));
            }
            match capability.access {
                FilesystemAccess::Read => push_existing(&mut read_only, &path),
                FilesystemAccess::ReadWrite => push_existing(&mut read_write, &path),
            }
        }
    }

    check_read_only_grants(spec, &cwd, &read_write)?;

    read_only.sort_unstable();
    read_only.dedup();
    read_write.sort_unstable();
    read_write.dedup();
    // A staged application directory is already mounted at its original
    // path. Do not re-bind the host lower read-only over that COW view.
    read_only.retain(|path| !read_write.binary_search(path).is_ok());
    Ok(SandboxPlan {
        root,
        cwd,
        attestation,
        read_only,
        read_write,
        staged_roots: minimal_roots,
        staged_workspace,
        staged_workspace_source,
        network,
        filesystem_isolated,
        process_limit: spec.runtime.resource_limits.processes,
    })
}

#[cfg(target_os = "linux")]
fn project_graphical_session(
    invocation: &ProcessInvocation,
    read_only: &mut Vec<PathBuf>,
    read_write: &mut Vec<PathBuf>,
) -> bool {
    let value = |key: &str| {
        invocation.env.get(key).cloned().or_else(|| {
            invocation
                .inherit_env
                .then(|| std::env::var(key).ok())
                .flatten()
        })
    };

    let display = value("DISPLAY");
    if let Some(authority) = value("XAUTHORITY") {
        push_existing(read_only, Path::new(&authority));
    }
    if let Some(display) = &display {
        // Local X11 displays use /tmp/.X11-unix/X<N>. Remote displays do not
        // need a host socket projection.
        let display_number = display
            .rsplit_once(':')
            .map(|(_, suffix)| suffix.split('.').next().unwrap_or(suffix));
        if let Some(number) = display_number
            .filter(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
        {
            push_existing(
                read_write,
                &PathBuf::from(format!("/tmp/.X11-unix/X{number}")),
            );
        }
    }
    let wayland_available = if let (Some(runtime), Some(wayland)) =
        (value("XDG_RUNTIME_DIR"), value("WAYLAND_DISPLAY"))
    {
        let socket = Path::new(&wayland);
        let socket = if socket.is_absolute() {
            socket.to_path_buf()
        } else {
            Path::new(&runtime).join(socket)
        };
        push_existing(read_write, &socket);
        true
    } else {
        false
    };
    let dbus_address = value("DBUS_SESSION_BUS_ADDRESS");
    if let Some(address) = &dbus_address {
        for transport in address.split(';') {
            if let Some(path) = transport.strip_prefix("unix:path=") {
                let path = path.split(',').next().unwrap_or(path);
                push_existing(read_write, Path::new(path));
            }
        }
    }
    let desktop_session = display.is_some() || wayland_available || dbus_address.is_some();
    if desktop_session {
        push_existing(read_write, Path::new("/run/dbus/system_bus_socket"));
    }
    desktop_session
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn projected_state_roots(invocation: &ProcessInvocation) -> Vec<PathBuf> {
    [
        "HOME",
        "CODEX_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
    ]
    .into_iter()
    .filter_map(|key| {
        invocation.env.get(key).map(PathBuf::from).or_else(|| {
            invocation
                .inherit_env
                .then(|| std::env::var_os(key).map(PathBuf::from))
                .flatten()
        })
    })
    .filter_map(|path| path.canonicalize().ok())
    .filter(|path| path.is_dir())
    .collect()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn push_existing(paths: &mut Vec<PathBuf>, path: &Path) {
    if let Ok(path) = path.canonicalize() {
        paths.push(path);
    }
}

#[cfg(target_os = "linux")]
fn push_runtime_directory(paths: &mut Vec<PathBuf>, directory: &Path, hidden: &[PathBuf]) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        push_existing(paths, directory);
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Application-specific adapters can hide helpers that are incompatible
        // with the rootless namespace without suppressing unrelated programs.
        if hidden.iter().any(|candidate| {
            candidate == &path
                || path
                    .canonicalize()
                    .is_ok_and(|canonical| candidate == &canonical)
        }) {
            continue;
        }
        push_existing(paths, &path);
    }
}

fn process_exit_outcome(
    status: std::process::ExitStatus,
) -> (RunState, Option<i32>, Option<RunFailure>) {
    let (state, exit_code, failure) = crate::executor::exit_outcome(status);
    // A signal is a process failure, not a requested Run cancellation, but still
    // needs a numeric status for the frontend's exit-code contract.
    #[cfg(unix)]
    let exit_code = {
        use std::os::unix::process::ExitStatusExt;
        exit_code.or_else(|| status.signal().map(|signal| 128 + signal))
    };
    (state, exit_code, failure)
}

#[async_trait]
impl RunExecutor for ProcessExecutor {
    fn descriptor(&self) -> ExecutorPlan {
        let (name, isolation) = if self.is_rootless() {
            ("local-rootless-v1", IsolationKind::RootlessProcess)
        } else if self.is_seatbelt() {
            ("local-seatbelt-v1", IsolationKind::SandboxedProcess)
        } else {
            ("local-process-v1", IsolationKind::HostProcess)
        };
        let mut capability_plan = CapabilityEnforcementPlan::default()
            .planned(CapabilityDimension::Resources, "posix-rlimit");
        if self.is_rootless() {
            capability_plan = capability_plan
                .planned(
                    CapabilityDimension::FilesystemRead,
                    "linux-synthetic-root-landlock",
                )
                .planned(
                    CapabilityDimension::FilesystemWrite,
                    "linux-synthetic-root-landlock",
                );
        } else if self.is_seatbelt() {
            capability_plan = capability_plan.planned(
                CapabilityDimension::FilesystemWrite,
                "macos-seatbelt-write-policy",
            );
        }
        ExecutorPlan {
            name: name.into(),
            kind: ExecutorKind::Process,
            isolation,
            capability_plan,
            supports_checkpoint: false,
            supports_migration: false,
        }
    }

    fn supports(&self, invocation: &RunInvocation) -> bool {
        matches!(invocation, RunInvocation::Process(_))
    }

    async fn execute(&self, context: &Session) -> ExecutorOutput {
        let spec = context.spec().clone();
        let RunInvocation::Process(invocation) = &spec.invocation;
        context
            .transition(RunState::Starting, Some("spawning local process".into()))
            .await;

        let PreparedCommand {
            mut command,
            mut resources,
        } = match self.spawn_command(&spec, invocation) {
            Ok(command) => command,
            Err(error) => {
                return ExecutorOutput {
                    executor_observations: Default::default(),

                    state: RunState::Failed,

                    exit_code: None,
                    failure: Some(RunFailure {
                        kind: RunFailureKind::Spawn,
                        message: error.to_string(),
                        retryable: false,
                    }),
                    output: ProcessOutput::default(),
                    value: None,
                    metrics: Default::default(),
                    artifacts: Vec::new(),
                    event_stream_ref: None,
                    warnings: Vec::new(),
                };
            }
        };
        let mut warnings = Vec::new();
        let mut metrics = std::collections::BTreeMap::new();
        #[cfg(target_os = "linux")]
        let _resource_cgroup = match ResourceCgroup::prepare(&spec.runtime.resource_limits) {
            Ok(Some(cgroup)) => match cgroup.install(&mut command) {
                Ok(()) => {
                    metrics.insert("resource.cgroup_v2".into(), 1.0);
                    Some(cgroup)
                }
                Err(error) => {
                    warnings.push(format!(
                        "cgroup v2 resource controller unavailable; using inherited rlimits: {error}"
                    ));
                    None
                }
            },
            Ok(None) => None,
            Err(error) => {
                warnings.push(format!(
                    "cgroup v2 resource controller unavailable; using inherited rlimits: {error}"
                ));
                None
            }
        };
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return ExecutorOutput {
                    executor_observations: Default::default(),

                    state: RunState::Failed,

                    exit_code: None,
                    failure: Some(RunFailure {
                        kind: RunFailureKind::Spawn,
                        message: error.to_string(),
                        retryable: false,
                    }),
                    output: ProcessOutput::default(),
                    value: None,
                    metrics: Default::default(),
                    artifacts: Vec::new(),
                    event_stream_ref: None,
                    warnings: Vec::new(),
                };
            }
        };
        let process_group = child.id();
        #[cfg(unix)]
        let _foreground = match ForegroundProcessGroup::give_to(&child, invocation) {
            Ok(foreground) => foreground,
            Err(error) => {
                terminate_process_tree(
                    &mut child,
                    process_group,
                    spec.runtime.termination_grace_ms,
                )
                .await;
                return ExecutorOutput {
                    executor_observations: Default::default(),

                    state: RunState::Failed,

                    exit_code: None,
                    failure: Some(RunFailure {
                        kind: RunFailureKind::Infrastructure,
                        message: format!("failed to give terminal to child process: {error}"),
                        retryable: false,
                    }),
                    output: ProcessOutput::default(),
                    value: None,
                    metrics: Default::default(),
                    artifacts: Vec::new(),
                    event_stream_ref: None,
                    warnings: Vec::new(),
                };
            }
        };

        let drain_stop = tokio_util::sync::CancellationToken::new();
        let stdout_task = child.stdout.take().map(|stdout| {
            let limit = spec.runtime.max_output_bytes;
            let stop = drain_stop.clone();
            tokio::spawn(async move { read_limited(stdout, limit, stop).await })
        });
        let stderr_task = child.stderr.take().map(|stderr| {
            let limit = spec.runtime.max_output_bytes;
            let stop = drain_stop.clone();
            tokio::spawn(async move { read_limited(stderr, limit, stop).await })
        });

        context.transition(RunState::Running, None).await;

        let end = context
            .wait_child(&mut child, spec.runtime.timeout_ms)
            .await;

        terminate_process_tree(&mut child, process_group, spec.runtime.termination_grace_ms).await;

        let capture = async {
            let stdout = match stdout_task {
                Some(task) => task.await.ok().and_then(Result::ok),
                None => None,
            };
            let stderr = match stderr_task {
                Some(task) => task.await.ok().and_then(Result::ok),
                None => None,
            };
            (stdout, stderr)
        };
        tokio::pin!(capture);
        let (stdout, stderr) = match tokio::time::timeout(
            std::time::Duration::from_millis(spec.runtime.termination_grace_ms),
            &mut capture,
        )
        .await
        {
            Ok(output) => output,
            Err(_) => {
                drain_stop.cancel();
                warnings.push(
                    "output drain timed out; a descendant may still hold an output pipe".into(),
                );
                capture.await
            }
        };
        let mut output = ProcessOutput::default();
        if let Some(captured) = stdout {
            output.stdout = Some(captured.text);
            output.stdout_truncated = captured.truncated;
        }
        if let Some(captured) = stderr {
            output.stderr = Some(captured.text);
            output.stderr_truncated = captured.truncated;
        }

        let installed_controls = resources.observed_controls();
        let sandbox_attested = installed_controls.is_some();
        let sandbox_setup_failed = self.is_sandboxed() && !sandbox_attested;
        let mut executor_observations = ExecutorObservations::default();
        if !sandbox_setup_failed {
            executor_observations.origin = pvisor_core::event::Origin::Backend;
            executor_observations.enforcement = installed_controls.unwrap_or_default();
            // Report installed rlimits even when macOS cannot enforce requested memory.
            // The aggregate Resources evidence below still requires every requested limit.
            let limits = &spec.runtime.resource_limits;
            if limits.processes.is_some()
                || limits.cpu_time_ms.is_some()
                || limits.open_files.is_some()
                || limits.file_size_bytes.is_some()
                || (limits.memory_bytes.is_some() && !cfg!(target_os = "macos"))
            {
                metrics.insert("resource.posix_rlimit".into(), 1.0);
            }
            if !spec.runtime.resource_limits.is_empty()
                && !(cfg!(target_os = "macos")
                    && spec.runtime.resource_limits.memory_bytes.is_some())
            {
                executor_observations.enforcement = executor_observations
                    .enforcement
                    .enforced(CapabilityDimension::Resources, "posix-rlimit");
                #[cfg(target_os = "linux")]
                if metrics.get("resource.cgroup_v2") == Some(&1.0) {
                    executor_observations.enforcement = executor_observations
                        .enforcement
                        .enforced(CapabilityDimension::Resources, "linux-cgroup-v2");
                }
            }
        }

        let (state, exit_code, failure) = if sandbox_setup_failed {
            (
                RunState::Failed,
                None,
                Some(RunFailure {
                    kind: RunFailureKind::Infrastructure,
                    message: "local sandbox setup failed before Agent execution".into(),
                    retryable: false,
                }),
            )
        } else {
            match end {
                End::Exited(Ok(status)) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::ExitStatusExt;
                        executor_observations.termination_signal = status.signal();
                    }
                    process_exit_outcome(status)
                }
                End::Exited(Err(error)) => (
                    RunState::Failed,
                    None,
                    Some(RunFailure {
                        kind: RunFailureKind::Infrastructure,
                        message: error.to_string(),
                        retryable: true,
                    }),
                ),
                End::Cancelled => (RunState::Cancelled, None, None),
                End::Deadline => (
                    RunState::Failed,
                    None,
                    Some(RunFailure {
                        kind: RunFailureKind::DeadlineExceeded,
                        message: format!(
                            "attempt exceeded {} ms deadline",
                            spec.runtime.timeout_ms.unwrap_or_default()
                        ),
                        retryable: false,
                    }),
                ),
            }
        };

        ExecutorOutput {
            executor_observations,

            state,

            exit_code,
            failure,
            output,
            value: None,
            metrics,
            artifacts: Vec::new(),
            event_stream_ref: None,
            warnings: {
                if sandbox_setup_failed {
                    warnings.push(SANDBOX_SETUP_FAILED_WARNING.into());
                }
                warnings
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    #[cfg(unix)]
    #[test]
    fn foreground_query_preserves_non_enotty_errors() {
        assert_eq!(
            controlling_foreground_pgrp(-1).unwrap_err().raw_os_error(),
            Some(libc::EBADF)
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn process_executor_respects_pty_session_boundaries() {
        use std::io::{Read, Write};
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::process::CommandExt;

        const CASE_ENV: &str = "PVISOR_TEST_EXECUTOR_PTY_CASE";
        if let Ok(case) = std::env::var(CASE_ENV) {
            assert_eq!(unsafe { libc::isatty(libc::STDIN_FILENO) }, 1);
            let own_pgrp = unsafe { libc::getpgrp() };
            let mut foreground_child = if case == "background" {
                let child = std::process::Command::new("/bin/sleep")
                    .arg("5")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0)
                    .spawn()
                    .unwrap();
                set_terminal_pgrp(libc::STDIN_FILENO, child.id() as libc::pid_t).unwrap();
                Some(child)
            } else {
                None
            };
            let original_foreground = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
            let foreground_error = (original_foreground < 0).then(std::io::Error::last_os_error);
            if case == "detached" {
                assert_eq!(original_foreground, -1);
                assert_eq!(foreground_error.unwrap().raw_os_error(), Some(libc::ENOTTY));
            } else if case == "foreground" {
                assert_eq!(original_foreground, own_pgrp);
            } else {
                assert_ne!(original_foreground, own_pgrp);
            }

            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let mut spec = RunSpec::process("pty-session-boundary", "test", "/bin/sh");
                    let RunInvocation::Process(process) = &mut spec.invocation;
                    process.args = vec![
                        "-c".into(),
                        if case == "background" {
                            "exit 0".into()
                        } else {
                            "read line; printf '%s' \"$line\"".into()
                        },
                    ];
                    process.stdin = StdioMode::Inherit;
                    process.stdout = StdioMode::Capture;
                    process.stderr = StdioMode::Capture;
                    spec.runtime.timeout_ms = Some(1000);
                    spec.runtime.termination_grace_ms = 25;
                    let handle = crate::PVisor::new().run(spec).await.unwrap();
                    tokio::time::timeout(std::time::Duration::from_secs(2), handle.wait()).await
                });
            let final_foreground = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
            if let Some(child) = foreground_child.as_mut() {
                set_terminal_pgrp(libc::STDIN_FILENO, own_pgrp).unwrap();
                let _ = child.kill();
                child.wait().unwrap();
            }
            assert_eq!(
                final_foreground, original_foreground,
                "terminal ownership changed"
            );
            let result = result.expect("PTY Run did not finish").unwrap();
            if case == "background" {
                assert_eq!(result.state, RunState::Failed);
                assert_eq!(result.exit_code, None);
                assert_eq!(result.executor_observations.termination_signal, None);
                let failure = result.failure.unwrap();
                assert_eq!(failure.kind, RunFailureKind::Infrastructure);
                assert!(failure.message.contains("background process group"));
            } else {
                assert_eq!(result.state, RunState::Completed, "{:?}", result.failure);
                assert_eq!(result.exit_code, Some(0));
                assert_eq!(result.output.stdout.as_deref(), Some("ready"));
            }
            return;
        }

        // Re-exec the test after fork/setsid, rather than starting a Rust runtime
        // in a forked multi-threaded test process or changing its shared stdin.
        for case in ["detached", "foreground", "background"] {
            let mut master = -1;
            let mut slave = -1;
            assert_eq!(
                unsafe {
                    libc::openpty(
                        &mut master,
                        &mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                },
                0
            );
            // Keep only the parent holding the master across exec.
            assert_eq!(
                unsafe { libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC) },
                0
            );
            let mut master = unsafe { std::fs::File::from_raw_fd(master) };
            let slave = unsafe { std::fs::File::from_raw_fd(slave) };
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "executor::process::tests::process_executor_respects_pty_session_boundaries",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CASE_ENV, case)
                .stdin(slave)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            unsafe {
                command.pre_exec(move || {
                    if libc::setsid() < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if case != "detached"
                        && libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = command.spawn().unwrap();
            master.write_all(b"ready\n").unwrap();
            let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
            assert!(flags >= 0);
            assert_eq!(
                unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
                0
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while child.try_wait().unwrap().is_none() {
                // Drain terminal echo while the child closes its slave. Darwin
                // can wait for queued terminal output during process exit.
                let mut echo = [0; 1024];
                loop {
                    match master.read(&mut echo) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) =>
                        {
                            break;
                        }
                        Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                        Err(error) => panic!("PTY echo read failed: {error}"),
                    }
                }
                if std::time::Instant::now() >= deadline {
                    drop(master);
                    let _ = child.kill();
                    let output = child.wait_with_output().unwrap();
                    panic!("PTY case {case} timed out: {output:?}");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "PTY case {case}: {output:?}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_exit_status_preserves_signal_codes_and_failure_semantics() {
        for (script, expected_code, expected_signal) in [
            ("exit 0", 0, None),
            ("exit 7", 7, None),
            ("exit 129", 129, None),
            ("exit 130", 130, None),
            ("exit 143", 143, None),
            ("kill -HUP $$", 128 + libc::SIGHUP, Some(libc::SIGHUP)),
            ("kill -INT $$", 128 + libc::SIGINT, Some(libc::SIGINT)),
            ("kill -TERM $$", 128 + libc::SIGTERM, Some(libc::SIGTERM)),
            ("kill -KILL $$", 128 + libc::SIGKILL, Some(libc::SIGKILL)),
        ] {
            let mut spec = RunSpec::process("process-exit-status", "test", "/bin/sh");
            let RunInvocation::Process(process) = &mut spec.invocation;
            process.args = vec!["-c".into(), script.into()];
            process.stdout = StdioMode::Capture;
            process.stderr = StdioMode::Capture;
            spec.runtime.termination_grace_ms = 25;
            let handle = crate::PVisor::new().run(spec).await.unwrap();
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle.wait())
                .await
                .expect("process exit status was not published")
                .unwrap();
            assert_eq!(result.exit_code, Some(expected_code), "{script}");
            assert_eq!(
                result.executor_observations.termination_signal, expected_signal,
                "{script}"
            );
            if expected_code == 0 {
                assert_eq!(result.state, RunState::Completed);
                assert!(result.failure.is_none());
            } else {
                assert_eq!(result.state, RunState::Failed, "{script}");
                assert_eq!(result.failure.unwrap().kind, RunFailureKind::ProcessExit);
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn requested_cancellation_is_not_a_signal_exit_failure() {
        let mut spec = RunSpec::process("process-cancel-status", "test", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "while :; do sleep 1; done".into()];
        process.stdout = StdioMode::Capture;
        process.stderr = StdioMode::Capture;
        spec.runtime.termination_grace_ms = 25;
        let mut handle = crate::PVisor::new().run(spec).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while handle.status().state != RunState::Running {
                handle
                    .status_changed()
                    .await
                    .expect("Run ended before startup");
            }
        })
        .await
        .expect("process did not start");
        handle.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle.wait())
            .await
            .expect("cancelled process did not finish")
            .unwrap();
        assert_eq!(result.state, RunState::Cancelled);
        assert_eq!(result.exit_code, None);
        assert_eq!(result.executor_observations.termination_signal, None);
        assert!(result.failure.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_finishes_with_background_pipes_after_exit_or_deadline() {
        for (script, expected_state) in [
            ("printf ready; sleep 5 & exit 0", RunState::Completed),
            (
                "(trap '' TERM; printf ready; sleep 5) & wait",
                RunState::Failed,
            ),
        ] {
            let mut spec = RunSpec::process("process-cleanup", "test", "/bin/sh");
            let RunInvocation::Process(process) = &mut spec.invocation;
            process.args = vec!["-c".into(), script.into()];
            process.stdout = StdioMode::Capture;
            process.stderr = StdioMode::Capture;
            spec.runtime.timeout_ms = Some(150);
            spec.runtime.termination_grace_ms = 25;
            let handle = crate::PVisor::new().run(spec).await.unwrap();
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle.wait())
                .await
                .expect("Run waited for a background pipe after leader exit")
                .unwrap();
            assert_eq!(result.state, expected_state);
            assert_eq!(result.executor_observations.termination_signal, None);
            assert_eq!(result.output.stdout.as_deref(), Some("ready"));
            if expected_state == RunState::Failed {
                assert_eq!(
                    result.failure.unwrap().kind,
                    RunFailureKind::DeadlineExceeded
                );
            }
        }
    }

    #[tokio::test]
    async fn stopped_output_drain_preserves_captured_bytes() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(64);
        let stop = tokio_util::sync::CancellationToken::new();
        let capture = tokio::spawn(read_limited(reader, 64, stop.clone()));
        writer.write_all(b"partial").await.unwrap();
        tokio::task::yield_now().await;
        stop.cancel();
        let captured = capture.await.unwrap().unwrap();
        assert_eq!(captured.text, "partial");
        assert!(captured.truncated);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_membership_path_rejects_non_normal_components() {
        assert!(validate_cgroup_relative_path(Path::new("user.slice/session.scope")).is_ok());
        assert!(validate_cgroup_relative_path(Path::new("")).is_ok());
        assert!(validate_cgroup_relative_path(Path::new("../escape")).is_err());
        assert!(validate_cgroup_relative_path(Path::new("/absolute")).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_process_receives_requested_open_file_limit() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "ulimit -n"]);
        command.stdout(Stdio::piped());
        install_resource_limit_hook(
            &mut command,
            ResourceLimits {
                open_files: Some(32),
                ..ResourceLimits::default()
            },
        );
        let output = command.output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "32");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rootless_executor_reports_an_honest_partial_boundary() {
        let executor =
            ProcessExecutor::rootless_with_launcher(std::env::current_exe().unwrap()).unwrap();
        let descriptor = executor.descriptor();
        assert_eq!(descriptor.name, "local-rootless-v1");
        assert_eq!(descriptor.isolation, IsolationKind::RootlessProcess);
        assert!(
            descriptor
                .capability_plan
                .is_planned(CapabilityDimension::FilesystemRead)
        );
        assert!(
            descriptor
                .capability_plan
                .is_planned(CapabilityDimension::FilesystemWrite)
        );
        assert!(
            !descriptor
                .capability_plan
                .is_planned(CapabilityDimension::Network)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_executor_reports_write_confinement_without_overclaiming_capabilities() {
        let executor =
            ProcessExecutor::seatbelt_with_launcher(std::env::current_exe().unwrap()).unwrap();
        let descriptor = executor.descriptor();
        assert_eq!(descriptor.name, "local-seatbelt-v1");
        assert_eq!(descriptor.isolation, IsolationKind::SandboxedProcess);
        assert!(
            !descriptor
                .capability_plan
                .is_planned(CapabilityDimension::FilesystemRead)
        );
        assert!(
            descriptor
                .capability_plan
                .is_planned(CapabilityDimension::FilesystemWrite)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_plan_and_scratch_override_an_untrusted_environment() {
        let temporary = tempfile::tempdir().unwrap();
        let mut spec = RunSpec::process("run", "agent", "/usr/bin/true");
        {
            let RunInvocation::Process(invocation) = &mut spec.invocation;
            invocation.cwd = Some(temporary.path().display().to_string());
            invocation.inherit_env = false;
            invocation
                .env
                .insert(SANDBOX_PLAN_ENV.into(), r#"{"attestation":"/"}"#.into());
            invocation.env.insert("TMPDIR".into(), "/".into());
        }

        let executor =
            ProcessExecutor::seatbelt_with_launcher(std::env::current_exe().unwrap()).unwrap();
        let RunInvocation::Process(invocation) = &spec.invocation;
        let prepared = executor.spawn_command(&spec, invocation).unwrap();
        let environment = prepared
            .command
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.unwrap().to_string_lossy().into_owned(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let plan: SeatbeltPlan =
            serde_json::from_str(environment.get(SANDBOX_PLAN_ENV).unwrap()).unwrap();
        assert_ne!(plan.attestation, PathBuf::from("/"));
        assert_ne!(environment.get("TMPDIR").map(String::as_str), Some("/"));
        assert!(prepared.resources.scratch_path().unwrap().is_dir());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_grants_state_writes_only_for_ordinary_unstaged_runs() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let home = temp.path().join("home");
        let codex = temp.path().join("external-codex");
        for path in [&workspace, &home, &codex] {
            std::fs::create_dir(path).unwrap();
        }
        let home = home.canonicalize().unwrap();
        let codex = codex.canonicalize().unwrap();
        let mut spec = RunSpec::process("run", "agent", "/usr/bin/true");
        spec.capabilities.network = NetworkCapability::Deny;
        let RunInvocation::Process(invocation) = &mut spec.invocation;
        invocation.cwd = Some(workspace.display().to_string());
        invocation.inherit_env = false;
        invocation
            .env
            .insert("HOME".into(), home.display().to_string());
        invocation
            .env
            .insert("CODEX_HOME".into(), codex.display().to_string());
        let executor =
            ProcessExecutor::seatbelt_with_launcher(std::env::current_exe().unwrap()).unwrap();
        for (safe, staged) in [(false, false), (false, true), (true, false), (true, true)] {
            spec.metadata.insert(
                crate::executor::sandbox::REQUIRED_SANDBOX_KEY.into(),
                safe.into(),
            );
            spec.metadata.insert(
                "pvisor.runtime.implant".into(),
                serde_json::json!({
                    "overlay_merged": staged.then_some(workspace.display().to_string()),
                }),
            );
            let RunInvocation::Process(invocation) = &spec.invocation;
            let prepared = executor.spawn_command(&spec, invocation).unwrap();
            let writable = prepared
                .command
                .as_std()
                .get_args()
                .filter_map(|arg| arg.to_str())
                .filter(|arg| arg.starts_with("-DPVISOR_WRITABLE_"))
                .filter_map(|arg| arg.split_once('=').map(|(_, path)| PathBuf::from(path)))
                .collect::<Vec<_>>();
            for path in [&home, &codex] {
                assert_eq!(
                    writable.contains(path),
                    !safe && !staged,
                    "safe={safe} staged={staged} path={path:?}"
                );
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn read_only_grants_reject_writable_ancestors_and_symlink_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let shared = temp.path().join("shared");
        let separate = temp.path().join("separate");
        std::fs::create_dir(&shared).unwrap();
        std::fs::create_dir(&separate).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&shared, &alias).unwrap();
        let mut spec = RunSpec::process("run", "agent", "/bin/true");
        spec.capabilities
            .filesystem
            .push(pvisor_core::FilesystemCapability {
                path: alias.display().to_string(),
                access: FilesystemAccess::Read,
            });
        assert!(check_read_only_grants(&spec, temp.path(), &[separate]).is_ok());
        assert!(check_read_only_grants(&spec, temp.path(), &[shared]).is_err());
        assert!(check_read_only_grants(&spec, temp.path(), &[temp.path().to_owned()]).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rootless_plan_writes_through_normally_and_stages_state_for_safe_shells() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("home");
        let codex_home = home.join(".codex");
        let workspace = home.join("project");
        let merged = temporary.path().join("merged");
        for path in [&codex_home, &workspace, &merged] {
            std::fs::create_dir_all(path).unwrap();
        }
        let mut spec = RunSpec::process("run", "bash", "/bin/bash");
        let RunInvocation::Process(invocation) = &mut spec.invocation;
        invocation.inherit_env = false;
        invocation.cwd = Some(merged.display().to_string());
        invocation
            .env
            .insert("HOME".into(), home.display().to_string());
        invocation
            .env
            .insert("CODEX_HOME".into(), codex_home.display().to_string());
        let RunInvocation::Process(invocation) = &spec.invocation;

        let normal = rootless_plan(
            &spec,
            invocation,
            Path::new("/bin/bash"),
            temporary.path().join("root"),
            temporary.path().join("attestation"),
            NetworkIsolation::Ambient,
            true,
        )
        .unwrap();
        assert!(normal.read_write.contains(&PathBuf::from("/")));
        assert!(normal.read_write.contains(&home));
        assert!(normal.staged_roots.is_empty());

        spec.metadata.insert(
            crate::executor::sandbox::REQUIRED_SANDBOX_KEY.into(),
            true.into(),
        );
        spec.metadata.insert(
            "pvisor.workspace".into(),
            workspace.display().to_string().into(),
        );
        spec.metadata.insert(
            "pvisor.runtime.implant".into(),
            serde_json::json!({"overlay_merged": merged.display().to_string()}),
        );
        let RunInvocation::Process(invocation) = &spec.invocation;
        let safe = rootless_plan(
            &spec,
            invocation,
            Path::new("/bin/bash"),
            temporary.path().join("root"),
            temporary.path().join("attestation"),
            NetworkIsolation::Ambient,
            true,
        )
        .unwrap();
        assert!(!safe.read_write.contains(&PathBuf::from("/")));
        assert_eq!(safe.staged_roots, vec![home]);
        assert_eq!(safe.staged_workspace, Some(workspace));
        assert_eq!(safe.staged_workspace_source, Some(merged));
        assert!(safe.filesystem_isolated);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rootless_plan_is_reserved_even_when_the_run_clears_or_poisons_its_environment() {
        let temporary = tempfile::tempdir().unwrap();
        let mut spec = RunSpec::process("run", "agent", "/bin/true");
        {
            let RunInvocation::Process(invocation) = &mut spec.invocation;
            invocation.cwd = Some(temporary.path().display().to_string());
            invocation.inherit_env = false;
            invocation
                .env
                .insert(SANDBOX_PLAN_ENV.into(), r#"{"read_write":["/"]}"#.into());
        }

        let executor =
            ProcessExecutor::rootless_with_launcher(std::env::current_exe().unwrap()).unwrap();
        let RunInvocation::Process(invocation) = &spec.invocation;
        let command = executor.spawn_command(&spec, invocation).unwrap();
        let plan_path = command
            .command
            .as_std()
            .get_envs()
            .find_map(|(key, value)| {
                (key == SANDBOX_PLAN_ENV).then(|| value.unwrap().to_string_lossy().into_owned())
            })
            .expect("trusted sandbox plan must survive env_clear");
        let encoded = std::fs::read_to_string(&plan_path).unwrap();
        let plan: SandboxPlan = serde_json::from_str(&encoded).unwrap();
        assert_eq!(plan.cwd, temporary.path().canonicalize().unwrap());
        assert_ne!(encoded, r#"{"read_write":["/"]}"#);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rootless_plan_can_keep_host_filesystem_access_for_network_only_runs() {
        let temporary = tempfile::tempdir().unwrap();
        let mut spec = RunSpec::process("run", "agent", "/bin/true");
        spec.metadata.insert(
            "pvisor.filesystem.mode".into(),
            serde_json::Value::String("host".into()),
        );
        {
            let RunInvocation::Process(invocation) = &mut spec.invocation;
            invocation.cwd = Some(temporary.path().display().to_string());
        }

        let executor =
            ProcessExecutor::rootless_with_launcher(std::env::current_exe().unwrap()).unwrap();
        let RunInvocation::Process(invocation) = &spec.invocation;
        let command = executor.spawn_command(&spec, invocation).unwrap();
        let plan_path = command
            .command
            .as_std()
            .get_envs()
            .find_map(|(key, value)| {
                (key == SANDBOX_PLAN_ENV).then(|| value.unwrap().to_string_lossy().into_owned())
            })
            .unwrap();
        let encoded = std::fs::read_to_string(plan_path).unwrap();
        let plan: SandboxPlan = serde_json::from_str(&encoded).unwrap();
        assert!(!plan.filesystem_isolated);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn sandbox_launcher_preserves_symlink_argv0_with_an_untrusted_environment() {
        let temporary = tempfile::tempdir().unwrap();
        let alias = temporary.path().join("sh");
        std::os::unix::fs::symlink("/bin/sh", &alias).unwrap();
        let mut spec = RunSpec::process("run", "agent", alias.to_str().unwrap());
        {
            let RunInvocation::Process(invocation) = &mut spec.invocation;
            invocation.args = vec!["-c".into(), "exit 0".into()];
            invocation.cwd = Some(temporary.path().display().to_string());
            invocation.inherit_env = false;
            invocation
                .env
                .insert(SANDBOX_ARG0_ENV.into(), "wrong-applet".into());
        }

        #[cfg(target_os = "linux")]
        let executor =
            ProcessExecutor::rootless_with_launcher(std::env::current_exe().unwrap()).unwrap();
        #[cfg(target_os = "macos")]
        let executor =
            ProcessExecutor::seatbelt_with_launcher(std::env::current_exe().unwrap()).unwrap();
        let RunInvocation::Process(invocation) = &spec.invocation;
        let prepared = executor.spawn_command(&spec, invocation).unwrap();
        let command = prepared.command.as_std();
        let arg0 = command
            .get_envs()
            .find_map(|(key, value)| (key == SANDBOX_ARG0_ENV).then(|| value.unwrap()))
            .expect("trusted launcher argv[0] must survive the run environment");
        assert_eq!(arg0, alias.as_os_str());
        let arguments = command.get_args().collect::<Vec<_>>();
        assert_eq!(
            arguments[arguments.len() - 3],
            alias.canonicalize().unwrap()
        );
        assert_eq!(arguments[arguments.len() - 2], "-c");
        assert_eq!(arguments[arguments.len() - 1], "exit 0");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn rootless_executor_fails_closed_off_linux() {
        let error = ProcessExecutor::rootless_with_launcher("pvisor").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    }

    #[cfg(unix)]
    #[test]
    fn resolves_bare_program_before_overlay_cwd_is_applied() {
        let resolved = resolve_host_program("sh");
        assert!(
            resolved.is_absolute(),
            "resolved path: {}",
            resolved.display()
        );
        assert!(
            is_executable(&resolved),
            "resolved path: {}",
            resolved.display()
        );
        assert_eq!(
            resolved.file_name().and_then(|name| name.to_str()),
            Some("sh")
        );
        let path = std::env::var_os("PATH").expect("test requires PATH");
        assert!(
            std::env::split_paths(&path).any(|directory| directory.join("sh") == resolved),
            "{} was not resolved from PATH",
            resolved.display()
        );
        assert_eq!(
            resolve_host_program("./agent-script"),
            std::path::PathBuf::from("./agent-script")
        );
    }
}
