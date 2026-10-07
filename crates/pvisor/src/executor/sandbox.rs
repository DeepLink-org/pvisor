//! Platform sandbox launchers used by the local process executor.
//!
//! The launcher is a hidden self-exec mode of the `pvisor` binary. On Linux it
//! installs namespaces and Landlock before Agent code starts. On macOS it is
//! entered only after `/usr/bin/sandbox-exec` has installed a generated
//! Seatbelt profile and records an attestation before replacing itself with
//! the Agent.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::PathBuf;

pub const REQUIRED_SANDBOX_KEY: &str = "pvisor.sandbox.required";
pub const LANDLOCK_SANDBOX_KEY: &str = "pvisor.sandbox.landlock";
pub(crate) const SANDBOX_PROXY_KEY: &str = "pvisor.sandbox.proxy";
pub(crate) const SANDBOX_HIDDEN_PATHS_KEY: &str = "pvisor.sandbox.hidden_paths";
pub(crate) const SANDBOX_NO_GPU_KEY: &str = "pvisor.sandbox.no_gpu";

pub(crate) fn sandbox_required(spec: &pvisor_core::RunSpec) -> bool {
    spec.metadata
        .get(REQUIRED_SANDBOX_KEY)
        .and_then(serde_json::Value::as_bool)
        == Some(true)
}

pub(crate) fn landlock_required(spec: &pvisor_core::RunSpec) -> bool {
    spec.metadata
        .get(LANDLOCK_SANDBOX_KEY)
        .and_then(serde_json::Value::as_bool)
        == Some(true)
}

pub(crate) const INTERNAL_SANDBOX_ARG: &str = "__pvisor-sandbox-exec";
pub(crate) const SANDBOX_PLAN_ENV: &str = "PVISOR_INTERNAL_SANDBOX_PLAN";
/// Original Agent argv[0] preserved across the canonicalizing launcher.
///
/// Linux distributions such as Alpine commonly expose commands as symlinks
/// to BusyBox.  The launcher must execute the canonical inode for its
/// filesystem setup, while still presenting the symlink's basename to the
/// child so BusyBox selects the requested applet.
pub(crate) const SANDBOX_ARG0_ENV: &str = "PVISOR_INTERNAL_SANDBOX_ARG0";
/// Reserved launcher exit status: setup failed before the Agent was executed.
#[doc(hidden)]
pub const SANDBOX_SETUP_EXIT_CODE: i32 = 125;
pub(crate) const SANDBOX_SETUP_FAILED_WARNING: &str = "pvisor.sandbox.setup_failed";

#[cfg(target_os = "macos")]
pub(crate) const MACOS_SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
#[cfg(target_os = "macos")]
pub(crate) const SEATBELT_ATTESTATION: &[u8] = b"pvisor-seatbelt-ready-v1\n";
#[cfg(target_os = "linux")]
pub(crate) const ROOTLESS_ATTESTATION: &[u8] = b"pvisor-rootless-ready-v1\n";

#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_EXECUTE: u64 = 1 << 0;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_READ_FILE: u64 = 1 << 2;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_READ_DIR: u64 = 1 << 3;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_TRUNCATE: u64 = 1 << 14;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_V1: u64 = (1 << 13) - 1;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_V2: u64 = (1 << 14) - 1;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_V3: u64 = (1 << 15) - 1;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_READ: u64 =
    LANDLOCK_ACCESS_FS_EXECUTE | LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum NetworkIsolation {
    Ambient,
    LoopbackOnly,
    /// Only the supervisor-owned proxy may receive IP traffic. None denies all IP.
    ProxyOnly(Option<std::net::SocketAddr>),
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl NetworkIsolation {
    pub(crate) const fn is_loopback_only(self) -> bool {
        matches!(self, Self::LoopbackOnly | Self::ProxyOnly(_))
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SandboxPlan {
    pub root: PathBuf,
    pub cwd: PathBuf,
    pub attestation: PathBuf,
    pub read_only: Vec<PathBuf>,
    pub read_write: Vec<PathBuf>,
    #[serde(default)]
    pub staged_roots: Vec<PathBuf>,
    #[serde(default)]
    pub staged_workspace: Option<PathBuf>,
    #[serde(default)]
    pub staged_workspace_source: Option<PathBuf>,
    pub network: NetworkIsolation,
    /// Whether the launcher should construct the synthetic root and install
    /// Landlock. Network isolation can run independently of this policy.
    #[serde(default = "default_filesystem_isolated")]
    pub filesystem_isolated: bool,
    /// Applied after the private PID namespace is initialized so the trusted
    /// launcher itself can still create its init/reaper process.
    #[serde(default)]
    pub process_limit: Option<u64>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
const fn default_filesystem_isolated() -> bool {
    true
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SeatbeltPlan {
    pub restrict_reads: bool,
    pub attestation: PathBuf,
    pub network: NetworkIsolation,
    #[serde(default = "default_filesystem_isolated")]
    pub filesystem_isolated: bool,
}

/// Enter the hidden launcher when the first argument is the internal marker.
///
/// Returns `Ok(false)` for an ordinary pVisor invocation.  A successful
/// sandbox invocation never returns because it supervises or replaces itself
/// with the Agent.
#[doc(hidden)]
pub fn run_internal_if_requested() -> anyhow::Result<bool> {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new(INTERNAL_SANDBOX_ARG)) {
        return Ok(false);
    }
    run_internal()?;
    Ok(true)
}

#[cfg(target_os = "linux")]
fn run_internal() -> anyhow::Result<()> {
    use anyhow::{Context, bail};
    use std::os::unix::process::CommandExt;

    let plan_path = std::env::var(SANDBOX_PLAN_ENV).context("missing rootless sandbox plan")?;
    let encoded = std::fs::read(&plan_path).context("read rootless sandbox plan")?;
    let plan: SandboxPlan =
        serde_json::from_slice(&encoded).context("decode rootless sandbox plan")?;
    let mut arguments = std::env::args_os().skip(2);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
        bail!("invalid internal rootless sandbox invocation");
    }
    let program = arguments
        .next()
        .context("rootless sandbox invocation is missing the Agent executable")?;
    let arguments = arguments.collect::<Vec<_>>();
    let arg0 = std::env::var_os(SANDBOX_ARG0_ENV).unwrap_or_else(|| program.clone());

    enter_rootless_namespaces(plan.network).context("initialize rootless namespaces")?;
    if plan.filesystem_isolated {
        enter_child_pid_namespace().context("initialize private PID namespace")?;
    }
    if let Some(limit) = plan.process_limit {
        apply_process_limit(limit).context("apply Agent process limit")?;
    }
    // Open the parent-owned inode before chroot/Landlock. The descriptor is
    // retained only by trusted setup code and closed before Agent execution,
    // so no attestation pathname needs to be projected into the sandbox.
    let mut attestation = std::fs::OpenOptions::new()
        .write(true)
        .open(&plan.attestation)
        .with_context(|| {
            format!(
                "open rootless setup attestation {}",
                plan.attestation.display()
            )
        })?;
    let mut plan = plan;
    if plan.filesystem_isolated {
        enter_synthetic_root(&plan).context("construct private sandbox root")?;
        // The private tmpfs created by `enter_synthetic_root` is writable by
        // the Agent, but must also be present in the Landlock allowlist. This
        // uses the host-side mount path because rules are installed before
        // chroot.
        plan.read_write.push(PathBuf::from("/tmp"));
        plan.read_write.push(PathBuf::from("/dev/shm"));
        if let Some(runtime) = private_runtime_dconf_path() {
            plan.read_write.push(runtime);
        }
    }
    std::env::set_current_dir(&plan.cwd)
        .with_context(|| format!("enter sandbox workspace {}", plan.cwd.display()))?;

    // Enumerating /proc/self/fd must happen before Landlock intentionally
    // removes access to the host procfs tree.
    close_unexpected_file_descriptors(Some(attestation.as_raw_fd()))
        .context("close inherited file descriptors")?;

    if !plan.filesystem_isolated {
        // Network-only runs deliberately do not enter CLONE_NEWPID. Exec the
        // Agent in the launcher's existing process group so the outer
        // ProcessExecutor can terminate that group without the PID-namespace
        // supervisor's kill(-1) semantics reaching unrelated host processes.
        drop_process_capabilities().context("drop namespace capabilities")?;
        // The child process is configuring its environment immediately before
        // exec; no concurrent environment mutation occurs in this scope.
        unsafe {
            std::env::remove_var(SANDBOX_PLAN_ENV);
            std::env::remove_var(SANDBOX_ARG0_ENV);
            std::env::set_var("PVISOR_SANDBOX_FILESYSTEM", "host");
            std::env::remove_var("PVISOR_SANDBOX_LANDLOCK_ABI");
            std::env::set_var("PVISOR_SANDBOX_USER_NAMESPACE", "1");
            std::env::set_var(
                "PVISOR_SANDBOX_NETWORK",
                if matches!(plan.network, NetworkIsolation::ProxyOnly(Some(_))) {
                    "proxy-only"
                } else if plan.network.is_loopback_only() {
                    "deny"
                } else {
                    "ambient"
                },
            );
        }
        write_rootless_attestation(&mut attestation)
            .context("record installed rootless network controls")?;
        drop(attestation);
        let mut command = std::process::Command::new(program);
        command.args(arguments).arg0(arg0);
        return Err(command.exec().into());
    }

    supervise_pid_namespace(program, arguments, arg0, attestation, &plan)
}

#[cfg(target_os = "macos")]
fn run_internal() -> anyhow::Result<()> {
    use anyhow::{Context, bail};
    use std::io::Write;
    use std::os::unix::process::CommandExt;

    let encoded = std::env::var(SANDBOX_PLAN_ENV).context("missing Seatbelt sandbox plan")?;
    let plan: SeatbeltPlan =
        serde_json::from_str(&encoded).context("decode Seatbelt sandbox plan")?;
    let mut arguments = std::env::args_os().skip(2);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
        bail!("invalid internal Seatbelt sandbox invocation");
    }
    let program = arguments
        .next()
        .context("Seatbelt sandbox invocation is missing the Agent executable")?;
    let arguments = arguments.collect::<Vec<_>>();
    let arg0 = std::env::var_os(SANDBOX_ARG0_ENV).unwrap_or_else(|| program.clone());

    // The parent keeps the already-open inode and checks these bytes after the
    // process exits. Unlinking before Agent execution keeps the random path and
    // its narrow write grant out of the Agent-visible filesystem namespace.
    let mut attestation = std::fs::OpenOptions::new()
        .write(true)
        .open(&plan.attestation)
        .with_context(|| {
            format!(
                "open Seatbelt setup attestation {}",
                plan.attestation.display()
            )
        })?;
    attestation
        .write_all(SEATBELT_ATTESTATION)
        .context("write Seatbelt setup attestation")?;
    attestation
        .sync_data()
        .context("sync Seatbelt setup attestation")?;
    drop(attestation);
    std::fs::remove_file(&plan.attestation).with_context(|| {
        format!(
            "unlink Seatbelt setup attestation {}",
            plan.attestation.display()
        )
    })?;

    if plan.restrict_reads {
        // Do not retain host files or sockets opened before Seatbelt was installed.
        close_unexpected_file_descriptors(None).context("close inherited file descriptors")?;
    }

    // The child process is configuring its environment immediately before
    // exec; no concurrent environment mutation occurs in this scope.
    unsafe {
        std::env::remove_var(SANDBOX_PLAN_ENV);
        std::env::remove_var(SANDBOX_ARG0_ENV);
        std::env::set_var(
            "PVISOR_SANDBOX_FILESYSTEM",
            if plan.restrict_reads {
                "seatbelt-read-write"
            } else if plan.filesystem_isolated {
                "seatbelt-write"
            } else {
                "host"
            },
        );
        std::env::set_var(
            "PVISOR_SANDBOX_NETWORK",
            if matches!(plan.network, NetworkIsolation::ProxyOnly(Some(_))) {
                "proxy-only"
            } else if plan.network.is_loopback_only() {
                "deny"
            } else {
                "ambient"
            },
        );
    }

    let mut command = std::process::Command::new(program);
    command.args(arguments).arg0(arg0);
    Err(command.exec().into())
}

#[cfg(target_os = "linux")]
pub(crate) fn landlock_runtime_available() -> bool {
    const CREATE_RULESET_VERSION: libc::c_uint = 1;
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0,
            CREATE_RULESET_VERSION,
        )
    };
    abi >= 1
}

#[cfg(target_os = "linux")]
fn install_landlock(plan: &SandboxPlan) -> std::io::Result<u32> {
    use std::io::{Error, ErrorKind};

    // Calling the small stable kernel ABI directly keeps this launcher
    // dependency-free. Each kernel must only receive the access bits introduced
    // by the ABI it implements: v2 adds REFER and v3 adds TRUNCATE.
    const CREATE_RULESET_VERSION: libc::c_uint = 1;
    const RULE_PATH_BENEATH: libc::c_int = 1;
    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
    }

    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0,
            CREATE_RULESET_VERSION,
        )
    };
    if abi < 0 {
        return Err(Error::last_os_error());
    }
    if abi < 1 {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!("Landlock ABI v1 or newer is required; kernel provides v{abi}"),
        ));
    }

    let handled_access_fs = landlock_access_fs_for_abi(abi as u32);

    let attr = RulesetAttr { handled_access_fs };
    let ruleset_fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr,
            std::mem::size_of::<RulesetAttr>(),
            0,
        )
    } as libc::c_int;
    if ruleset_fd < 0 {
        return Err(Error::last_os_error());
    }
    let ruleset = OwnedFd(ruleset_fd);

    for path in &plan.read_only {
        add_landlock_path_rule(ruleset.0, path, LANDLOCK_ACCESS_FS_READ, RULE_PATH_BENEATH)
            .map_err(|error| {
                Error::new(
                    error.kind(),
                    format!("add read-only rule for {}: {error}", path.display()),
                )
            })?;
    }
    for path in &plan.read_write {
        add_landlock_path_rule(ruleset.0, path, handled_access_fs, RULE_PATH_BENEATH).map_err(
            |error| {
                Error::new(
                    error.kind(),
                    format!("add read-write rule for {}: {error}", path.display()),
                )
            },
        )?;
    }

    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(Error::last_os_error());
    }
    if unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset.0, 0) } != 0 {
        return Err(Error::last_os_error());
    }
    Ok(abi as u32)
}

#[cfg(target_os = "linux")]
const fn landlock_access_fs_for_abi(abi: u32) -> u64 {
    match abi {
        1 => LANDLOCK_ACCESS_FS_V1,
        2 => LANDLOCK_ACCESS_FS_V2,
        _ => LANDLOCK_ACCESS_FS_V3,
    }
}

/// Confine the libkrun VMM process while leaving the pVisor FUSE server in the
/// trusted parent. The VMM gets a private network and mount namespace, may
/// access only its virtio-fs root plus KVM/runtime files, and retains no
/// namespace capabilities after setup.
#[cfg(target_os = "linux")]
pub(crate) fn restrict_krun_runner(
    overlay_read_only: Vec<PathBuf>,
    overlay_read_write: Vec<PathBuf>,
    library_dir: Option<PathBuf>,
) -> anyhow::Result<u32> {
    use anyhow::Context;

    enter_rootless_namespaces(NetworkIsolation::LoopbackOnly)
        .context("initialize libkrun user, mount, and network namespaces")?;
    let mut read_only = [
        "/usr/lib",
        "/usr/lib64",
        "/lib",
        "/lib64",
        "/proc/self",
        "/dev/urandom",
    ]
    .into_iter()
    .map(PathBuf::from)
    .filter(|path| path.exists())
    .collect::<Vec<_>>();
    if let Some(directory) = library_dir {
        read_only.push(directory);
    }
    read_only.extend(overlay_read_only);
    let mut read_write = overlay_read_write;
    if PathBuf::from("/dev/kvm").exists() {
        read_write.push(PathBuf::from("/dev/kvm"));
    }
    let plan = SandboxPlan {
        root: PathBuf::from("/"),
        cwd: PathBuf::from("/"),
        attestation: PathBuf::from("/dev/null"),
        read_only,
        read_write,
        staged_roots: Vec::new(),
        staged_workspace: None,
        staged_workspace_source: None,
        network: NetworkIsolation::LoopbackOnly,
        filesystem_isolated: true,
        process_limit: None,
    };
    let abi = install_landlock(&plan).context("install libkrun Landlock policy")?;
    drop_process_capabilities().context("drop libkrun namespace capabilities")?;
    Ok(abi)
}

#[cfg(target_os = "linux")]
fn add_landlock_path_rule(
    ruleset_fd: libc::c_int,
    path: &std::path::Path,
    allowed_access: u64,
    rule_type: libc::c_int,
) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileTypeExt;

    #[repr(C, packed)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: libc::c_int,
    }

    // Landlock rejects directory-only access bits on a non-directory anchor.
    // Filter the requested access against the anchor's inode type before
    // adding the rule.  Pathname Unix sockets are not governed by Landlock's
    // filesystem rights, so there is no useful rule to add for them.
    let file_type = std::fs::metadata(path)?.file_type();
    let allowed_access = if file_type.is_dir() {
        allowed_access
    } else if file_type.is_file() {
        allowed_access
            & (LANDLOCK_ACCESS_FS_EXECUTE
                | LANDLOCK_ACCESS_FS_WRITE_FILE
                | LANDLOCK_ACCESS_FS_READ_FILE
                | LANDLOCK_ACCESS_FS_TRUNCATE)
    } else if file_type.is_socket() {
        return Ok(());
    } else {
        allowed_access & (LANDLOCK_ACCESS_FS_WRITE_FILE | LANDLOCK_ACCESS_FS_READ_FILE)
    };
    if allowed_access == 0 {
        return Ok(());
    }

    let encoded = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("sandbox path contains a NUL byte: {}", path.display()),
        )
    })?;
    let path_fd = unsafe { libc::open(encoded.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if path_fd < 0 {
        return Err(Error::last_os_error());
    }
    let path_fd = OwnedFd(path_fd);
    let attr = PathBeneathAttr {
        allowed_access,
        parent_fd: path_fd.0,
    };
    if unsafe { libc::syscall(libc::SYS_landlock_add_rule, ruleset_fd, rule_type, &attr, 0) } != 0 {
        return Err(Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
struct OwnedFd(libc::c_int);

#[cfg(target_os = "linux")]
impl Drop for OwnedFd {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.0);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;

    fn topology_fixture(root: &std::path::Path) -> StateRootTopology {
        let bytes = b"1 0 0:1 / / rw - ext4 /dev/root rw\n2 1 0:2 / /state\\040root rw shared:1 - tmpfs state\\011source rw\n3 2 0:3 / /state\\040root/nested\\011\xff rw - tmpfs nested rw\n4 1 0:4 / /state\\040root-sibling rw - tmpfs sibling rw\n";
        select_state_topology(root, parse_state_mounts(bytes).unwrap()).unwrap()
    }

    #[test]
    fn mountinfo_uses_raw_paths_and_strict_component_descendants() {
        use std::os::unix::ffi::OsStrExt;
        let topology = topology_fixture(std::path::Path::new("/state root"));
        assert_eq!(topology.covering.point, PathBuf::from("/state root"));
        assert_eq!(topology.covering.fstype, "tmpfs");
        assert_eq!(topology.covering.source.as_bytes(), b"state\tsource");
        assert_eq!(topology.descendants.len(), 1);
        assert_eq!(
            topology.descendants[0].point.as_os_str().as_bytes(),
            b"/state root/nested\t\xff"
        );
        let topology = topology_fixture(std::path::Path::new("/state root-sibling"));
        assert!(topology.descendants.is_empty(), "exact mount is allowed");
        let topology = topology_fixture(std::path::Path::new("/state"));
        assert!(
            topology.descendants.is_empty(),
            "prefix-only siblings are allowed"
        );
        assert_eq!(topology.covering.point, PathBuf::from("/"));
        assert_eq!(
            mountinfo_unescape(b"a\\134b\\012c").unwrap().as_bytes(),
            b"a\\b\nc"
        );
    }

    #[test]
    fn unrelated_mount_with_literal_carriage_returns_does_not_block_staging() {
        use std::os::unix::ffi::OsStrExt;
        let bytes = b"1 0 0:1 / / rw - ext4 /dev/root rw\n2 1 0:2 / /other\rdir rw - tmpfs source\rname rw\n";
        let mounts = parse_state_mounts(bytes).unwrap();
        assert_eq!(mounts[1].point.as_os_str().as_bytes(), b"/other\rdir");
        assert_eq!(mounts[1].source.as_bytes(), b"source\rname");
        let root = std::path::Path::new("/state");
        let topology = select_state_topology(root, mounts).unwrap();
        assert!(topology.descendants.is_empty());
        assert!(check_state_root_mount(root, Ok(topology), Ok(())).is_ok());
    }

    #[test]
    fn mountinfo_parser_is_bounded_and_rejects_malformed_records() {
        for bytes in [
            b"1 0 0:1 / / rw tmpfs source rw".as_slice(),
            b"1 0 0:1 / /bad\\04 rw - tmpfs source rw",
            b"1 0 0:1 / relative rw - tmpfs source rw",
            b"1 0 0:1 / / rw - tmpfs",
        ] {
            assert!(parse_state_mounts(bytes).is_err(), "{bytes:?}");
        }
        assert!(parse_state_mounts(&vec![b'x'; MOUNTINFO_LIMIT + 1]).is_err());
        assert!(parse_state_mounts(&vec![b'x'; 64 * 1024 + 1]).is_err());
        for bytes in [b"\\", b"\\0".as_slice(), b"\\00", b"\\999", b"\\000"] {
            assert!(mountinfo_unescape(bytes).is_err());
        }
        assert!(select_state_topology(std::path::Path::new("/state"), Vec::new()).is_err());
    }

    #[test]
    fn state_mount_failure_preserves_errno_and_topology_without_fallback() {
        let root = std::path::Path::new("/state root");
        let error = check_state_root_mount(
            root,
            Ok(topology_fixture(root)),
            Err(std::io::Error::from_raw_os_error(libc::EINVAL)),
        )
        .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(libc::EINVAL)
        );
        let message = format!("{error:#}");
        assert!(message.contains(
            "state root mountpoint=\"/state root\" fstype=\"tmpfs\" source=\"state\\tsource\""
        ));
        assert!(message.contains("1 strict descendants"));
        assert!(message.contains("Invalid argument (os error 22)"));
        assert!(check_state_root_mount(root, Ok(topology_fixture(root)), Ok(())).is_err());
        let root = std::path::Path::new("/state root-sibling");
        assert!(check_state_root_mount(root, Ok(topology_fixture(root)), Ok(())).is_ok());
        let error = check_state_root_mount(
            root,
            Err(std::io::Error::other("unreadable mountinfo")),
            Err(std::io::Error::from_raw_os_error(libc::EPERM)),
        )
        .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(libc::EPERM)
        );
        assert!(
            check_state_root_mount(
                root,
                Err(std::io::Error::other("unreadable mountinfo")),
                Ok(())
            )
            .is_err()
        );
    }

    #[test]
    fn landlock_access_mask_matches_negotiated_abi() {
        assert_eq!(landlock_access_fs_for_abi(1), LANDLOCK_ACCESS_FS_V1);
        assert_eq!(landlock_access_fs_for_abi(2), LANDLOCK_ACCESS_FS_V2);
        assert_eq!(landlock_access_fs_for_abi(3), LANDLOCK_ACCESS_FS_V3);
        assert_eq!(landlock_access_fs_for_abi(99), LANDLOCK_ACCESS_FS_V3);
        assert_eq!(LANDLOCK_ACCESS_FS_V2, LANDLOCK_ACCESS_FS_V1 | (1 << 13));
        assert_eq!(
            LANDLOCK_ACCESS_FS_V3,
            LANDLOCK_ACCESS_FS_V2 | LANDLOCK_ACCESS_FS_TRUNCATE
        );
    }

    #[test]
    fn namespace_errors_preserve_stage_and_os_error() {
        let error = with_io_context(
            "unshare mount namespace",
            std::io::Error::from_raw_os_error(libc::EPERM),
        );
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            error.to_string(),
            "unshare mount namespace: Operation not permitted (os error 1)"
        );
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn run_internal() -> anyhow::Result<()> {
    anyhow::bail!("the local process sandbox is not available on this platform")
}

/// Generate a compatibility-oriented Seatbelt profile.
///
/// Reads remain ambient so ordinary developer toolchains keep working. Every
/// pathname write outside `writable_paths` is denied by Seatbelt. A
/// network-isolated Run starts from `deny default` and admits loopback IP,
/// exact Run-scoped Unix sockets, and sockets rooted in Run-owned directories.
#[cfg(target_os = "macos")]
pub(crate) fn seatbelt_profile(
    writable_paths: &[PathBuf],
    allowed_unix_sockets: &[PathBuf],
    local_socket_roots: &[PathBuf],
    network: NetworkIsolation,
    filesystem_isolated: bool,
) -> std::io::Result<(String, Vec<(String, PathBuf)>)> {
    seatbelt_profile_with_reads(
        writable_paths,
        None,
        allowed_unix_sockets,
        local_socket_roots,
        network,
        filesystem_isolated,
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn seatbelt_profile_with_reads(
    writable_paths: &[PathBuf],
    readable_paths: Option<&[PathBuf]>,
    allowed_unix_sockets: &[PathBuf],
    local_socket_roots: &[PathBuf],
    network: NetworkIsolation,
    filesystem_isolated: bool,
) -> std::io::Result<(String, Vec<(String, PathBuf)>)> {
    use std::io::{Error, ErrorKind};

    let writable_paths = canonical_seatbelt_paths(writable_paths, "writable")?;
    if writable_paths.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Seatbelt requires at least one writable path",
        ));
    }
    if writable_paths
        .iter()
        .any(|path| path == std::path::Path::new("/"))
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "the host root cannot be granted as a Seatbelt writable path",
        ));
    }
    let mut parameters = Vec::with_capacity(writable_paths.len());
    for (index, path) in writable_paths.iter().enumerate() {
        let key = format!("PVISOR_WRITABLE_{index}");
        parameters.push((key, path.clone()));
    }

    if network.is_loopback_only() {
        let allowed_unix_sockets = canonical_seatbelt_paths(allowed_unix_sockets, "Unix socket")?;
        let local_socket_roots = canonical_seatbelt_paths(local_socket_roots, "local socket root")?;
        parameters.reserve(allowed_unix_sockets.len() + local_socket_roots.len());
        for (index, path) in allowed_unix_sockets.iter().enumerate() {
            parameters.push((format!("PVISOR_UNIX_SOCKET_{index}"), path.clone()));
        }
        for (index, path) in local_socket_roots.iter().enumerate() {
            parameters.push((format!("PVISOR_SOCKET_ROOT_{index}"), path.clone()));
        }

        // Deny by default for a network-isolated Run. The allowlist below is
        // intentionally small and mirrors the system services required by
        // shells, language runtimes, PTYs, and read-only preferences. Socket
        // binds/listeners are allowed for Unix IPC; outbound connections require
        // an explicit loopback or Unix-path grant below.
        let mut profile = String::from(
            "(version 1)\n\
             (deny default)\n\
             (allow process-exec)\n\
             (allow process-fork)\n\
             (allow signal (target same-sandbox))\n\
             (allow process-info* (target same-sandbox))\n\
             (allow file-read* file-test-existence file-map-executable)\n\
             (allow sysctl-read)\n\
             (allow system-mac-syscall (mac-policy-name \"vnguard\"))\n\
             (allow system-mac-syscall\n\
               (require-all (mac-policy-name \"Sandbox\") (mac-syscall-number 67)))\n\
             (allow system-fsctl)\n\
             (allow iokit-open (iokit-registry-entry-class \"RootDomainUserClient\"))\n\
             (allow ipc-posix-sem)\n\
             (allow ipc-posix-shm-read*)\n\
             (allow pseudo-tty)\n\
             (allow user-preference-read)\n\
             (allow mach-lookup\n\
               (global-name \"com.apple.system.opendirectoryd.libinfo\")\n\
               (global-name \"com.apple.system.opendirectoryd.membership\")\n\
               (global-name \"com.apple.cfprefsd.daemon\")\n\
               (global-name \"com.apple.cfprefsd.agent\")\n\
               (local-name \"com.apple.cfprefsd.agent\")\n\
               (global-name \"com.apple.PowerManagement.control\"))\n\
             (allow file-ioctl (regex #\"^/dev/ttys[0-9]+$\"))\n\
             (allow system-socket (socket-domain AF_UNIX))\n\
             (allow network-bind network-inbound (local unix-socket))\n\
             (deny network-bind (local ip))\n\
             (deny network-inbound (local ip))\n\
             (deny network-outbound\n\
               (require-all\n\
                 (remote ip)\n\
                 (require-not (remote ip \"localhost:*\"))))\n\
             (allow network-outbound (remote ip \"localhost:*\"))\n",
        );
        if let Some(readable) = readable_paths {
            let readable = canonical_seatbelt_paths(readable, "readable")?;
            if readable
                .iter()
                .any(|path| path == std::path::Path::new("/"))
            {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "required sandbox cannot grant the host root",
                ));
            }
            profile = profile.replace(
                "(allow file-read* file-test-existence file-map-executable)",
                // dyld's libignition opens / as an openat traversal anchor (see
                // Apple's dyld-support.sb). This is literal, never recursive.
                "(allow file-read-metadata)\n(allow file-read* (literal \"/\"))",
            );
            profile.push_str("(allow file-read* file-test-existence file-map-executable\n");
            for (index, path) in readable.into_iter().enumerate() {
                let key = format!("PVISOR_READABLE_{index}");
                profile.push_str(&format!(
                    "  (literal (param \"{key}\")) (subpath (param \"{key}\"))\n"
                ));
                parameters.push((key, path));
            }
            for index in 0..writable_paths.len() {
                profile.push_str(&format!("  (literal (param \"PVISOR_WRITABLE_{index}\")) (subpath (param \"PVISOR_WRITABLE_{index}\"))\n"));
            }
            profile.push_str(")\n");
        }
        if let NetworkIsolation::ProxyOnly(endpoint) = network {
            // Deny all IP except the allocated loopback TCP proxy port; unrelated localhost
            // services must not become alternate egress paths.
            profile = profile.replace("(allow network-outbound (remote ip \"localhost:*\"))", "");
            match endpoint {
                Some(endpoint) if endpoint.ip().is_loopback() && endpoint.port() != 0 => {
                    // A missing bind rule inherits the network-inbound denial.
                    // TCP connect may implicitly bind: permit that operation,
                    // while listen and outbound peers remain restricted.
                    profile = profile.replace(
                        "(deny network-bind (local ip))",
                        "(allow network-bind (local tcp))",
                    );
                    profile = profile.replace(
                        "(remote ip \"localhost:*\")",
                        &format!("(remote tcp \"localhost:{}\")", endpoint.port()),
                    );
                    profile.push_str(&format!(
                        "(allow network-outbound (remote tcp \"localhost:{}\"))\n",
                        endpoint.port()
                    ));
                }
                Some(_) => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "sandbox proxy must be a concrete loopback endpoint",
                    ));
                }
                None => profile.push_str("(deny network-outbound (remote ip))\n"),
            }
        }
        if filesystem_isolated {
            profile.push_str("(allow file-write*\n");
            for index in 0..writable_paths.len() {
                profile.push_str(&format!(
                    "  (literal (param \"PVISOR_WRITABLE_{index}\"))\n\
                     (subpath (param \"PVISOR_WRITABLE_{index}\"))\n"
                ));
            }
            profile.push_str(")\n");
        } else {
            profile.push_str("(allow file-write*)\n");
        }
        // Keep Unix-path grants positive and separate from IP rules. A negated
        // Unix-path deny can also reject permitted TCP connections on macOS.
        for index in 0..allowed_unix_sockets.len() {
            profile.push_str(&format!(
                "(allow network-outbound (remote unix-socket (literal (param \"PVISOR_UNIX_SOCKET_{index}\"))))\n"
            ));
        }
        for index in 0..local_socket_roots.len() {
            profile.push_str(&format!(
                "(allow network-outbound (remote unix-socket (subpath (param \"PVISOR_SOCKET_ROOT_{index}\"))))\n"
            ));
        }
        return Ok((profile, parameters));
    }

    // Starting from `allow default` preserves compatibility with local macOS
    // toolchains. The filtered deny is fail-closed for writes: it matches only
    // when a target is neither an exact writable root nor beneath one.
    let mut profile = String::from(
        "(version 1)\n\
         (allow default)\n\
         (deny file-write*\n\
           (require-all\n",
    );
    for index in 0..writable_paths.len() {
        profile.push_str(&format!(
            "    (require-not (literal (param \"PVISOR_WRITABLE_{index}\")))\n\
             (require-not (subpath (param \"PVISOR_WRITABLE_{index}\")))\n"
        ));
    }
    profile.push_str("  )\n)\n");
    Ok((profile, parameters))
}

#[cfg(target_os = "macos")]
fn canonical_seatbelt_paths(paths: &[PathBuf], kind: &str) -> std::io::Result<Vec<PathBuf>> {
    use std::io::{Error, ErrorKind};

    let mut canonical = paths
        .iter()
        .map(|path| {
            path.canonicalize().map_err(|error| {
                Error::new(
                    error.kind(),
                    format!(
                        "canonicalize Seatbelt {kind} path {}: {error}",
                        path.display()
                    ),
                )
            })
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    canonical.sort_unstable();
    canonical.dedup();
    if canonical.iter().any(|path| path.to_str().is_none()) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Seatbelt {kind} paths must be valid UTF-8"),
        ));
    }
    Ok(canonical)
}

#[cfg(target_os = "linux")]
fn enter_rootless_namespaces(network: NetworkIsolation) -> std::io::Result<()> {
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    if unsafe { libc::unshare(libc::CLONE_NEWUSER) } != 0 {
        return Err(namespace_stage_error("unshare user namespace"));
    }

    // A one-ID identity mapping is sufficient for a local Agent executable and
    // avoids /etc/subuid, newuidmap, and a privileged setup helper.
    match std::fs::write("/proc/self/setgroups", b"deny\n") {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(with_io_context(
                "disable setgroups in user namespace",
                error,
            ));
        }
    }
    std::fs::write("/proc/self/uid_map", format!("{uid} {uid} 1\n"))
        .map_err(|error| with_io_context("write user namespace UID map", error))?;
    std::fs::write("/proc/self/gid_map", format!("{gid} {gid} 1\n"))
        .map_err(|error| with_io_context("write user namespace GID map", error))?;

    if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
        return Err(namespace_stage_error("unshare mount namespace"));
    }
    if network.is_loopback_only() && unsafe { libc::unshare(libc::CLONE_NEWNET) } != 0 {
        return Err(namespace_stage_error("unshare network namespace"));
    }
    if network.is_loopback_only() {
        bring_loopback_up()
            .map_err(|error| with_io_context("enable network namespace loopback", error))?;
    }

    // Never propagate mounts performed by the child back into the host mount
    // namespace.  Landlock later prevents the Agent from changing topology.
    if unsafe {
        libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(namespace_stage_error(
            "set mount namespace root propagation to private",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn bring_loopback_up() -> std::io::Result<()> {
    // A newly-created network namespace starts with only `lo`, administratively
    // down. Enable that interface before dropping capabilities; no route or
    // non-loopback device is created, so children cannot reach the host network.
    #[repr(C)]
    struct Ifreq {
        name: [libc::c_char; libc::IFNAMSIZ],
        flags: libc::c_short,
        _pad: [u8; 22],
    }
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let _guard = OwnedFd(fd);
    let mut ifreq = Ifreq {
        name: [0; libc::IFNAMSIZ],
        flags: 0,
        _pad: [0; 22],
    };
    ifreq.name[0] = b'l' as libc::c_char;
    ifreq.name[1] = b'o' as libc::c_char;
    if unsafe { libc::ioctl(fd, libc::SIOCGIFFLAGS as _, &mut ifreq) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    ifreq.flags |= libc::IFF_UP as libc::c_short | libc::IFF_RUNNING as libc::c_short;
    if unsafe { libc::ioctl(fd, libc::SIOCSIFFLAGS as _, &ifreq) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn namespace_stage_error(stage: &str) -> std::io::Error {
    with_io_context(stage, std::io::Error::last_os_error())
}

#[cfg(target_os = "linux")]
fn with_io_context(stage: &str, error: std::io::Error) -> std::io::Error {
    std::io::Error::new(error.kind(), format!("{stage}: {error}"))
}

#[cfg(target_os = "linux")]
fn enter_child_pid_namespace() -> std::io::Result<()> {
    if unsafe { libc::unshare(libc::CLONE_NEWPID) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn apply_process_limit(processes: u64) -> std::io::Result<()> {
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NPROC, &mut current) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let requested = processes as libc::rlim_t;
    let effective = requested.min(current.rlim_max);
    let limit = libc::rlimit {
        rlim_cur: effective,
        rlim_max: effective,
    };
    if unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn write_rootless_attestation(attestation: &mut std::fs::File) -> std::io::Result<()> {
    use std::io::Write;

    attestation.write_all(ROOTLESS_ATTESTATION)?;
    attestation.sync_data()
}

#[cfg(target_os = "linux")]
fn enter_synthetic_root(plan: &SandboxPlan) -> anyhow::Result<()> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::fs::PermissionsExt;

    if !plan.root.is_absolute() || plan.root == std::path::Path::new("/") {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "sandbox root must be a non-root absolute path: {}",
                plan.root.display()
            ),
        )
        .into());
    }
    if !plan.root.is_dir() {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("sandbox root does not exist: {}", plan.root.display()),
        )
        .into());
    }

    let root = path_cstring(&plan.root)?;
    if unsafe {
        libc::mount(
            c"tmpfs".as_ptr(),
            root.as_ptr(),
            c"tmpfs".as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            c"mode=0755,size=16m".as_ptr().cast(),
        )
    } != 0
    {
        return Err(Error::last_os_error().into());
    }

    // Give the Agent a private temporary directory.  Binding the host /tmp
    // would let a staged Run mutate unrelated host state, while omitting it
    // breaks ordinary tools that need a scratch directory.  This tmpfs is
    // intentionally ephemeral and is not part of the durable workspace
    // OverlayFS stage.
    let tmp = plan.root.join("tmp");
    std::fs::create_dir(&tmp)?;
    let tmp = path_cstring(&tmp)?;
    if unsafe {
        libc::mount(
            c"tmpfs".as_ptr(),
            tmp.as_ptr(),
            c"tmpfs".as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            c"mode=1777,size=64m".as_ptr().cast(),
        )
    } != 0
    {
        return Err(Error::last_os_error().into());
    }

    // Chromium uses POSIX shared memory even when its own sandbox is disabled.
    // Give it a private, ephemeral /dev/shm rather than exposing the host's.
    let dev_shm = plan.root.join("dev/shm");
    std::fs::create_dir_all(&dev_shm)?;
    let dev_shm_mount = path_cstring(&dev_shm)?;
    if unsafe {
        libc::mount(
            c"tmpfs".as_ptr(),
            dev_shm_mount.as_ptr(),
            c"tmpfs".as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            c"mode=1777,size=256m".as_ptr().cast(),
        )
    } != 0
    {
        return Err(Error::last_os_error().into());
    }

    // The synthetic /dev starts empty. Recreate the conventional descriptor
    // links locally instead of bind-mounting the host's /dev/fd magic link,
    // which would remain tied to the setup process's procfs view.
    let dev = plan.root.join("dev");
    for (link, target) in [
        ("fd", "/proc/self/fd"),
        ("stdin", "/proc/self/fd/0"),
        ("stdout", "/proc/self/fd/1"),
        ("stderr", "/proc/self/fd/2"),
        ("ptmx", "pts/ptmx"),
    ] {
        let link = dev.join(link);
        if std::fs::symlink_metadata(&link).is_err() {
            std::os::unix::fs::symlink(target, link)?;
        }
    }

    mount_staged_roots(plan)?;

    // Desktop toolkits sometimes create dconf state below XDG_RUNTIME_DIR.
    // Keep that one writable runtime subdirectory private and ephemeral while
    // still allowing explicitly projected Wayland/D-Bus sockets beside it.
    if let Some(runtime) = private_runtime_dconf_path() {
        let runtime = plan
            .root
            .join(runtime.strip_prefix("/").expect("absolute runtime path"));
        let runtime_root = runtime.parent().expect("dconf path has a runtime parent");
        std::fs::create_dir_all(runtime_root)?;
        std::fs::set_permissions(runtime_root, std::fs::Permissions::from_mode(0o700))?;
        std::fs::create_dir_all(&runtime)?;
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))?;
    }

    // The trusted launcher needs /proc/self/fd to close inherited descriptors.
    // PID 1 replaces this temporary host procfs with a private PID-scoped
    // procfs before it forks or releases the Agent.
    bind_path_into_root(&plan.root, std::path::Path::new("/proc"))?;

    let mut paths = plan
        .read_only
        .iter()
        .chain(&plan.read_write)
        .filter(|path| !path.starts_with(&plan.root) && *path != std::path::Path::new("/"))
        .collect::<Vec<_>>();
    paths.sort_unstable_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });
    paths.dedup();
    for path in paths {
        bind_path_into_root(&plan.root, path)?;
    }
    if let Some(workspace) = &plan.staged_workspace {
        let source = plan.staged_workspace_source.as_ref().ok_or_else(|| {
            std::io::Error::other("staged workspace is missing its merged source")
        })?;
        bind_staged_workspace(&plan.root, source, workspace)?;
    }

    // chroot is safe here because the process has a private mount namespace,
    // no Agent code has run, every non-stdio FD is closed immediately below,
    // and all namespace capabilities are dropped before exec.
    if unsafe { libc::chroot(root.as_ptr()) } != 0 {
        return Err(Error::last_os_error().into());
    }
    if unsafe { libc::chdir(c"/".as_ptr()) } != 0 {
        return Err(Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
const MOUNTINFO_LIMIT: usize = 4 * 1024 * 1024;

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct StateMount {
    point: PathBuf,
    fstype: std::ffi::OsString,
    source: std::ffi::OsString,
}

#[cfg(target_os = "linux")]
fn mountinfo_unescape(field: &[u8]) -> std::io::Result<std::ffi::OsString> {
    use std::os::unix::ffi::OsStringExt;
    let mut decoded = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] == b'\\' {
            let escape = field.get(index + 1..index + 4);
            decoded.push(match escape {
                Some(b"040") => b' ',
                Some(b"011") => b'\t',
                Some(b"012") => b'\n',
                Some(b"134") => b'\\',
                _ => return Err(std::io::Error::other("invalid mountinfo escape")),
            });
            index += 4;
        } else {
            if matches!(field[index], 0 | b'\t') {
                return Err(std::io::Error::other("invalid mountinfo field"));
            }
            decoded.push(field[index]);
            index += 1;
        }
    }
    Ok(std::ffi::OsString::from_vec(decoded))
}

#[cfg(target_os = "linux")]
fn parse_state_mounts(bytes: &[u8]) -> std::io::Result<Vec<StateMount>> {
    if bytes.len() > MOUNTINFO_LIMIT {
        return Err(std::io::Error::other("mountinfo exceeds 4 MiB limit"));
    }
    let mut mounts = Vec::new();
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        if line.len() > 64 * 1024 {
            return Err(std::io::Error::other("mountinfo line exceeds 64 KiB limit"));
        }
        let mut fields = line.split(|byte| *byte == b' ');
        let invalid = || std::io::Error::other("invalid mountinfo record");
        for _ in 0..4 {
            if fields.next().is_none_or(|field| field.is_empty()) {
                return Err(invalid());
            }
        }
        let point = PathBuf::from(mountinfo_unescape(fields.next().ok_or_else(invalid)?)?);
        if !point.is_absolute() || fields.next().is_none_or(|field| field.is_empty()) {
            return Err(invalid());
        }
        if !fields.by_ref().any(|field| field == b"-") {
            return Err(invalid());
        }
        let fstype = mountinfo_unescape(fields.next().ok_or_else(invalid)?)?;
        let source = mountinfo_unescape(fields.next().ok_or_else(invalid)?)?;
        if fstype.is_empty() || source.is_empty() || fields.next().is_none() {
            return Err(invalid());
        }
        mounts.push(StateMount {
            point,
            fstype,
            source,
        });
    }
    Ok(mounts)
}

#[cfg(target_os = "linux")]
struct StateRootTopology {
    covering: StateMount,
    descendants: Vec<StateMount>,
}

#[cfg(target_os = "linux")]
fn select_state_topology(
    root: &std::path::Path,
    mounts: Vec<StateMount>,
) -> std::io::Result<StateRootTopology> {
    let mut covering: Option<StateMount> = None;
    let mut descendants = Vec::new();
    for mount in mounts {
        if mount.point != root && mount.point.starts_with(root) {
            descendants.push(mount);
        } else if root.starts_with(&mount.point)
            && covering.as_ref().is_none_or(|previous| {
                mount.point.components().count() >= previous.point.components().count()
            })
        {
            covering = Some(mount);
        }
    }
    Ok(StateRootTopology {
        covering: covering
            .ok_or_else(|| std::io::Error::other("state root has no covering mountinfo record"))?,
        descendants,
    })
}

#[cfg(target_os = "linux")]
fn state_root_topology(root: &std::path::Path) -> std::io::Result<StateRootTopology> {
    use std::io::Read;
    let root = std::fs::canonicalize(root)?;
    let mut bytes = Vec::new();
    std::fs::File::open("/proc/self/mountinfo")?
        .take((MOUNTINFO_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    select_state_topology(&root, parse_state_mounts(&bytes)?)
}

#[cfg(target_os = "linux")]
fn check_state_root_mount(
    root: &std::path::Path,
    topology: std::io::Result<StateRootTopology>,
    result: std::io::Result<()>,
) -> anyhow::Result<()> {
    use anyhow::Context;
    let context = match &topology {
        Ok(topology) => {
            let mount = &topology.covering;
            let mut context = format!(
                "mount staged state OverlayFS at {root:?}; state root mountpoint={:?} fstype={:?} source={:?}",
                mount.point, mount.fstype, mount.source,
            );
            if !topology.descendants.is_empty() {
                use std::fmt::Write;
                let _ = write!(
                    context,
                    "; unsupported inherited nested mounts ({} strict descendants; cannot safely stage submounts)",
                    topology.descendants.len()
                );
                for mount in topology.descendants.iter().take(8) {
                    let _ = write!(
                        context,
                        "; mountpoint={:?} fstype={:?} source={:?}",
                        mount.point, mount.fstype, mount.source
                    );
                }
            }
            context
        }
        Err(error) => {
            format!("mount staged state OverlayFS at {root:?}; mount topology unavailable: {error}")
        }
    };
    // Do not replace the kernel error with a topology guess. A successful mount
    // is also unsafe with submounts: OverlayFS would silently hide their data.
    result.with_context(|| context.clone())?;
    let topology = topology.with_context(|| context.clone())?;
    if !topology.descendants.is_empty() {
        anyhow::bail!("{context}");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn mount_staged_roots(plan: &SandboxPlan) -> anyhow::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    if plan.staged_roots.is_empty() {
        return Ok(());
    }
    let stage = plan.root.join(".pvisor-state-stage");
    std::fs::create_dir(&stage)?;
    std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700))?;
    let stage_path = path_cstring(&stage)?;
    if unsafe {
        libc::mount(
            c"tmpfs".as_ptr(),
            stage_path.as_ptr(),
            c"tmpfs".as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            c"mode=0700".as_ptr().cast(),
        )
    } != 0
    {
        return Err(
            with_io_context("mount private state stage", std::io::Error::last_os_error()).into(),
        );
    }

    for (index, source) in plan.staged_roots.iter().enumerate() {
        let target = plan
            .root
            .join(source.strip_prefix("/").map_err(std::io::Error::other)?);
        std::fs::create_dir_all(&target)?;
        let backing = stage.join(index.to_string());
        let upper = backing.join("upper");
        let work = backing.join("work");
        std::fs::create_dir_all(&upper)?;
        std::fs::create_dir_all(&work)?;
        let mut options = b"userxattr,lowerdir=".to_vec();
        for path in [source, &upper, &work] {
            if path
                .as_os_str()
                .as_bytes()
                .iter()
                .any(|byte| matches!(byte, b',' | b':' | b'\\'))
            {
                return Err(std::io::Error::other(format!(
                    "overlay stage path contains an unsupported separator: {}",
                    path.display()
                ))
                .into());
            }
        }
        options.extend_from_slice(source.as_os_str().as_bytes());
        options.extend_from_slice(b",upperdir=");
        options.extend_from_slice(upper.as_os_str().as_bytes());
        options.extend_from_slice(b",workdir=");
        options.extend_from_slice(work.as_os_str().as_bytes());
        let options = std::ffi::CString::new(options).map_err(std::io::Error::other)?;
        let target = path_cstring(&target)?;
        let topology = state_root_topology(source);
        let mounted = unsafe {
            libc::mount(
                c"overlay".as_ptr(),
                target.as_ptr(),
                c"overlay".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV,
                options.as_ptr().cast(),
            )
        };
        // Capture errno before diagnostics perform any further I/O.
        let result = if mounted == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        };
        check_state_root_mount(source, topology, result)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn bind_staged_workspace(
    root: &std::path::Path,
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    let target = root.join(
        destination
            .strip_prefix("/")
            .map_err(std::io::Error::other)?,
    );
    std::fs::create_dir_all(&target)?;
    let source = path_cstring(source)?;
    let target = path_cstring(&target)?;
    if unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(with_io_context(
            "bind staged workspace at original path",
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn private_runtime_dconf_path() -> Option<std::path::PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let runtime = std::path::PathBuf::from(runtime);
    if !runtime.is_absolute() || runtime.starts_with("/tmp") {
        return None;
    }
    Some(runtime.join("dconf"))
}

#[cfg(target_os = "linux")]
fn bind_path_into_root(root: &std::path::Path, source: &std::path::Path) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};

    let relative = source.strip_prefix("/").map_err(|_| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("sandbox path must be absolute: {}", source.display()),
        )
    })?;
    let target = root.join(relative);
    if std::fs::symlink_metadata(&target).is_ok() {
        // A parent hierarchy (for example /usr or /proc) already projects the
        // same absolute source path into the synthetic root.
        return Ok(());
    }
    let metadata = std::fs::metadata(source)?;
    if metadata.is_dir() {
        std::fs::create_dir_all(&target)?;
    } else {
        let parent = target.parent().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                format!("sandbox target has no parent: {}", target.display()),
            )
        })?;
        std::fs::create_dir_all(parent)?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
    }

    let source = path_cstring(source)?;
    let target = path_cstring(&target)?;
    let flags = libc::MS_BIND | if metadata.is_dir() { libc::MS_REC } else { 0 };
    if unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            flags,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn path_cstring(path: &std::path::Path) -> std::io::Result<std::ffi::CString> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::ffi::OsStrExt;

    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("sandbox path contains a NUL byte: {}", path.display()),
        )
    })
}

#[cfg(target_os = "linux")]
fn drop_process_capabilities() -> std::io::Result<()> {
    use std::io::Error;

    const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;
    #[repr(C)]
    struct CapabilityHeader {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CapabilityData {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }

    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(Error::last_os_error());
    }

    let mut header = CapabilityHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let mut data = [CapabilityData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];
    if unsafe { libc::syscall(libc::SYS_capset, &mut header, data.as_mut_ptr()) } != 0 {
        return Err(Error::last_os_error());
    }
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
        return Err(Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
extern "C" fn forward_namespace_signal(signal: libc::c_int) {
    // PID 1 is excluded from kill(-1, ...), so this forwards cancellation to
    // every Agent descendant even after setsid(2) or a double fork.
    unsafe {
        libc::kill(-1, signal);
    }
}

#[cfg(target_os = "linux")]
static NAMESPACE_INIT_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[cfg(target_os = "linux")]
extern "C" fn forward_launcher_signal(signal: libc::c_int) {
    let pid = NAMESPACE_INIT_PID.load(std::sync::atomic::Ordering::Relaxed);
    if pid > 0 {
        unsafe {
            libc::kill(pid, signal);
        }
    }
}

#[cfg(target_os = "linux")]
fn install_namespace_signal_handlers(handler: libc::sighandler_t) -> std::io::Result<()> {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT] {
        let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
        action.sa_sigaction = handler;
        action.sa_flags = libc::SA_RESTART;
        unsafe {
            libc::sigemptyset(&mut action.sa_mask);
        }
        if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn exit_with_wait_status(status: libc::c_int) -> ! {
    if libc::WIFEXITED(status) {
        unsafe { libc::_exit(libc::WEXITSTATUS(status)) };
    }
    if libc::WIFSIGNALED(status) {
        let signal = libc::WTERMSIG(status);
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::kill(libc::getpid(), signal);
            libc::_exit(128 + signal);
        }
    }
    unsafe { libc::_exit(SANDBOX_SETUP_EXIT_CODE) };
}

/// Run a tiny trusted PID-namespace supervisor. The first child after
/// CLONE_NEWPID becomes namespace PID 1; when it exits, the kernel kills all
/// remaining processes in that namespace, including daemonized descendants.
#[cfg(target_os = "linux")]
fn supervise_pid_namespace(
    program: std::ffi::OsString,
    arguments: Vec<std::ffi::OsString>,
    arg0: std::ffi::OsString,
    mut attestation: std::fs::File,
    plan: &SandboxPlan,
) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::os::unix::process::CommandExt;

    let mut ready_pipe = [0; 2];
    let mut release_pipe = [0; 2];
    if unsafe { libc::pipe2(ready_pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error()).context("create PID supervisor ready pipe");
    }
    if unsafe { libc::pipe2(release_pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(ready_pipe[0]);
            libc::close(ready_pipe[1]);
        }
        return Err(error).context("create PID supervisor release pipe");
    }

    let namespace_init = unsafe { libc::fork() };
    if namespace_init < 0 {
        unsafe {
            libc::close(ready_pipe[0]);
            libc::close(ready_pipe[1]);
            libc::close(release_pipe[0]);
            libc::close(release_pipe[1]);
        }
        return Err(std::io::Error::last_os_error()).context("fork PID namespace init");
    }
    if namespace_init > 0 {
        drop_process_capabilities().context("drop PID supervisor namespace capabilities")?;
        unsafe {
            libc::close(ready_pipe[1]);
            libc::close(release_pipe[0]);
        }
        NAMESPACE_INIT_PID.store(namespace_init, std::sync::atomic::Ordering::Relaxed);
        if let Err(error) = install_namespace_signal_handlers(
            forward_launcher_signal as *const () as libc::sighandler_t,
        ) {
            unsafe {
                libc::kill(namespace_init, libc::SIGKILL);
                libc::waitpid(namespace_init, std::ptr::null_mut(), 0);
            }
            return Err(error).context("install PID namespace launcher signal handlers");
        }
        let mut ready = 0_u8;
        let ready_count = unsafe { libc::read(ready_pipe[0], (&mut ready as *mut u8).cast(), 1) };
        unsafe {
            libc::close(ready_pipe[0]);
        }
        if ready_count != 1 || ready != 1 {
            unsafe {
                libc::kill(namespace_init, libc::SIGKILL);
                libc::waitpid(namespace_init, std::ptr::null_mut(), 0);
                libc::close(release_pipe[1]);
            }
            return Err(std::io::Error::other(
                "PID namespace Agent setup did not attest",
            ))
            .context("initialize PID namespace supervisor");
        }
        if let Err(error) = write_rootless_attestation(&mut attestation) {
            unsafe {
                libc::kill(namespace_init, libc::SIGKILL);
                libc::waitpid(namespace_init, std::ptr::null_mut(), 0);
                libc::close(release_pipe[1]);
            }
            return Err(error).context("record installed rootless sandbox controls");
        }
        let release = 1_u8;
        let released = unsafe { libc::write(release_pipe[1], (&release as *const u8).cast(), 1) };
        unsafe {
            libc::close(release_pipe[1]);
        }
        if released != 1 {
            let error = std::io::Error::last_os_error();
            let _ = attestation.set_len(0);
            let _ = attestation.sync_data();
            unsafe {
                libc::kill(namespace_init, libc::SIGKILL);
                libc::waitpid(namespace_init, std::ptr::null_mut(), 0);
            }
            return Err(error).context("release attested Agent executable");
        }
        drop(attestation);
        let mut status = 0;
        loop {
            let waited = unsafe { libc::waitpid(namespace_init, &mut status, 0) };
            if waited == namespace_init {
                exit_with_wait_status(status);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error).context("wait for PID namespace init");
            }
        }
    }

    unsafe {
        libc::close(ready_pipe[0]);
        libc::close(release_pipe[1]);
    }
    drop(attestation);

    // This child is PID 1 in the new namespace and still holds the temporary
    // user-namespace mount capability. A procfs mounted here exposes only
    // this private PID namespace to Chromium and other child processes.
    mount_pid_namespace_procfs().context("mount private PID namespace procfs")?;
    let landlock_abi = if plan.filesystem_isolated {
        install_landlock(plan).context("install Landlock filesystem policy")?
    } else {
        0
    };
    // This is still trusted setup code, before the Agent child is forked.
    unsafe {
        std::env::remove_var(SANDBOX_PLAN_ENV);
        std::env::set_var(
            "PVISOR_SANDBOX_FILESYSTEM",
            if plan.filesystem_isolated {
                "landlock"
            } else {
                "chroot"
            },
        );
        if plan.filesystem_isolated {
            std::env::set_var("PVISOR_SANDBOX_LANDLOCK_ABI", landlock_abi.to_string());
        }
        std::env::set_var("PVISOR_SANDBOX_USER_NAMESPACE", "1");
        std::env::set_var(
            "PVISOR_SANDBOX_NETWORK",
            if matches!(plan.network, NetworkIsolation::ProxyOnly(Some(_))) {
                "proxy-only"
            } else if plan.network.is_loopback_only() {
                "deny"
            } else {
                "ambient"
            },
        );
    }
    drop_process_capabilities().context("drop PID namespace capabilities")?;

    // If the outer launcher is terminated before it can forward a signal,
    // killing PID 1 still gives the kernel an authoritative cleanup point.
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0 {
        unsafe { libc::_exit(SANDBOX_SETUP_EXIT_CODE) };
    }
    if install_namespace_signal_handlers(
        forward_namespace_signal as *const () as libc::sighandler_t,
    )
    .is_err()
    {
        unsafe { libc::_exit(SANDBOX_SETUP_EXIT_CODE) };
    }

    let agent = unsafe { libc::fork() };
    if agent < 0 {
        unsafe { libc::_exit(SANDBOX_SETUP_EXIT_CODE) };
    }
    if agent == 0 {
        let supervisor = unsafe { libc::getppid() };
        if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0
            || unsafe { libc::getppid() } != supervisor
            || install_namespace_signal_handlers(libc::SIG_DFL).is_err()
        {
            unsafe { libc::_exit(SANDBOX_SETUP_EXIT_CODE) };
        }
        let ready = 1_u8;
        let ready_count = unsafe { libc::write(ready_pipe[1], (&ready as *const u8).cast(), 1) };
        unsafe {
            libc::close(ready_pipe[1]);
        }
        if ready_count != 1 {
            unsafe { libc::_exit(SANDBOX_SETUP_EXIT_CODE) };
        }
        let mut release = 0_u8;
        let release_count =
            unsafe { libc::read(release_pipe[0], (&mut release as *mut u8).cast(), 1) };
        unsafe {
            libc::close(release_pipe[0]);
        }
        if release_count != 1 || release != 1 {
            unsafe { libc::_exit(SANDBOX_SETUP_EXIT_CODE) };
        }
        let mut command = std::process::Command::new(program);
        command.args(arguments).arg0(arg0);
        let error = command.exec();
        eprintln!("pvisor: execute sandboxed Agent: {error}");
        unsafe { libc::_exit(SANDBOX_SETUP_EXIT_CODE) };
    }

    unsafe {
        libc::close(ready_pipe[1]);
        libc::close(release_pipe[0]);
    }

    // Reap all descendants while the Agent is alive. Orphans are reparented
    // to namespace PID 1, so they cannot accumulate as unreaped zombies.
    loop {
        let mut status = 0;
        let waited = unsafe { libc::waitpid(-1, &mut status, 0) };
        if waited == agent {
            exit_with_wait_status(status);
        }
        if waited < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                unsafe { libc::_exit(SANDBOX_SETUP_EXIT_CODE) };
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn mount_pid_namespace_procfs() -> std::io::Result<()> {
    if unsafe { libc::umount2(c"/proc".as_ptr(), libc::MNT_DETACH) } != 0 {
        return Err(with_io_context(
            "unmount temporary host procfs",
            std::io::Error::last_os_error(),
        ));
    }
    if unsafe {
        libc::mount(
            c"proc".as_ptr(),
            c"/proc".as_ptr(),
            c"proc".as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(with_io_context(
            "mount procfs for private PID namespace",
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn close_unexpected_file_descriptors(retain: Option<libc::c_int>) -> std::io::Result<()> {
    let mut descriptors = Vec::new();
    let directory = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(fd) = name.parse::<libc::c_int>() else {
            continue;
        };
        if fd > libc::STDERR_FILENO && Some(fd) != retain {
            descriptors.push(fd);
        }
    }
    descriptors.sort_unstable();
    descriptors.dedup();
    for fd in descriptors {
        unsafe {
            libc::close(fd);
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn seatbelt_proxy_allows_only_its_tcp_endpoint() {
        // /usr/bin/python3 is an Xcode launcher on some runners. Resolve its
        // interpreter and runtime before entering the network test's sandbox.
        let python = std::process::Command::new("/usr/bin/python3")
            .args([
                "-c",
                "import json, sys; print(json.dumps([sys.executable, sys.base_prefix]))",
            ])
            .output()
            .unwrap();
        assert!(
            python.status.success(),
            "{}",
            String::from_utf8_lossy(&python.stderr)
        );
        let [interpreter, runtime]: [PathBuf; 2] = serde_json::from_slice(&python.stdout).unwrap();
        // Repeat with fresh ephemeral ports to catch intermittent deny-policy failures.
        for _ in 0..16 {
            let temp = tempfile::tempdir().unwrap();
            let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = proxy.local_addr().unwrap();
            let socket_path = temp.path().join("agentctl.sock");
            let _unix = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
            let local_root = temp.path().join("ipc");
            std::fs::create_dir(&local_root).unwrap();
            let local_socket = local_root.join("local.sock");
            let denied_socket = temp.path().join("denied.sock");
            let _local = std::os::unix::net::UnixListener::bind(&local_socket).unwrap();
            let _denied = std::os::unix::net::UnixListener::bind(&denied_socket).unwrap();
            let (profile, params) = seatbelt_profile_with_reads(
                &[temp.path().to_owned()],
                Some(&[
                    PathBuf::from("/System"),
                    PathBuf::from("/usr"),
                    interpreter.clone(),
                    runtime.clone(),
                    PathBuf::from("/private/etc"),
                    PathBuf::from("/dev"),
                ]),
                std::slice::from_ref(&socket_path),
                &[local_root],
                NetworkIsolation::ProxyOnly(Some(endpoint)),
                true,
            )
            .unwrap();
            let mut command = std::process::Command::new(MACOS_SANDBOX_EXEC);
            command.current_dir(temp.path()).arg("-p").arg(&profile);
            for (key, value) in params {
                command.arg("-D").arg(format!("{key}={}", value.display()));
            }
            let output = command
                // Prove the sandboxed invocation does not require xcrun selection.
                .env("DEVELOPER_DIR", temp.path().join("no-developer-tools"))
                .arg(&interpreter)
                .args([
                    "-c",
                    r#"
import errno, socket, sys
allowed, denied = map(int, sys.argv[1:3])
for path in sys.argv[3:5]:
    with socket.socket(socket.AF_UNIX) as s: s.connect(path)
with socket.socket(socket.AF_UNIX) as s:
    assert s.connect_ex(sys.argv[5]) in (errno.EACCES, errno.EPERM)
for kind, port in [(socket.SOCK_STREAM, denied), (socket.SOCK_DGRAM, allowed)]:
    with socket.socket(socket.AF_INET, kind) as s:
        assert s.connect_ex(('127.0.0.1', port)) in (errno.EACCES, errno.EPERM)
with socket.socket() as s:
    s.bind(('127.0.0.1', 0))
    try: s.listen(1)
    except PermissionError: pass
    else: raise AssertionError('listener escaped')
with socket.create_connection(('127.0.0.1', allowed), timeout=1): pass
# Exercise the local bind explicitly: connect() can also perform it implicitly.
for source in ['0.0.0.0', '127.0.0.1']:
    with socket.create_connection(('127.0.0.1', allowed), timeout=1,
                                  source_address=(source, 0)): pass

"#,
                ])
                .arg(endpoint.port().to_string())
                .arg(other.local_addr().unwrap().port().to_string())
                .arg(&socket_path)
                .arg(&local_socket)
                .arg(&denied_socket)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "proxy={endpoint}, interpreter={}, profile={profile}\n{}",
                interpreter.display(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn required_seatbelt_can_start_the_trusted_launcher() {
        let temp = tempfile::tempdir().unwrap();
        let launcher = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("pvisor");
        let readable = vec![
            launcher.clone(),
            PathBuf::from("/System/Library"),
            PathBuf::from("/System/Cryptexes/OS"),
            PathBuf::from("/usr"),
            PathBuf::from("/bin"),
            PathBuf::from("/dev"),
        ];
        let (profile, params) = seatbelt_profile_with_reads(
            &[temp.path().to_owned()],
            Some(&readable),
            &[],
            &[],
            NetworkIsolation::ProxyOnly(None),
            true,
        )
        .unwrap();
        let mut command = std::process::Command::new(MACOS_SANDBOX_EXEC);
        command.arg("-p").arg(&profile);
        for (key, value) in params {
            command.arg("-D").arg(format!("{key}={}", value.display()));
        }
        let output = command.arg(&launcher).arg("--version").output().unwrap();
        assert!(
            output.status.success(),
            "{:?}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn seatbelt_profile_uses_parameters_and_rejects_a_writable_host_root() {
        let temporary = tempfile::Builder::new()
            .prefix("pvisor-\")-(deny-default-")
            .tempdir()
            .unwrap();
        let canonical = temporary.path().canonicalize().unwrap();
        let (profile, parameters) = seatbelt_profile(
            &[temporary.path().to_owned()],
            &[],
            &[],
            NetworkIsolation::LoopbackOnly,
            true,
        )
        .unwrap();

        assert!(!profile.contains(canonical.to_str().unwrap()));
        assert_eq!(parameters, [("PVISOR_WRITABLE_0".into(), canonical)]);
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(remote ip \"localhost:*\")"));
        assert!(profile.contains("(allow network-outbound (remote ip \"localhost:*\"))"));

        let error = seatbelt_profile(
            &[PathBuf::from("/")],
            &[],
            &[],
            NetworkIsolation::Ambient,
            true,
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
