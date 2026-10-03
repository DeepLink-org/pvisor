//! libkrun VM process isolation over a pVisor-provided root OverlayFS.

use crate::config::VmSettings;
use crate::executor::{ExecutorOutput, RunExecutor, Session, SessionEnd as End};
use crate::executor::{join_capture, read_limited, stdio};
use anyhow::Context as _;
use async_trait::async_trait;
use pvisor_core::{
    CapabilityDimension, CapabilityEnforcementEvidence, CapabilityEnforcementPlan, ExecutorKind,
    ExecutorObservations, ExecutorPlan, IsolationKind, ProcessOutput, ResourceLimits, RunFailure,
    RunFailureKind, RunInvocation, RunState,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use tokio::process::Command;

const RUNNER_SPEC_ENV: &str = "PVISOR_KRUN_RUNNER_SPEC";
const WORKSPACE_TAG: &str = "pvisor-workspace";
const NETWORK_FD_ENV: &str = "PVISOR_KRUN_NETWORK_FD";
const NETWORK_CHILD_FD: RawFd = 198;
const CONTROL_FD_ENV: &str = "PVISOR_KRUN_CONTROL_FD";
const CONTROL_CHILD_FD: RawFd = 199;
const RAM_FD_ENV: &str = "PVISOR_KRUN_RAM_FD";
const RAM_CHILD_FD: RawFd = 200;

fn runner_fd(fd: RawFd) -> std::io::Result<OwnedFd> {
    // Keep both source descriptors above the fixed destinations, avoiding dup2
    // clobbering one socket when the parent's fd table happens to be crowded.
    let duplicate = unsafe {
        libc::fcntl(
            fd,
            libc::F_DUPFD_CLOEXEC,
            crate::diagnostics::INHERITED_LOG_FD + 1,
        )
    };
    if duplicate < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn needs_krun_enomem_workaround() -> bool {
    match std::env::var("PVISOR_KRUN_ENOMEM_WORKAROUND").as_deref() {
        Ok("1") => return true,
        Ok("0") => return false,
        _ => {}
    }
    std::fs::read_to_string("/proc/sys/kernel/osrelease").map_or(true, |release| {
        kernel_release_needs_krun_workaround(&release)
    })
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn kernel_release_needs_krun_workaround(release: &str) -> bool {
    release
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .is_none_or(|major| major < 7)
}

#[cfg(all(target_os = "linux", target_env = "musl", not(target_arch = "x86_64")))]
compile_error!("static musl VM support currently targets x86_64 only");

#[cfg(all(target_os = "linux", target_env = "musl", target_arch = "x86_64"))]
mod embedded_kernel {
    include!(concat!(env!("OUT_DIR"), "/embedded_kernel.rs"));
}

#[derive(Debug, Clone)]
pub struct VmExecutor {
    settings: VmSettings,
}

#[derive(Debug, Serialize, Deserialize)]
struct RunnerSpec {
    #[serde(default)]
    run_id: String,
    setup_attestation: PathBuf,
    root: OverlayDeviceSpec,
    workspace: Option<OverlayDeviceSpec>,
    workspace_target: Option<PathBuf>,
    guest: pvisor_guest::GuestConfig,
    cpus: u8,
    memory_mib: u32,
    library_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OverlayDeviceSpec {
    lowers: Vec<PathBuf>,
    upper: PathBuf,
    work: Option<PathBuf>,
    #[serde(default)]
    preimages: Option<PathBuf>,
    #[serde(default)]
    excluded: Vec<PathBuf>,
    #[serde(default)]
    access_policy: pvisor_core::overlay::FileAccessPolicy,
}

/// A host-root VM must not reach the same workspace through its original lower
/// path, or reach writable backing state through the root device.
fn protect_overlay_backing(
    root: &mut OverlayDeviceSpec,
    workspace: Option<&OverlayDeviceSpec>,
) -> anyhow::Result<()> {
    root.access_policy = root.access_policy.for_view("rootfs");
    let mut hidden = vec![root.upper.clone()];
    hidden.extend(root.work.iter().cloned());
    hidden.extend(root.preimages.iter().cloned());
    if let Some(workspace) = workspace {
        hidden.push(workspace.upper.clone());
        hidden.extend(workspace.work.iter().cloned());
        hidden.extend(workspace.preimages.iter().cloned());
        for lower in &root.lowers {
            let lower = lower.canonicalize()?;
            for source in &workspace.lowers {
                let source = source.canonicalize()?;
                if let Ok(relative) = source.strip_prefix(&lower) {
                    if relative.as_os_str().is_empty() {
                        continue;
                    }
                    let prefix = relative
                        .to_str()
                        .ok_or_else(|| anyhow::anyhow!("file rule prefix must be UTF-8"))?;
                    root.access_policy
                        .extend(&workspace.access_policy.prefixed(prefix)?)?;
                }
            }
        }
    }
    for lower in &root.lowers {
        let lower = lower.canonicalize()?;
        for path in &hidden {
            let path = path.canonicalize()?;
            if let Ok(relative) = path.strip_prefix(&lower) {
                anyhow::ensure!(
                    !relative.as_os_str().is_empty(),
                    "overlay backing must not equal its lower"
                );
                root.excluded.push(relative.to_owned());
            }
        }
    }
    Ok(())
}

fn hide_ram_backing(device: &mut OverlayDeviceSpec, path: &Path) -> anyhow::Result<()> {
    let path = path.canonicalize()?;
    for lower in &device.lowers {
        let lower = lower.canonicalize()?;
        if let Ok(relative) = path.strip_prefix(lower) {
            anyhow::ensure!(
                !relative.as_os_str().is_empty(),
                "RAM backing directory cannot be the guest lower root"
            );
            device.excluded.push(relative.to_owned());
        }
    }
    Ok(())
}

impl VmExecutor {
    pub fn new(mut settings: VmSettings) -> anyhow::Result<Self> {
        anyhow::ensure!(
            settings.memory_pool.is_none()
                || cfg!(all(target_os = "macos", target_arch = "aarch64")),
            "vm.memory_pool requires macOS on Apple Silicon"
        );
        if let Some(path) = settings.memory_pool.as_ref() {
            settings.memory_pool = Some(
                path.canonicalize()
                    .context("resolve vm.memory_pool socket")?,
            );
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        anyhow::ensure!(
            !settings.ram_compression
                || (settings.memory_pool.is_none()
                    && std::env::var_os(super::pager::POOL_ENV).is_none()),
            "experimental cold pager cannot use a FUSE RAM backing"
        );
        anyhow::ensure!(settings.memory_mib > 0, "vm.memory_mib must be positive");
        anyhow::ensure!(settings.cpus > 0, "vm.cpus must be positive");
        anyhow::ensure!(settings.cpus <= 8, "libkrunfw supports at most 8 vCPUs");
        #[cfg(all(target_os = "linux", target_env = "musl", target_arch = "x86_64"))]
        anyhow::ensure!(
            settings.library_dir.is_none(),
            "vm.library_dir is unavailable in the static musl build; libkrun's kernel bundle is embedded"
        );
        let rootfs = settings
            .rootfs
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("vm.rootfs must be configured"))?;
        anyhow::ensure!(
            rootfs.is_dir(),
            "vm.rootfs is not a directory: {}",
            rootfs.display()
        );
        if let Some(directory) = &settings.library_dir {
            anyhow::ensure!(
                directory.is_dir(),
                "vm.library_dir is not a directory: {}",
                directory.display()
            );
            anyhow::ensure!(
                directory.join(firmware_name()).is_file(),
                "vm.library_dir does not contain {}: {}",
                firmware_name(),
                directory.display()
            );
        } else if let Some(directory) = bundled_firmware_dir() {
            settings.library_dir = Some(directory);
        }
        Ok(Self { settings })
    }

    pub fn settings(&self) -> &VmSettings {
        &self.settings
    }
}

pub(crate) fn bundled_firmware_dir() -> Option<PathBuf> {
    let directory = std::env::current_exe().ok()?.parent()?.to_path_buf();
    directory
        .join(firmware_name())
        .is_file()
        .then_some(directory)
}

pub(crate) const fn firmware_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "libkrunfw.5.dylib"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "libkrunfw.so.5"
    }
}

#[async_trait]
impl RunExecutor for VmExecutor {
    fn descriptor(&self) -> ExecutorPlan {
        ExecutorPlan {
            name: "libkrun-root-overlay-v1".into(),
            kind: ExecutorKind::VirtualMachine,
            isolation: IsolationKind::VirtualMachine,
            capability_plan: CapabilityEnforcementPlan::default()
                .planned(
                    CapabilityDimension::FilesystemRead,
                    "libkrun-guest-kernel-virtiofs-root",
                )
                .planned(
                    CapabilityDimension::FilesystemWrite,
                    "libkrun-guest-kernel-virtiofs-overlay",
                ),
            supports_checkpoint: true,
            supports_migration: false,
        }
    }

    fn supports(&self, invocation: &RunInvocation) -> bool {
        matches!(invocation, RunInvocation::Process(_))
    }

    fn supports_vm_network_attachment(&self) -> bool {
        true
    }

    async fn execute(&self, context: &Session) -> ExecutorOutput {
        crate::util::startup_mark_run("vm.prepare_begin", context.spec().run_id.as_str());
        let mut spec = context.spec().clone();
        context
            .transition(
                RunState::Starting,
                Some("starting libkrun guest over pVisor root OverlayFS".into()),
            )
            .await;
        if !cfg!(any(
            target_os = "linux",
            all(target_os = "macos", target_arch = "aarch64")
        )) {
            return failed_to_start(
                "libkrun execution requires Linux/KVM or Apple Silicon macOS/HVF".into(),
            );
        }
        #[cfg(target_os = "linux")]
        if let Err(error) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/kvm")
        {
            return failed_to_start(format!(
                "libkrun VM requires an accessible /dev/kvm: {error}"
            ));
        }

        let RunInvocation::Process(invocation) = &mut spec.invocation;
        let overlay_target = spec
            .metadata
            .get("pvisor.vm.overlay_target")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from);
        let root = self
            .settings
            .rootfs
            .clone()
            .expect("validated by VmExecutor::new");
        if !root.is_dir() {
            return failed_to_start(format!(
                "prepared root OverlayFS is not mounted: {}",
                root.display()
            ));
        }
        let guest_cwd = spec
            .metadata
            .get("pvisor.vm.guest_cwd")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        let workspace = spec
            .metadata
            .get("pvisor.vm.workspace_overlay")
            .cloned()
            .map(serde_json::from_value::<OverlayDeviceSpec>)
            .transpose();
        let configured_overlay = match workspace {
            Ok(workspace) => workspace,
            Err(error) => {
                return failed_to_start(format!(
                    "invalid libkrun workspace overlay metadata: {error}"
                ));
            }
        };
        let access_policy = configured_overlay
            .as_ref()
            .map(|overlay| overlay.access_policy.clone())
            .unwrap_or_default();
        let (root_overlay, mut workspace) = if overlay_target.is_none() {
            (
                configured_overlay.unwrap_or_else(|| OverlayDeviceSpec {
                    lowers: vec![root.clone()],
                    upper: PathBuf::new(),
                    work: None,
                    preimages: None,
                    excluded: Vec::new(),
                    access_policy: access_policy.clone(),
                }),
                None,
            )
        } else {
            (
                OverlayDeviceSpec {
                    lowers: vec![root.clone()],
                    upper: PathBuf::new(),
                    work: None,
                    preimages: None,
                    excluded: Vec::new(),
                    access_policy: access_policy.clone(),
                },
                configured_overlay,
            )
        };
        let workspace_target = workspace.as_ref().and(overlay_target.clone());
        let mut env = if invocation.inherit_env {
            std::env::vars().collect::<BTreeMap<_, _>>()
        } else {
            BTreeMap::new()
        };
        for key in [
            crate::image::cache::SERVER_ENV,
            "PVISOR_CACHE_TOKEN",
            crate::AGENTCTL_ENDPOINT_ENV,
            crate::AGENTCTL_TOKEN_ENV,
            crate::AGENTCTL_TRANSPORT_ENV,
            crate::AGENTCTL_VERSION_ENV,
        ] {
            env.remove(key);
        }
        for key in [
            "DYLD_LIBRARY_PATH",
            "DYLD_FALLBACK_LIBRARY_PATH",
            "LD_LIBRARY_PATH",
        ] {
            env.remove(key);
        }
        if !invocation.env.contains_key("PATH")
            && (root != Path::new("/") || !env.contains_key("PATH"))
        {
            env.insert(
                "PATH".into(),
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into(),
            );
        }
        if !invocation.env.contains_key("HOME")
            && (root != Path::new("/") || !env.contains_key("HOME"))
        {
            env.insert("HOME".into(), "/root".into());
        }
        if !invocation.env.contains_key("TMPDIR") {
            env.insert("TMPDIR".into(), "/tmp".into());
        }
        env.extend(invocation.env.clone());

        let temporary = match tempfile::Builder::new().prefix("pvisor-krun-").tempdir() {
            Ok(value) => value,
            Err(error) => {
                return failed_to_start(error.to_string());
            }
        };
        let executable = match std::env::current_exe() {
            Ok(path) if path.is_absolute() => path,
            Ok(path) => {
                return failed_to_start(format!(
                    "pVisor executable is not absolute: {}",
                    path.display()
                ));
            }
            Err(error) => {
                return failed_to_start(error.to_string());
            }
        };
        let runner_root = root.clone();
        let root_upper = temporary.path().join("root-upper");
        let root_work = temporary.path().join("root-work");
        if let Err(error) =
            std::fs::create_dir_all(&root_upper).and_then(|()| std::fs::create_dir_all(&root_work))
        {
            return failed_to_start(format!("prepare libkrun root overlay: {error}"));
        }
        let mut root_overlay = if root_overlay.upper.as_os_str().is_empty() {
            OverlayDeviceSpec {
                lowers: root_overlay.lowers,
                upper: root_upper.clone(),
                work: Some(root_work.clone()),
                preimages: root_overlay.preimages,
                excluded: root_overlay.excluded,
                access_policy: root_overlay.access_policy,
            }
        } else {
            root_overlay
        };
        if let Err(error) = protect_overlay_backing(&mut root_overlay, workspace.as_ref()) {
            return failed_to_start(error.to_string());
        }
        let vm_network_enabled = context
            .spec()
            .metadata
            .get("pvisor.network.driver")
            .and_then(serde_json::Value::as_str)
            == Some("vm-smoltcp");
        if vm_network_enabled {
            let network_lower = temporary.path().join("network-lower");
            let resolver = network_lower.join("etc/resolv.conf");
            if let Err(error) = std::fs::create_dir_all(resolver.parent().expect("resolver parent"))
                .and_then(|()| {
                    std::fs::write(
                        &resolver,
                        b"nameserver 192.0.2.1\noptions timeout:2 attempts:2\n",
                    )
                })
            {
                return failed_to_start(format!("prepare VM synthetic resolver: {error}"));
            }
            root_overlay.lowers.insert(0, network_lower);
        }
        if let Some(workspace) = &workspace {
            if workspace.lowers.is_empty()
                || workspace.lowers.iter().any(|lower| !lower.is_dir())
                || !workspace.upper.is_dir()
                || workspace.work.as_ref().is_some_and(|work| !work.is_dir())
            {
                return failed_to_start(
                    "libkrun workspace overlay contains a missing backing directory".into(),
                );
            }
            let target = workspace_target
                .as_deref()
                .expect("workspace is only configured with an overlay target");
            let mountpoint = match guest_path_in_root(&runner_root, target) {
                Ok(path) => path,
                Err(error) => {
                    return failed_to_start(error.to_string());
                }
            };
            match std::fs::symlink_metadata(&mountpoint) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return failed_to_start(format!(
                        "guest overlay target must be a directory: {}",
                        target.display()
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let upper_mountpoint =
                        guest_path_in_root(&root_upper, target).expect("validated guest target");
                    if let Err(error) = std::fs::create_dir_all(&upper_mountpoint) {
                        return failed_to_start(format!("create guest overlay target: {error}"));
                    }
                }
                Err(error) => {
                    return failed_to_start(error.to_string());
                }
            }
        }
        let guest = guest_config(
            &invocation.program,
            &invocation.args,
            env,
            guest_cwd,
            workspace_target.clone(),
            &spec.runtime.resource_limits,
            vm_network_enabled,
        );
        if let Err(error) = guest.command() {
            return failed_to_start(error.to_string());
        }
        let requested_memory_mib = spec
            .runtime
            .resource_limits
            .memory_bytes
            .map(|bytes| bytes.div_ceil(1024 * 1024).max(1))
            .and_then(|mib| u32::try_from(mib).ok());
        let attestation = match tempfile::NamedTempFile::new_in(temporary.path()) {
            Ok(file) => file,
            Err(error) => {
                return failed_to_start(error.to_string());
            }
        };
        crate::util::startup_mark_run("vm.ram_backing_begin", spec.run_id.as_str());
        let mut ram_backing =
            match super::control::RamBacking::create(self.settings.ram_backing.as_deref()) {
                Ok(backing) => backing,
                Err(error) => return failed_to_start(format!("create VM RAM backing: {error}")),
            };
        if self.settings.ram_compression
            && let Err(error) = ram_backing.enable_compression()
        {
            return failed_to_start(format!(
                "create compressed RAM backing (FUSE/macFUSE required): {error}"
            ));
        }
        let mut hidden = vec![ram_backing.path.clone()];
        if let Some(layers) = ram_backing.layer_directory() {
            hidden.push(layers.to_path_buf());
        }
        if let Some(cache) = dirs::cache_dir()
            .map(|path| path.join("pvisor/ram"))
            .filter(|path| path.exists())
        {
            hidden.push(cache);
        }
        for path in hidden {
            if let Err(error) = hide_ram_backing(&mut root_overlay, &path).and_then(|()| {
                workspace
                    .as_mut()
                    .map_or(Ok(()), |device| hide_ram_backing(device, &path))
            }) {
                return failed_to_start(format!("hide VM RAM backing: {error}"));
            }
        }
        crate::util::startup_mark_run("vm.ram_backing_ready", spec.run_id.as_str());
        let runner = RunnerSpec {
            run_id: spec.run_id.to_string(),
            setup_attestation: attestation.path().to_path_buf(),
            root: root_overlay,
            workspace,
            workspace_target: workspace_target.clone(),
            guest,
            cpus: self.settings.cpus as u8,
            memory_mib: requested_memory_mib
                .map(|requested| requested.min(self.settings.memory_mib))
                .unwrap_or(self.settings.memory_mib),
            library_dir: self.settings.library_dir.clone(),
        };
        crate::util::startup_mark_run("vm.spec_write_begin", spec.run_id.as_str());
        // This private launch message is consumed only by the child spawned below.
        // It is not recovery metadata: complete the write, without disk sync.
        let runner_file = (|| -> anyhow::Result<_> {
            use std::io::Write;
            let contents = serde_json::to_vec(&runner)?;
            let mut file = tempfile::NamedTempFile::new_in(temporary.path())?;
            file.write_all(&contents)?;
            Ok(file)
        })();
        let runner_file = match runner_file {
            Ok(file) => file,
            Err(error) => return failed_to_start(error.to_string()),
        };
        let runner_path = runner_file.path();
        crate::util::startup_mark_run("vm.spec_write_ready", spec.run_id.as_str());
        let mut vm_network = match context.take_vm_network() {
            Ok(network) => network,
            Err(error) => {
                return failed_to_start(error.to_string());
            }
        };
        if vm_network_enabled && vm_network.is_none() {
            return failed_to_start("pVisor VM network attachment is missing".into());
        }
        let ram_runner = match runner_fd(ram_backing.file.as_raw_fd()) {
            Ok(fd) => fd,
            Err(error) => {
                return failed_to_start(format!("duplicate RAM backing descriptor: {error}"));
            }
        };
        let control_pair = (|| {
            let (host, runner) = std::os::unix::net::UnixStream::pair()?;
            host.set_nonblocking(true)?;
            let runner = runner_fd(runner.as_raw_fd())?;
            Ok::<_, std::io::Error>((tokio::net::UnixStream::from_std(host)?, runner))
        })();
        let (control_host, control_runner) = match control_pair {
            Ok(pair) => pair,
            Err(error) => return failed_to_start(format!("create VM control socket: {error}")),
        };
        let network_runner = match vm_network
            .as_ref()
            .map(|network| runner_fd(network.guest_stream().as_raw_fd()))
            .transpose()
        {
            Ok(fd) => fd,
            Err(error) => return failed_to_start(format!("duplicate VM network socket: {error}")),
        };
        let mut command = Command::new(executable);
        command
            .env(RUNNER_SPEC_ENV, runner_path)
            .env(CONTROL_FD_ENV, CONTROL_CHILD_FD.to_string())
            .env(RAM_FD_ENV, RAM_CHILD_FD.to_string())
            .stdin(stdio(invocation.stdin))
            .stdout(stdio(invocation.stdout))
            .stderr(stdio(invocation.stderr))
            .kill_on_drop(true)
            .process_group(0);
        if let Some(path) = &self.settings.memory_pool {
            command.env("PVISOR_EXPERIMENTAL_MEMORY_POOL", path);
        }
        let diagnostic_runner =
            crate::diagnostics::runner_output().and_then(|file| runner_fd(file.as_raw_fd()).ok());
        let diagnostic_source_fd = diagnostic_runner.as_ref().map(AsRawFd::as_raw_fd);
        if diagnostic_source_fd.is_some() {
            command.env(crate::diagnostics::INHERITED_LOG_ENV, "201");
        } else {
            command.env_remove(crate::diagnostics::INHERITED_LOG_ENV);
        }
        let control_source_fd = control_runner.as_raw_fd();
        let ram_source_fd = ram_runner.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                if let Some(fd) = diagnostic_source_fd
                    && libc::dup2(fd, crate::diagnostics::INHERITED_LOG_FD) < 0
                {
                    // Logging cannot prevent execution. Ensure the child cannot
                    // mistake an unrelated descriptor for diagnostic output.
                    libc::close(crate::diagnostics::INHERITED_LOG_FD);
                }
                if libc::dup2(control_source_fd, CONTROL_CHILD_FD) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::dup2(ram_source_fd, RAM_CHILD_FD) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        if let Some(network) = &network_runner {
            let source_fd = network.as_raw_fd();
            command.env(NETWORK_FD_ENV, NETWORK_CHILD_FD.to_string());
            // The socketpair has CLOEXEC. Duplicate it to one fixed inherited
            // descriptor after fork and before exec; the JSON runner spec never
            // contains a process-local FD number.
            unsafe {
                command.pre_exec(move || {
                    if libc::dup2(source_fd, NETWORK_CHILD_FD) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        // The vendored workaround sleeps before every KVM_RUN. It is still
        // needed on the Fedora 43 / Linux 6.17 host used by pVisor's Linux
        // validation, but makes newer kernels much slower. Keep it for 6.x
        // and unknown hosts; allow an explicit override for diagnostics.
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        if needs_krun_enomem_workaround() {
            command.env("KRUN_ENOMEM_WORKAROUND", "1");
        } else {
            command.env_remove("KRUN_ENOMEM_WORKAROUND");
        }
        if let Some(directory) = &self.settings.library_dir {
            #[cfg(target_os = "linux")]
            command.env("LD_LIBRARY_PATH", directory);
            #[cfg(target_os = "macos")]
            command.env("DYLD_LIBRARY_PATH", directory);
        }
        crate::util::startup_mark_run("vm.spawn_begin", spec.run_id.as_str());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return failed_to_start(error.to_string());
            }
        };
        crate::util::startup_mark_run("vm.spawn_returned", spec.run_id.as_str());
        drop(diagnostic_runner);
        drop(control_runner);
        drop(ram_runner);
        drop(network_runner);
        let process_group = child.id();
        let _foreground =
            match crate::executor::process::ForegroundProcessGroup::give_to(&child, invocation) {
                Ok(foreground) => foreground,
                Err(error) => {
                    crate::session::lifecycle::terminate_process_tree(
                        &mut child,
                        process_group,
                        spec.runtime.termination_grace_ms,
                    )
                    .await;
                    return failed_to_start(format!(
                        "failed to give terminal to VM runner: {error}"
                    ));
                }
            };
        let stdout_task = child.stdout.take().map(|stdout| {
            let limit = spec.runtime.max_output_bytes;
            tokio::spawn(async move { read_limited(stdout, limit).await })
        });
        let stderr_task = child.stderr.take().map(|stderr| {
            let limit = spec.runtime.max_output_bytes;
            tokio::spawn(async move { read_limited(stderr, limit).await })
        });
        context.vm_control.attach(control_host, ram_backing).await;
        context.transition(RunState::Running, None).await;

        let end = context
            .wait_child(&mut child, spec.runtime.timeout_ms)
            .await;
        if matches!(end, End::Cancelled | End::Deadline) {
            crate::session::lifecycle::terminate_process_tree(
                &mut child,
                process_group,
                spec.runtime.termination_grace_ms,
            )
            .await;
        }
        context.vm_control.detach().await;
        let transport_stdout = join_capture(stdout_task).await;
        let transport_stderr = join_capture(stderr_task).await;
        let mut output = ProcessOutput::default();
        if let Some(captured) = transport_stdout {
            output.stdout = Some(captured.text);
            output.stdout_truncated = captured.truncated;
        }
        if let Some(captured) = transport_stderr {
            output.stderr = Some(captured.text);
            output.stderr_truncated = captured.truncated;
        }
        // Signals/cancellation can interrupt between configuring the VMM and
        // entering it. Without a normal runner exit we leave enforcement unknown.
        let runner_exited = matches!(&end, End::Exited(Ok(status)) if status.code().is_some_and(|code| code != 125));
        let (state, exit_code, failure) = match end {
            End::Cancelled => (RunState::Cancelled, None, None),
            End::Deadline => (
                RunState::Failed,
                None,
                Some(RunFailure {
                    kind: RunFailureKind::DeadlineExceeded,
                    message: "libkrun guest exceeded the transport watchdog".into(),
                    retryable: false,
                }),
            ),
            End::Exited(Ok(status)) => guest_exit_outcome(status),
            End::Exited(Err(error)) => (
                RunState::Failed,
                None,
                Some(RunFailure {
                    kind: RunFailureKind::Infrastructure,
                    message: error.to_string(),
                    retryable: true,
                }),
            ),
        };
        // The trusted runner writes only after all VMM devices and confinement
        // controls install successfully. Failed entry clears the receipt.
        let mut executor_observations = ExecutorObservations::default();
        if runner_exited
            && std::fs::read(attestation.path())
                .is_ok_and(|bytes| bytes == b"pvisor-vmm-installed-v1\n")
        {
            executor_observations.origin = pvisor_core::event::Origin::Backend;
            executor_observations.enforcement = CapabilityEnforcementEvidence::default()
                .enforced(
                    CapabilityDimension::FilesystemRead,
                    "libkrun-configured-root-device",
                )
                .enforced(
                    CapabilityDimension::FilesystemWrite,
                    "libkrun-configured-overlay-device",
                );
            if vm_network
                .as_ref()
                .is_some_and(|network| network.is_enforcing())
            {
                executor_observations.enforcement = executor_observations.enforcement.enforced(
                    CapabilityDimension::Network,
                    "vm-smoltcp-installed-net-device",
                );
            }
        }
        let mut warnings = Vec::new();
        if let Some(network) = vm_network.take()
            && let Err(error) = network.shutdown()
        {
            tracing::warn!(%error, "failed to stop VM smoltcp backend");
            warnings.push(format!("failed to stop VM smoltcp backend: {error:#}"));
        }
        ExecutorOutput {
            executor_observations,

            state,

            exit_code,
            failure,
            output,
            value: None,
            metrics: BTreeMap::from([(
                "resource.vm_memory_bytes".into(),
                f64::from(runner.memory_mib) * 1024.0 * 1024.0,
            )]),
            artifacts: Vec::new(),
            event_stream_ref: None,
            warnings,
        }
    }
}

fn guest_exit_outcome(
    status: std::process::ExitStatus,
) -> (RunState, Option<i32>, Option<RunFailure>) {
    if status.code().is_some() {
        return crate::executor::exit_outcome(status);
    }
    (
        RunState::Failed,
        None,
        Some(RunFailure {
            kind: RunFailureKind::Infrastructure,
            message: format!("libkrun runner exited with {status}"),
            retryable: false,
        }),
    )
}

/// Handle the self-exec libkrun runner.
/// Returns `true` when the current process was consumed by an internal mode.
pub fn run_internal_if_requested() -> anyhow::Result<bool> {
    if let Some(path) = std::env::var_os(RUNNER_SPEC_ENV) {
        crate::util::startup_mark("runner.spec_read_begin");
        let spec: RunnerSpec = serde_json::from_slice(&std::fs::read(&path)?)?;
        crate::util::startup_mark_run("runner.spec_read_ready", &spec.run_id);
        run_runner(spec)?;
        return Ok(true);
    }
    Ok(false)
}

fn run_runner(spec: RunnerSpec) -> anyhow::Result<()> {
    let attestation = std::fs::OpenOptions::new()
        .write(true)
        .open(&spec.setup_attestation)?;
    #[cfg(target_os = "linux")]
    {
        let mut read_only = spec.root.lowers.clone();
        let mut read_write = vec![spec.root.upper.clone()];
        read_write.extend(spec.root.work.iter().cloned());
        read_write.extend(spec.root.preimages.iter().cloned());
        if let Some(workspace) = &spec.workspace {
            read_only.extend(workspace.lowers.iter().cloned());
            read_write.push(workspace.upper.clone());
            read_write.extend(workspace.work.iter().cloned());
            read_write.extend(workspace.preimages.iter().cloned());
        }
        crate::executor::sandbox::restrict_krun_runner(
            read_only,
            read_write,
            spec.library_dir.clone(),
        )?;
    }
    run_linked_krun(spec, attestation)
}

fn run_linked_krun(spec: RunnerSpec, mut attestation: std::fs::File) -> anyhow::Result<()> {
    use std::io::Write;
    if std::env::var_os("PVISOR_KRUN_LOG").is_some() {
        check_krun(krun::krun_set_log_level(5), "krun_set_log_level")?;
    }
    let workspace_tag = CString::new(WORKSPACE_TAG)?;
    let guest_config = serde_json::to_vec(&spec.guest)?;
    crate::util::startup_mark_run("runner.context_begin", &spec.run_id);
    let ctx = check_ctx(krun::krun_create_ctx(), "krun_create_ctx")?;
    crate::util::startup_mark_run("runner.context_ready", &spec.run_id);
    let ram = std::env::var(RAM_FD_ENV)?.parse::<RawFd>()?;
    anyhow::ensure!(ram == RAM_CHILD_FD, "invalid RAM backing descriptor");
    if unsafe { libc::fcntl(ram, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    check_krun(
        krun::krun_set_ram_backing(ctx, unsafe { std::fs::File::from_raw_fd(ram) }),
        "krun_set_ram_backing",
    )?;
    check_krun(
        krun::krun_set_vm_config(ctx, spec.cpus, spec.memory_mib),
        "krun_set_vm_config",
    )?;
    #[cfg(all(target_os = "linux", target_env = "musl", target_arch = "x86_64"))]
    check_krun(
        unsafe {
            krun::krun_set_embedded_kernel(
                ctx,
                embedded_kernel::KERNEL.as_ptr(),
                embedded_kernel::KERNEL.len(),
                embedded_kernel::GUEST_ADDR,
                embedded_kernel::ENTRY_ADDR,
            )
        },
        "krun_set_embedded_kernel",
    )?;
    add_krun_overlay(ctx, "/dev/root", &spec.root, 1 << 29)?;
    check_krun(
        unsafe {
            krun::krun_fs_add_overlay_file(
                ctx,
                c"/dev/root".as_ptr(),
                c"/.pvisor-guest.json".as_ptr(),
                guest_config.as_ptr(),
                guest_config.len(),
                0o400,
                true,
            )
        },
        "krun_fs_add_overlay_file(guest config)",
    )?;
    if let Some(workspace) = &spec.workspace {
        add_krun_overlay(ctx, workspace_tag.to_str()?, workspace, 0)?;
    }
    if let Some(fd) = std::env::var_os(NETWORK_FD_ENV) {
        let fd = fd
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("invalid {NETWORK_FD_ENV}"))?
            .parse::<RawFd>()
            .with_context(|| format!("parse {NETWORK_FD_ENV}"))?;
        check_krun(
            unsafe {
                krun::krun_add_net_unixstream(
                    ctx,
                    std::ptr::null(),
                    fd,
                    pvisor_overlaynet::vm::VM_MAC.as_ptr(),
                    0,
                    0,
                )
            },
            "krun_add_net_unixstream",
        )?;
    }
    // Contexts start with an implicit vsock whose heuristic enables TSI when
    // there is no virtio-net device. Replace it with an explicit zero-feature
    // device so ordinary guest sockets cannot escape through the host stack.
    check_krun(
        krun::krun_disable_implicit_vsock(ctx),
        "krun_disable_implicit_vsock",
    )?;
    check_krun(krun::krun_add_vsock(ctx, 0), "krun_add_vsock")?;
    crate::util::startup_mark_run("runner.devices_configured", &spec.run_id);
    attestation.write_all(b"pvisor-vmm-installed-v1\n")?;
    attestation.sync_data()?;
    crate::util::startup_mark_run("runner.attestation_ready", &spec.run_id);
    let control = std::env::var(CONTROL_FD_ENV)
        .with_context(|| format!("missing {CONTROL_FD_ENV}"))?
        .parse::<RawFd>()?;
    anyhow::ensure!(control == CONTROL_CHILD_FD, "invalid VM control descriptor");
    // Validate the inherited descriptor before taking ownership and keep it
    // out of any later exec in the VMM.
    if unsafe { libc::fcntl(control, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut control = unsafe { std::os::unix::net::UnixStream::from_raw_fd(control) };
    crate::util::startup_mark_run("runner.krun_enter", &spec.run_id);
    let started = krun::krun_start_enter_with_handle(ctx, move |handle| {
        crate::util::startup_mark_run("runner.vmm_built", &spec.run_id);
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        super::pager::start_if_requested(handle.clone())?;
        std::thread::Builder::new()
            .name("pvisor-vm-control".into())
            .spawn(move || {
                use std::io::Read;
                loop {
                    let mut length = [0; 4];
                    if control.read_exact(&mut length).is_err() {
                        // The host supervisor is gone. Do not leave its VM parked
                        // indefinitely, or resume an orphaned workload.
                        std::process::exit(1);
                    }
                    let size = u32::from_be_bytes(length) as usize;
                    if size > super::control::MAX_FRAME {
                        std::process::exit(1);
                    }
                    let mut request = vec![0; size];
                    if control.read_exact(&mut request).is_err() {
                        std::process::exit(1);
                    }
                    use pvisor_core::operation::{OperationKind, VmMemory, VmState};
                    let mut rejection_state = None;
                    let result = match serde_json::from_slice::<OperationKind>(&request) {
                        Ok(OperationKind::RunPause) => {
                            handle.pause().map(|()| (VmState::Paused, None))
                        }
                        Ok(OperationKind::RunResume) => {
                            handle.resume().map(|()| (VmState::Running, None))
                        }
                        Ok(OperationKind::RunOffload { .. }) => {
                            if cfg!(all(target_os = "macos", target_arch = "aarch64")) && std::env::var_os("PVISOR_EXPERIMENTAL_MEMORY_POOL").is_some() {
                                rejection_state = handle.is_paused().ok().map(|paused|
                                    if paused { VmState::Paused } else { VmState::Running });
                                Err("whole-VM offload is incompatible with the experimental cold pager".into())
                            } else { handle.offload_ram().map(|memory| {
                                (
                                    VmState::Offloaded,
                                    Some(VmMemory {
                                        backing_file: PathBuf::new(),
                                        backed_bytes: memory.backed_bytes,
                                        resident_before_bytes: memory.resident_before_bytes,
                                        resident_after_bytes: memory.resident_after_bytes,
                                    }),
                                )
                            }) }
                        }
                        _ => Err("invalid VM control primitive".into()),
                    };
                    let reply = match result {
                        Ok((state, memory)) => super::control::ControlReply {
                            state: Some(state),
                            memory,
                            error: None,
                        },
                        Err(error) => {
                            eprintln!("VM control failed: {error}");
                            super::control::ControlReply {
                                state: rejection_state,
                                memory: None,
                                error: Some(error),
                            }
                        }
                    };
                    let Ok(response) = serde_json::to_vec(&reply) else {
                        std::process::exit(1);
                    };
                    if response.len() > super::control::MAX_FRAME {
                        std::process::exit(1);
                    }
                    if control
                        .write_all(&(response.len() as u32).to_be_bytes())
                        .is_err()
                        || control.write_all(&response).is_err()
                    {
                        std::process::exit(1);
                    }
                }
            })?;
        Ok(())
    });
    if started < 0 {
        attestation.set_len(0)?;
        attestation.sync_data()?;
    }
    #[cfg(target_os = "macos")]
    if started == -libc::EINVAL {
        anyhow::bail!(
            "krun_start_enter failed with errno 22; source-built macOS binaries must be signed \
             with crates/pvisor/macos-hypervisor.entitlements"
        );
    }
    check_krun(started, "krun_start_enter")?;
    Ok(())
}

fn add_krun_overlay(
    ctx: u32,
    tag: &str,
    overlay: &OverlayDeviceSpec,
    shm_size: u64,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !overlay.lowers.is_empty(),
        "libkrun overlay requires a lower directory"
    );
    let policy = CString::new(serde_json::to_string(&overlay.access_policy)?)?;
    let tag = CString::new(tag)?;
    let lowers = overlay
        .lowers
        .iter()
        .map(|path| path_cstring(path))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let lower_ptrs = lowers.iter().map(|path| path.as_ptr()).collect::<Vec<_>>();
    let upper = path_cstring(&overlay.upper)?;
    let work = overlay.work.as_deref().map(path_cstring).transpose()?;
    let preimages = overlay.preimages.as_deref().map(path_cstring).transpose()?;
    let excluded = overlay
        .excluded
        .iter()
        .map(|path| path_cstring(path))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let excluded_ptrs = excluded
        .iter()
        .map(|path| path.as_ptr())
        .collect::<Vec<_>>();
    check_krun(
        unsafe {
            krun::krun_add_virtiofs_overlay_with_policy(
                ctx,
                tag.as_ptr(),
                lower_ptrs.as_ptr(),
                lower_ptrs.len(),
                upper.as_ptr(),
                work.as_ref().map_or(std::ptr::null(), |path| path.as_ptr()),
                preimages
                    .as_ref()
                    .map_or(std::ptr::null(), |path| path.as_ptr()),
                excluded_ptrs.as_ptr(),
                excluded_ptrs.len(),
                shm_size,
                policy.as_ptr(),
            )
        },
        "krun_add_virtiofs_overlay",
    )
}

fn guest_config(
    program: &str,
    args: &[String],
    env: BTreeMap<String, String>,
    cwd: PathBuf,
    workspace: Option<PathBuf>,
    limits: &ResourceLimits,
    network: bool,
) -> pvisor_guest::GuestConfig {
    let mut rlimits = BTreeMap::new();
    for (name, value) in [
        ("RLIMIT_AS", limits.memory_bytes),
        ("RLIMIT_NPROC", limits.processes),
        (
            "RLIMIT_CPU",
            limits.cpu_time_ms.map(|ms| ms.div_ceil(1_000)),
        ),
        ("RLIMIT_NOFILE", limits.open_files),
        ("RLIMIT_FSIZE", limits.file_size_bytes),
    ] {
        if let Some(value) = value {
            rlimits.insert(name.into(), (value, value));
        }
    }
    pvisor_guest::GuestConfig {
        argv: std::iter::once(program.to_owned())
            .chain(args.iter().cloned())
            .collect(),
        env,
        cwd,
        workspace,
        limits: rlimits,
        network: network.then(|| pvisor_guest::NetworkConfig {
            address: pvisor_overlaynet::vm::GUEST_IPV4.octets(),
            gateway: pvisor_overlaynet::vm::ROUTER_IPV4.octets(),
        }),
        agent: None,
    }
}

fn failed_to_start(message: String) -> ExecutorOutput {
    ExecutorOutput {
        executor_observations: Default::default(),

        state: RunState::Failed,

        exit_code: None,
        failure: Some(RunFailure {
            kind: RunFailureKind::Spawn,
            message,
            retryable: false,
        }),
        output: ProcessOutput::default(),
        value: None,
        metrics: Default::default(),
        artifacts: Vec::new(),
        event_stream_ref: None,
        warnings: Vec::new(),
    }
}

fn path_cstring(path: &Path) -> anyhow::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    Ok(CString::new(path.as_os_str().as_bytes())?)
}

fn guest_path_in_root(root: &Path, target: &Path) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        target.is_absolute() && target != Path::new("/"),
        "libkrun guest overlay target must be an absolute path other than /"
    );
    anyhow::ensure!(
        !target
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir)),
        "libkrun guest overlay target must not contain .."
    );
    let relative = target.strip_prefix(Path::new("/"))?;
    let mut resolved = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            continue;
        };
        resolved.push(component);
        match std::fs::symlink_metadata(&resolved) {
            Ok(metadata) => anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "libkrun guest overlay target traverses a symlink: {}",
                target.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(resolved)
}

fn check_ctx(value: i32, operation: &str) -> anyhow::Result<u32> {
    if value < 0 {
        anyhow::bail!("{operation} failed with errno {}", -value);
    }
    Ok(value as u32)
}

fn check_krun(value: i32, operation: &str) -> anyhow::Result<()> {
    if value < 0 {
        anyhow::bail!("{operation} failed with errno {}", -value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn vm_nonzero_exit_is_failed() {
        use std::os::unix::process::ExitStatusExt;
        let (state, code, failure) = guest_exit_outcome(std::process::ExitStatus::from_raw(1 << 8));
        assert_eq!(state, RunState::Failed);
        assert_eq!(code, Some(1));
        assert_eq!(failure.unwrap().kind, RunFailureKind::ProcessExit);
        assert_eq!(
            guest_exit_outcome(std::process::ExitStatus::from_raw(0)).0,
            RunState::Completed
        );
    }

    use super::*;

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn krun_workaround_keeps_slow_path_on_older_or_unknown_kernels() {
        assert!(kernel_release_needs_krun_workaround("6.17.0-foo"));
        assert!(kernel_release_needs_krun_workaround("unknown"));
        assert!(!kernel_release_needs_krun_workaround(
            "7.1.13-200.fc44.x86_64"
        ));
    }

    #[test]
    fn file_rules_cover_original_vm_workspace_paths_and_hide_backing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for name in ["project[1]", "upper", "work", "root-upper"] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        let policy = pvisor_core::overlay::FileAccessPolicy::new(
            vec!["private.key".into()],
            vec![".env".into()],
        )
        .unwrap();
        let workspace = OverlayDeviceSpec {
            lowers: vec![root.join("project[1]")],
            upper: root.join("upper"),
            work: Some(root.join("work")),
            preimages: None,
            excluded: vec![],
            access_policy: policy.clone(),
        };
        let mut device = OverlayDeviceSpec {
            lowers: vec![root.clone()],
            upper: root.join("root-upper"),
            work: None,
            preimages: None,
            excluded: vec![],
            access_policy: policy,
        };
        protect_overlay_backing(&mut device, Some(&workspace)).unwrap();
        let access = &device.access_policy;
        assert!(access.denied(Path::new("project[1]/private.key")));
        assert!(!access.denied(Path::new("project1/private.key")));
        for name in ["root-upper", "upper", "work"] {
            assert!(device.excluded.contains(&PathBuf::from(name)));
        }
    }

    #[test]
    fn settings_validate_resource_limits() {
        let rootfs = tempfile::tempdir().unwrap();
        let settings = VmSettings {
            rootfs: Some(rootfs.path().to_path_buf()),
            ..VmSettings::default()
        };
        assert!(VmExecutor::new(settings.clone()).is_ok());
        assert!(VmExecutor::new(VmSettings::default()).is_err());
        assert!(
            VmExecutor::new(VmSettings {
                cpus: 9,
                ..settings
            })
            .is_err()
        );
    }

    #[test]
    fn guest_config_carries_limits_without_shell_unit_conversion() {
        let config = guest_config(
            "/bin/true",
            &[],
            BTreeMap::new(),
            PathBuf::from("/"),
            None,
            &ResourceLimits {
                open_files: Some(32),
                memory_bytes: Some(4097),
                cpu_time_ms: Some(1001),
                ..ResourceLimits::default()
            },
            true,
        );
        assert_eq!(config.limits["RLIMIT_NOFILE"], (32, 32));
        assert_eq!(config.limits["RLIMIT_AS"], (4097, 4097));
        assert_eq!(config.limits["RLIMIT_CPU"], (2, 2));
        assert!(config.network.is_some());
    }
}
