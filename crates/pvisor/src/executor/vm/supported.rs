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
use pvisor_vm::api::{RamDedupControl, SnapshotControl, VmControl};
use pvisor_vm::api::{RuntimeSupport, VmConfiguration, VmRuntime};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use tokio::process::Command;

pub(super) const RUNNER_SPEC_ENV: &str = "PVISOR_KRUN_RUNNER_SPEC";
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

#[derive(Debug, Clone)]
pub struct VmExecutor {
    settings: VmSettings,
    #[cfg(target_os = "linux")]
    cpu_group: Option<std::sync::Arc<super::cpu_qos::CpuQosGroup>>,
    #[cfg(target_os = "linux")]
    observe_cpu: bool,
    #[cfg(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    restore: Option<std::sync::Arc<super::checkpoint::PreparedRestore>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct RunnerSpec {
    #[serde(default)]
    pub(super) cpu_qos: Option<pvisor_core::CpuQosClass>,
    #[cfg(target_os = "linux")]
    #[serde(default)]
    cpu_group: Option<super::cpu_qos::GroupBinding>,
    #[serde(default)]
    pub(super) run_id: String,
    pub(super) setup_attestation: PathBuf,
    pub(super) root: OverlayDeviceSpec,
    pub(super) workspace: Option<OverlayDeviceSpec>,
    pub(super) workspace_target: Option<PathBuf>,
    pub(super) guest: pvisor_guest::GuestConfig,
    pub(super) cpus: u8,
    pub(super) memory_mib: u32,
    #[serde(default)]
    pub(super) ram_dedup: bool,
    pub(super) library_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) checkpoint: Option<super::checkpoint::LaunchBinding>,
    #[serde(default)]
    pub(super) restore: Option<super::checkpoint::RestoreLaunch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OverlayDeviceSpec {
    pub(super) lowers: Vec<PathBuf>,
    #[serde(default)]
    pub(super) apply_target: Option<PathBuf>,
    #[serde(default)]
    pub(super) baseline_lower: Option<PathBuf>,
    pub(super) upper: PathBuf,
    pub(super) work: Option<PathBuf>,
    #[serde(default)]
    pub(super) preimages: Option<PathBuf>,
    #[serde(default)]
    pub(super) excluded: Vec<PathBuf>,
    #[serde(default)]
    pub(super) access_policy: pvisor_core::overlay::FileAccessPolicy,
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
            for source in workspace.lowers.iter().chain(
                workspace
                    .apply_target
                    .iter()
                    .filter(|target| !workspace.lowers.contains(*target)),
            ) {
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
    /// Fully read the authenticated RAM view into an Attempt-owned file.
    /// Default restore keeps its lazy shared baseline. CPU mappings remain
    /// private; original owners stay pinned for later incremental captures.
    pub(crate) fn materialize_restore_ram(&mut self, storage: &Path) -> anyhow::Result<()> {
        #[cfg(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))]
        {
            use std::{fs, io::Write, os::unix::fs::FileExt, sync::Arc};
            let restore = self
                .restore
                .as_mut()
                .and_then(Arc::get_mut)
                .context("eager RAM requires an unlaunched restore")?;
            let directory = storage.join("execution-restore");
            fs::create_dir_all(&directory)?;
            let mut owned = tempfile::Builder::new()
                .prefix("eager-ram-")
                .tempfile_in(directory)?;
            let bytes = restore.ram.metadata()?.len();
            let mut buffer = vec![0u8; 1024 * 1024];
            let mut offset = 0u64;
            while offset < bytes {
                let count = (bytes - offset).min(buffer.len() as u64) as usize;
                restore.ram.read_exact_at(&mut buffer[..count], offset)?;
                owned.write_all(&buffer[..count])?;
                offset += count as u64;
            }
            owned.as_file().sync_all()?;
            restore.ram = Arc::new(fs::File::open(owned.path())?);
            restore.ram_path = owned.path().to_owned();
            restore._eager_ram = Some(owned);
            Ok(())
        }
        #[cfg(not(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        )))]
        {
            let _ = storage;
            anyhow::bail!("native execution restore is unsupported on this platform")
        }
    }

    /// CLI continuations keep the sealed guest environment rather than inherit
    /// variables from the shell issuing resume/fork. Admission still verifies
    /// the projected configuration against the captured guest contract.
    pub(crate) fn restored_guest_environment(&self) -> Option<BTreeMap<String, String>> {
        #[cfg(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))]
        {
            self.restore
                .as_ref()
                .map(|restore| restore.guest.env.clone())
        }
        #[cfg(not(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        )))]
        {
            None
        }
    }
    /// Derive the actual host/build/firmware restore binding without starting a VM.
    pub fn checkpoint_compatibility(
        settings: &VmSettings,
    ) -> anyhow::Result<crate::environment_snapshot::Compatibility> {
        #[cfg(all(target_os = "linux", target_env = "musl", target_arch = "x86_64"))]
        anyhow::ensure!(
            settings.library_dir.is_none(),
            "static musl checkpoints use the embedded kernel bundle"
        );
        let firmware = settings.library_dir.clone().or_else(bundled_firmware_dir);
        super::checkpoint::compatibility(firmware.as_deref())
    }
    pub fn new(mut settings: VmSettings) -> anyhow::Result<Self> {
        settings.validate_ram_dedup()?;
        anyhow::ensure!(
            settings.snapshot_filesystem_pool.is_none()
                || cfg!(all(target_os = "linux", target_arch = "x86_64"))
                    && settings.memory_pool.is_none()
                    && !settings.ram_compression
                    && settings.ram_backing.is_none(),
            "immutable snapshot pool requires the private-RAM native Linux x86-64 profile"
        );
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
                directory
                    .join(pvisor_vm::api::VmPlatform::firmware_name())
                    .is_file(),
                "vm.library_dir does not contain {}: {}",
                pvisor_vm::api::VmPlatform::firmware_name(),
                directory.display()
            );
        } else if let Some(directory) = pvisor_vm::api::VmPlatform::bundled_firmware_directory() {
            settings.library_dir = Some(directory);
        }
        Ok(Self {
            settings,
            #[cfg(target_os = "linux")]
            cpu_group: None,
            #[cfg(target_os = "linux")]
            observe_cpu: false,
            #[cfg(any(
                all(target_os = "linux", target_arch = "x86_64"),
                all(target_os = "macos", target_arch = "aarch64")
            ))]
            restore: None,
        })
    }

    /// Prepare a new Attempt from a sealed same-host snapshot. Pair the
    /// returned overlay with PVisorBuilder::overlay and the same storage path.
    /// The guest keeps its saved process environment; host identity is new.
    pub fn restore(
        settings: VmSettings,
        checkpoint: pvisor_core::operation::ExecutionCheckpoint,
        storage: &Path,
    ) -> anyhow::Result<(Self, crate::OverlayHint)> {
        #[cfg(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))]
        {
            let mut settings = settings;
            crate::util::create_dir_all_durable(storage)?;
            settings.rootfs = Some(storage.to_owned());
            let mut executor = Self::new(settings)?;
            let (prepared, overlay) = super::checkpoint::native::prepare_restore(
                checkpoint,
                &executor.settings,
                storage,
            )?;
            executor.settings.rootfs = Some(
                prepared
                    .root
                    .lowers
                    .last()
                    .context("restored root has no lower")?
                    .clone(),
            );
            executor.restore = Some(std::sync::Arc::new(prepared));
            Ok((executor, overlay))
        }
        #[cfg(not(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        )))]
        {
            let _ = (settings, checkpoint, storage);
            anyhow::bail!("execution restore is unavailable on this architecture")
        }
    }

    pub fn settings(&self) -> &VmSettings {
        &self.settings
    }

    #[cfg(target_os = "linux")]
    pub fn with_cpu_qos_group(
        mut self,
        group: std::sync::Arc<super::cpu_qos::CpuQosGroup>,
    ) -> Self {
        self.cpu_group = Some(group);
        self
    }

    #[cfg(target_os = "linux")]
    pub fn with_cpu_observation(mut self) -> Self {
        self.observe_cpu = true;
        self
    }

    fn ram_backing(&self) -> anyhow::Result<super::control::RamBacking> {
        #[cfg(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))]
        if let Some(restore) = &self.restore {
            return Ok(super::control::RamBacking::restored(restore.clone()));
        }
        super::control::RamBacking::create(self.settings.ram_backing.as_deref())
    }
}

pub(crate) fn bundled_firmware_dir() -> Option<PathBuf> {
    pvisor_vm::api::VmPlatform::bundled_firmware_directory()
}

// Reserved runner status, accepted only alongside a verified suspend receipt.
const SUSPEND_EXIT_CODE: i32 = 123;

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

    fn supports_cpu_qos(&self) -> bool {
        cfg!(target_os = "linux")
    }

    async fn execute(&self, context: &Session) -> ExecutorOutput {
        crate::util::startup_mark_run("vm.prepare_begin", context.spec().run_id.as_str());
        let mut spec = context.spec().clone();
        #[cfg(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))]
        if let Some(restore) = &self.restore
            && let Some(target) = &restore.workspace_target
        {
            spec.metadata.insert(
                "pvisor.vm.overlay_target".into(),
                serde_json::to_value(target).expect("path serialization"),
            );
        }
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
                    apply_target: None,
                    baseline_lower: None,
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
                    apply_target: None,
                    baseline_lower: None,
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
        crate::image::cache::scrub_guest_environment(&mut env);
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
        // AgentCtl uses a host Unix socket. Runtime injection into the
        // invocation must not expose its credentials to the isolated guest.
        for key in [
            crate::AGENTCTL_ENDPOINT_ENV,
            crate::AGENTCTL_TOKEN_ENV,
            crate::AGENTCTL_TRANSPORT_ENV,
            crate::AGENTCTL_VERSION_ENV,
        ] {
            env.remove(key);
        }

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
                apply_target: root_overlay.apply_target,
                baseline_lower: root_overlay.baseline_lower,
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
        let direct_owners = root_overlay
            .lowers
            .iter()
            .chain(workspace.iter().flat_map(|view| view.lowers.iter()))
            .filter_map(|lower| crate::image::cache::direct_image_owner(lower).map(Path::to_owned))
            .collect::<Vec<_>>();
        for owner in direct_owners {
            if let Err(error) = hide_ram_backing(&mut root_overlay, &owner).and_then(|()| {
                if let Some(workspace) = &mut workspace {
                    hide_ram_backing(workspace, &owner)?;
                }
                Ok(())
            }) {
                return failed_to_start(format!("hide private image backend: {error:#}"));
            }
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
        let mut guest = guest_config(
            &invocation.program,
            &invocation.args,
            env,
            guest_cwd,
            workspace_target.clone(),
            &spec.runtime.resource_limits,
            vm_network_enabled,
        );
        let requested_memory_mib = spec
            .runtime
            .resource_limits
            .memory_bytes
            .map(|bytes| bytes.div_ceil(1024 * 1024).max(1))
            .and_then(|mib| u32::try_from(mib).ok());
        let memory_mib = requested_memory_mib
            .map(|requested| requested.min(self.settings.memory_mib))
            .unwrap_or(self.settings.memory_mib);
        let scratch = pvisor_guest::TemporaryFilesystem {
            path: PathBuf::from(format!("/.pvisor-tmp-{}", spec.run_id)),
            size_bytes: (u64::from(memory_mib) * 1024 * 1024 / 4).min(64 * 1024 * 1024),
        };
        #[cfg(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))]
        let scratch = match &self.restore {
            // The restored process and mount still use their captured path,
            // including after a fork is captured again under a new Run ID.
            Some(restore) => restore.guest.temporary_filesystem.clone(),
            None => Some(scratch),
        };
        #[cfg(not(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        )))]
        let scratch = Some(scratch);
        if !invocation.env.contains_key("TMPDIR")
            && let Some(scratch) = scratch
            && guest.workspace.as_ref().is_none_or(|workspace| {
                !scratch.path.starts_with(workspace) && !workspace.starts_with(&scratch.path)
            })
        {
            guest
                .env
                .insert("TMPDIR".into(), scratch.path.to_string_lossy().into_owned());
            guest.temporary_filesystem = Some(scratch);
        }
        #[cfg(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))]
        if let Some(restore) = &self.restore {
            // The captured environment already contains TMPDIR. That suppresses
            // fresh scratch injection above, but must not erase a captured mount.
            guest.temporary_filesystem = restore.guest.temporary_filesystem.clone();
        }
        if let Err(error) = guest.command() {
            return failed_to_start(error.to_string());
        }
        let attestation = match tempfile::NamedTempFile::new_in(temporary.path()) {
            Ok(file) => file,
            Err(error) => {
                return failed_to_start(error.to_string());
            }
        };
        crate::util::startup_mark_run("vm.ram_backing_begin", spec.run_id.as_str());
        let mut ram_backing = match self.ram_backing() {
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
        crate::util::startup_mark_run("vm.checkpoint_binding_begin", spec.run_id.as_str());
        let checkpoint = if cfg!(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        )) && !vm_network_enabled
            && self.settings.memory_pool.is_none()
        {
            let prepared = if let Some(drivers) = &context.drivers {
                let store = match drivers.execution_snapshot_store() {
                    Ok(store) => store,
                    Err(error) => {
                        return failed_to_start(format!("bind VM checkpoint store: {error:#}"));
                    }
                };
                let run_id = spec.run_id.to_string();
                let attempt_id = context.attempt_id().to_string();
                let firmware = self.settings.library_dir.clone();
                let filesystem_pool = self.settings.snapshot_filesystem_pool.clone();
                tokio::task::spawn_blocking(move || {
                    super::checkpoint::binding(
                        store,
                        run_id,
                        attempt_id,
                        firmware.as_deref(),
                        filesystem_pool.as_deref(),
                    )
                })
                .await
                .map_err(anyhow::Error::from)
                .and_then(|binding| binding.map(Some))
            } else {
                Ok(None)
            };
            match prepared {
                Ok(binding) => binding,
                Err(error) => {
                    return failed_to_start(format!("bind VM checkpoint store: {error:#}"));
                }
            }
        } else {
            None
        };
        #[cfg(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))]
        if let Some(binding) = &checkpoint
            && let Err(error) = super::checkpoint::native::validate_pool_binding(
                binding,
                &root_overlay,
                workspace.as_ref(),
            )
        {
            return failed_to_start(format!("bind immutable snapshot pool: {error:#}"));
        }
        if self.settings.snapshot_filesystem_pool.is_some() && checkpoint.is_none() {
            return failed_to_start(
                "immutable snapshot pool requires the durable no-network native capture profile"
                    .into(),
            );
        }
        crate::util::startup_mark_run("vm.checkpoint_binding_ready", spec.run_id.as_str());
        if let Some(binding) = &checkpoint
            && let Err(error) = hide_ram_backing(&mut root_overlay, &binding.store).and_then(|()| {
                workspace
                    .as_mut()
                    .map_or(Ok(()), |device| hide_ram_backing(device, &binding.store))
            })
        {
            return failed_to_start(format!("hide execution checkpoint store: {error}"));
        }
        #[cfg(target_os = "linux")]
        let cpu_group = if spec.runtime.cpu_qos == Some(pvisor_core::CpuQosClass::LatencySensitive)
        {
            Some(match self.cpu_group.clone() {
                Some(group) => group,
                None => {
                    match tokio::task::spawn_blocking(super::cpu_qos::CpuQosGroup::shared).await {
                        Ok(Ok(group)) => group,
                        other => {
                            return failed_to_start(format!("CPU QoS group creation: {other:?}"));
                        }
                    }
                }
            })
        } else {
            None
        };
        let mut runner = RunnerSpec {
            cpu_qos: spec.runtime.cpu_qos,
            #[cfg(target_os = "linux")]
            cpu_group: cpu_group.as_ref().map(|group| group.binding()),
            run_id: spec.run_id.to_string(),
            setup_attestation: attestation.path().to_path_buf(),
            root: root_overlay,
            workspace,
            workspace_target: workspace_target.clone(),
            guest,
            cpus: self.settings.cpus as u8,
            memory_mib,
            ram_dedup: self.settings.ram_dedup,
            library_dir: self.settings.library_dir.clone(),
            checkpoint,
            restore: None,
        };
        #[cfg(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))]
        if let Some(restore) = &self.restore
            && let Err(error) = apply_restore(&mut runner, restore, context)
        {
            return failed_to_start(format!("restore launch contract: {error:#}"));
        }
        #[cfg(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))]
        if let Some(binding) = &mut runner.checkpoint
            && binding.filesystem_pool.is_some()
        {
            binding.readonly_lowers = match super::checkpoint::native::capture_lower_bindings(
                &runner.root,
                runner.workspace.as_ref(),
            ) {
                Ok(bindings) => bindings,
                Err(error) => {
                    return failed_to_start(format!("bind native lower slots: {error:#}"));
                }
            };
            binding.private_roots = match super::checkpoint::native::capture_private_bindings(
                &runner.root,
                runner.workspace.as_ref(),
            ) {
                Ok(bindings) => bindings,
                Err(error) => {
                    return failed_to_start(format!("bind native private slots: {error:#}"));
                }
            };
        }
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
        #[cfg(target_os = "linux")]
        if let Some(pid) = child.id() {
            context
                .vm_control
                .track_native_process(pid, &ram_backing.file);
        }
        context
            .vm_control
            .attach_with_checkpoint(control_host, ram_backing, runner.checkpoint.clone())
            .await;
        context.transition(RunState::Running, None).await;

        crate::util::startup_mark_run("vm.wait_begin", spec.run_id.as_str());
        #[cfg(target_os = "linux")]
        let mut final_cpu = None;
        #[cfg(not(target_os = "linux"))]
        let final_cpu = None;
        #[cfg(target_os = "linux")]
        let cpu_exit = if self.observe_cpu {
            match context.vm_control.cpu_exit_observer() {
                Ok(observer) => Some(observer),
                Err(error) => {
                    final_cpu = Some(pvisor_core::cpu::TerminalCpuUsage::unavailable(format!(
                        "{error:#}"
                    )));
                    None
                }
            }
        } else {
            None
        };
        #[cfg(target_os = "linux")]
        let mut unreaped = false;
        #[cfg(target_os = "linux")]
        let mut end = if let Some(observer) = &cpu_exit {
            context
                .wait_exit(
                    async {
                        match observer.ready().await {
                            Ok(status) => {
                                unreaped = true;
                                Ok(status)
                            }
                            Err(error) => {
                                final_cpu = Some(pvisor_core::cpu::TerminalCpuUsage::unavailable(
                                    format!("{error:#}"),
                                ));
                                child.wait().await
                            }
                        }
                    },
                    spec.runtime.timeout_ms,
                )
                .await
        } else {
            context
                .wait_child(&mut child, spec.runtime.timeout_ms)
                .await
        };
        #[cfg(target_os = "linux")]
        if unreaped {
            // Exit wins before the blocking probe. A deadline during final
            // observation cannot reclassify an already exited VM.
            final_cpu = Some(cpu_exit.as_ref().unwrap().sample().await);
            let waited = child.wait().await;
            end = match (&end, waited) {
                (End::Exited(Ok(observed)), Ok(reaped)) if observed != &reaped => {
                    End::Exited(Err(std::io::Error::other(
                        "native exit observation differs from authoritative reap",
                    )))
                }
                (_, waited) => End::Exited(waited),
            };
        }
        #[cfg(not(target_os = "linux"))]
        let end = context
            .wait_child(&mut child, spec.runtime.timeout_ms)
            .await;
        crate::util::startup_mark_run("vm.wait_done", spec.run_id.as_str());
        if matches!(end, End::Cancelled | End::Deadline) {
            #[cfg(target_os = "linux")]
            if let Some(observer) = &cpu_exit {
                final_cpu = Some(
                    observer
                        .terminate(process_group, spec.runtime.termination_grace_ms)
                        .await,
                );
            }
            crate::session::lifecycle::terminate_process_tree(
                &mut child,
                process_group,
                spec.runtime.termination_grace_ms,
            )
            .await;
        }
        let suspension = context.vm_control.detach().await;
        let transport_stdout = join_capture(stdout_task).await;
        let transport_stderr = join_capture(stderr_task).await;
        crate::util::startup_mark_run("vm.transport_drained", spec.run_id.as_str());
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
        let (mut state, mut exit_code, mut failure) = match end {
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
            End::Exited(Ok(status))
                if status.code() == Some(SUSPEND_EXIT_CODE) && suspension.is_some() =>
            {
                (RunState::Hibernated, None, None)
            }
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
        let mut executor_observations = ExecutorObservations {
            cpu_usage: final_cpu,
            ..Default::default()
        };
        let receipt = std::fs::read(attestation.path()).unwrap_or_default();
        #[cfg(target_os = "linux")]
        let qos = runner.cpu_qos.and_then(|class| {
            serde_json::from_slice::<pvisor_core::CpuQosObservation>(&receipt)
                .ok()
                .filter(|observation| {
                    super::cpu_qos::valid(observation, class, runner.cpu_group.as_ref())
                })
        });
        #[cfg(not(target_os = "linux"))]
        let qos: Option<pvisor_core::CpuQosObservation> = None;
        let receipt_valid = if runner.cpu_qos.is_some() {
            qos.is_some()
        } else {
            receipt == b"pvisor-vmm-installed-v1\n"
        };
        if runner_exited
            && runner.cpu_qos.is_some()
            && !receipt_valid
            && matches!(state, RunState::Completed | RunState::Hibernated)
        {
            state = RunState::Failed;
            exit_code = None;
            failure = Some(RunFailure {
                kind: RunFailureKind::Infrastructure,
                message: "native runner supplied no valid CPU QoS evidence".into(),
                retryable: false,
            });
        }
        if runner_exited && receipt_valid {
            executor_observations.cpu_qos = qos;
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
        crate::util::startup_mark_run("vm.output_ready", spec.run_id.as_str());
        ExecutorOutput {
            executor_observations,

            state,

            exit_code,
            failure,
            output,
            value: if state == RunState::Hibernated {
                suspension.map(|receipt| {
                    serde_json::to_value(receipt).expect("validated suspension receipt")
                })
            } else {
                None
            },
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

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn apply_restore(
    runner: &mut RunnerSpec,
    restore: &super::checkpoint::PreparedRestore,
    context: &Session,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        runner.cpu_qos == restore.cpu_qos,
        "restore CPU QoS differs from captured class"
    );
    anyhow::ensure!(
        runner.guest.network.is_none() && context.drivers.is_some(),
        "restore requires a durable no-network Attempt"
    );
    anyhow::ensure!(
        context.attempt_id().as_str() != restore.checkpoint.source_attempt_id,
        "restore must use a new Attempt identity"
    );
    anyhow::ensure!(
        context.spec().run_id.as_str() == restore.checkpoint.source_run_id
            || context.spec().parent_run_id.as_ref().map(|id| id.as_str())
                == Some(restore.checkpoint.source_run_id.as_str()),
        "restore parent Run must match the captured source"
    );
    let mut expected_guest = runner.guest.clone();
    let mut saved_guest = restore.guest.clone();
    for key in [
        "PVISOR_RUN_ID",
        "PVISOR_STORAGE",
        "PVISOR_OVERLAY_STAGE",
        "PVISOR_OVERLAY_UPPER",
        "PVISOR_OVERLAY_TARGET",
        "PVISOR_OVERLAY_ID",
    ] {
        expected_guest.env.remove(key);
        saved_guest.env.remove(key);
    }
    anyhow::ensure!(
        expected_guest.argv == saved_guest.argv,
        "restore command differs from captured command"
    );
    anyhow::ensure!(
        expected_guest.cwd == saved_guest.cwd && expected_guest.workspace == saved_guest.workspace,
        "restore guest paths differ from captured paths"
    );
    anyhow::ensure!(
        expected_guest.limits == saved_guest.limits,
        "restore resource limits differ from captured limits"
    );
    anyhow::ensure!(
        expected_guest.agent == saved_guest.agent,
        "restore guest agent differs from captured agent"
    );
    anyhow::ensure!(
        serde_json::to_value(&expected_guest.temporary_filesystem)?
            == serde_json::to_value(&saved_guest.temporary_filesystem)?,
        "restore temporary filesystem differs from captured configuration"
    );
    let changed_keys = expected_guest
        .env
        .keys()
        .chain(saved_guest.env.keys())
        .filter(|key| expected_guest.env.get(*key) != saved_guest.env.get(*key))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    anyhow::ensure!(
        changed_keys.is_empty(),
        "restore projected environment differs at keys: {changed_keys:?}"
    );
    let mut root = restore.root.clone();
    let mut workspace = restore.workspace.clone();
    anyhow::ensure!(
        root.access_policy.same_rules(&runner.root.access_policy),
        "restored root authorization rules changed"
    );
    root.access_policy = runner.root.access_policy.clone();
    if let Some(workspace) = &mut workspace {
        let current = runner
            .workspace
            .as_ref()
            .context("restored workspace projection missing")?;
        anyhow::ensure!(
            workspace.access_policy.same_rules(&current.access_policy),
            "restored workspace authorization rules changed"
        );
        workspace.access_policy = current.access_policy.clone();
    }
    runner.root = root;
    runner.workspace = workspace;
    runner.workspace_target = restore.workspace_target.clone();
    runner.guest = restore.guest.clone();
    runner.restore = Some(restore.launch.clone());
    Ok(())
}

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn write_control_reply(
    control: &mut std::os::unix::net::UnixStream,
    reply: &super::control::ControlReply,
) -> anyhow::Result<()> {
    use std::io::Write;
    let response = serde_json::to_vec(reply)?;
    anyhow::ensure!(
        response.len() <= super::control::MAX_FRAME,
        "VM control response too large"
    );
    control.write_all(&(response.len() as u32).to_be_bytes())?;
    control.write_all(&response)?;
    Ok(())
}

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn capture_checkpoint(
    spec: &RunnerSpec,
    handle: &pvisor_vm::api::VmmHandle,
    control: &mut std::os::unix::net::UnixStream,
    request: super::checkpoint::CaptureRequest,
) -> super::control::ControlReply {
    use pvisor_core::operation::{OperationKind, VmState};
    use std::io::Read;
    let operation = request.operation;
    let suspend = matches!(operation, OperationKind::RunSuspend { .. });
    if !suspend && handle.is_paused().ok() == Some(true) {
        return super::control::ControlReply {
            state: Some(VmState::Paused),
            memory: None,
            error: Some("checkpoint requires a running source VM".into()),
            checkpoint: None,
            capture: None,
        };
    }
    let result = (|| -> Result<_, String> {
        operation.validate().map_err(|e| e.to_string())?;
        let (OperationKind::RunCheckpoint { ram_storage, .. }
        | OperationKind::RunSuspend { ram_storage, .. }) = operation
        else {
            return Err("invalid snapshot operation".into());
        };
        let binding = spec
            .checkpoint
            .as_ref()
            .ok_or("CAPABILITY_UNSUPPORTED: Job has no durable full-device capture binding")?;
        if spec.guest.network.is_some() {
            return Err("CAPABILITY_UNSUPPORTED: network device capture is unavailable".into());
        }
        let capture = |vm: &mut pvisor_vm::api::FrozenMachine<'_>| {
            // Entering this closure proves the device workers and CPUs are
            // frozen. Keep this boundary separate from native capture I/O.
            crate::util::startup_mark_run("checkpoint.native_capture_begin", &binding.run_id);
            let ready = super::checkpoint::native::capture(
                spec,
                vm,
                &request.directory,
                ram_storage,
                request.ram_delta.as_ref(),
                &request.filesystem_reuse,
            )
            .map_err(|e| format!("{e:#}"))?;
            crate::util::startup_mark_run("checkpoint.native_capture_ready", &binding.run_id);
            write_control_reply(
                control,
                &super::control::ControlReply {
                    state: Some(VmState::Paused),
                    memory: None,
                    error: None,
                    checkpoint: None,
                    capture: Some(ready),
                },
            )
            .unwrap_or_else(|error| {
                eprintln!("checkpoint supervisor disconnected while frozen: {error}");
                std::process::exit(1);
            });
            let mut length = [0; 4];
            control.read_exact(&mut length).unwrap_or_else(|error| {
                eprintln!("checkpoint supervisor disconnected while frozen: {error}");
                std::process::exit(1);
            });
            let length = u32::from_be_bytes(length) as usize;
            if length > super::control::MAX_FRAME {
                std::process::exit(1);
            }
            let mut bytes = vec![0; length];
            control
                .read_exact(&mut bytes)
                .unwrap_or_else(|_| std::process::exit(1));
            let commit: super::checkpoint::CommitReply =
                serde_json::from_slice(&bytes).unwrap_or_else(|_| std::process::exit(1));
            crate::util::startup_mark_run("checkpoint.commit_received", &binding.run_id);
            match (commit.checkpoint, commit.error) {
                (Some(checkpoint), None) => {
                    checkpoint
                        .validate()
                        .unwrap_or_else(|_| std::process::exit(1));
                    if checkpoint.store != binding.store
                        || checkpoint.source_run_id != binding.run_id
                        || checkpoint.source_attempt_id != binding.attempt_id
                        || checkpoint.ram_storage != ram_storage
                    {
                        std::process::exit(1);
                    }
                    if suspend {
                        // Acknowledgement proves sealing, not exit. The parent
                        // reaps this process before reporting Hibernated and
                        // the controller releases capacity only on completion.
                        write_control_reply(
                            control,
                            &super::control::ControlReply {
                                state: Some(VmState::Paused),
                                memory: None,
                                error: None,
                                checkpoint: Some(checkpoint),
                                capture: None,
                            },
                        )
                        .unwrap_or_else(|_| std::process::exit(1));
                        crate::util::startup_mark_run(
                            "checkpoint.suspend_ack_sent",
                            &binding.run_id,
                        );
                        // Exit inside the frozen closure: no device/vCPU thaw
                        // and no guest instruction after the sealed point.
                        std::process::exit(SUSPEND_EXIT_CODE);
                    }
                    Ok(checkpoint)
                }
                (None, Some(error)) => Err(format!("host checkpoint publication failed: {error}")),
                _ => std::process::exit(1),
            }
        };
        crate::util::startup_mark_run("checkpoint.freeze_begin", &binding.run_id);
        if suspend {
            handle.with_snapshot_frozen(std::time::Duration::from_secs(30), capture)
        } else {
            handle.with_snapshot_quiesced(std::time::Duration::from_secs(30), capture)
        }
    })();
    match result {
        Ok(checkpoint) => super::control::ControlReply {
            state: Some(VmState::Running),
            memory: None,
            error: None,
            checkpoint: Some(checkpoint),
            capture: None,
        },
        Err(error) => super::control::ControlReply {
            // Only a healthy resumed source is a recoverable rejection. A
            // partial freeze/resume failure stays parked and terminates it.
            state: (!suspend)
                .then(|| {
                    handle
                        .is_paused()
                        .ok()
                        .filter(|paused| !paused)
                        .map(|_| VmState::Running)
                })
                .flatten(),
            memory: None,
            error: Some(error),
            checkpoint: None,
            capture: None,
        },
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
    if crate::image::cache::run_image_access_internal()? {
        return Ok(true);
    }
    #[cfg(target_os = "linux")]
    if super::cpu_qos::run_anchor_if_requested()? {
        return Ok(true);
    }
    if let Some(path) = std::env::var_os("PVISOR_VM_RESTORE_RAM_WATCHDOG") {
        crate::environment_snapshot::watch_mount(Path::new(&path))?;
        return Ok(true);
    }
    if let Some(path) = std::env::var_os("PVISOR_VM_RESTORE_RAM_SERVER") {
        crate::environment_snapshot::serve_ram(Path::new(&path))?;
        return Ok(true);
    }
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
    let direct_lowers = crate::image::cache::attach_runner_lowers(
        spec.root.lowers.iter().chain(
            spec.workspace
                .iter()
                .flat_map(|workspace| workspace.lowers.iter()),
        ),
    )?;
    // Acquire before Landlock and hold until the VM has stopped. Copy restores
    // use ordinary fingerprints; the original generation is no longer leased.
    let baseline_owners = if spec.restore.is_none() {
        std::iter::once(&spec.root)
            .chain(spec.workspace.iter())
            .map(lease_overlay_baseline)
            .collect::<anyhow::Result<Vec<_>>>()?
    } else {
        vec![]
    };
    let baseline_indexes = baseline_owners
        .iter()
        .map(|owner| {
            owner.as_ref().and_then(|base| {
                Some(base.content_index()).map(|(file, sha256)| {
                    pvisor_vm::api::BaselineContentIndex {
                        root: base.root(),
                        file,
                        sha256,
                    }
                })
            })
        })
        .collect::<Vec<_>>();
    #[cfg(target_os = "linux")]
    let _cpu_qos = spec
        .cpu_qos
        .map(|class| super::cpu_qos::apply(class, spec.cpu_group.as_ref()))
        .transpose()?;
    #[cfg(not(target_os = "linux"))]
    anyhow::ensure!(spec.cpu_qos.is_none(), "CPU QoS requires a Linux VM runner");
    let attestation = std::fs::OpenOptions::new()
        .write(true)
        .open(&spec.setup_attestation)?;
    #[cfg(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    let restore = if spec.restore.is_some() {
        let ram = std::env::var(RAM_FD_ENV)?.parse::<RawFd>()?;
        anyhow::ensure!(ram == RAM_CHILD_FD, "invalid restore RAM descriptor");
        if unsafe { libc::fcntl(ram, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Some(super::checkpoint::native::machine_restore(&spec, unsafe {
            std::fs::File::from_raw_fd(ram)
        })?)
    } else {
        None
    };
    #[cfg(target_os = "linux")]
    {
        let mut read_only = spec.root.lowers.clone();
        read_only.extend(direct_lowers.read_only.iter().cloned());
        read_only.extend(
            baseline_indexes
                .iter()
                .flatten()
                .map(|index| index.file.clone()),
        );
        let mut read_write = vec![spec.root.upper.clone()];
        read_write.extend(direct_lowers.read_write.iter().cloned());
        read_write.extend(spec.root.work.iter().cloned());
        read_write.extend(spec.root.preimages.iter().cloned());
        if let Some(binding) = &spec.checkpoint {
            read_write.push(binding.store.clone());
        }
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
    run_linked_krun(
        spec,
        attestation,
        &baseline_indexes,
        #[cfg(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))]
        restore,
    )
}

fn lease_overlay_baseline(
    overlay: &OverlayDeviceSpec,
) -> anyhow::Result<Option<crate::environment_snapshot::SnapshotBase>> {
    let Some(root) = overlay
        .baseline_lower
        .as_ref()
        .or_else(|| overlay.lowers.last())
    else {
        return Ok(None);
    };
    let root = root.canonicalize()?;
    let Some(base) = crate::environment_snapshot::SnapshotBase::lease_root(&root)? else {
        return Ok(None);
    };
    for mutable in std::iter::once(&overlay.upper)
        .chain(overlay.work.iter())
        .chain(overlay.preimages.iter())
        .chain(overlay.apply_target.iter())
    {
        let mutable = mutable.canonicalize()?;
        if root.starts_with(&mutable) || mutable.starts_with(&root) {
            return Ok(None);
        }
    }
    Ok(Some(base))
}

fn run_linked_krun(
    spec: RunnerSpec,
    mut attestation: std::fs::File,
    baseline_indexes: &[Option<pvisor_vm::api::BaselineContentIndex>],
    #[cfg(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    mut restore: Option<pvisor_vm::api::MachineRestore>,
) -> anyhow::Result<()> {
    use std::io::Write;
    if std::env::var_os("PVISOR_KRUN_LOG").is_some() {
        pvisor_vm::api::VmPlatform::init_logging("trace");
    }
    let mut guest = spec.guest.clone();
    // Virtio-console port names arrive asynchronously. Tell PID 1 which
    // non-terminal streams must be ready before it launches the workload.
    guest.stdio_ports = Some(std::array::from_fn(|fd| {
        // SAFETY: isatty only queries this runner's standard descriptor.
        unsafe { libc::isatty(fd as libc::c_int) != 1 }
    }));
    let guest_config = serde_json::to_vec(&guest)?;
    crate::util::startup_mark_run("runner.context_begin", &spec.run_id);
    #[cfg(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    let mut vm = if let Some(restore) = restore.take() {
        pvisor_vm::api::VmBuilder::from_restore(
            pvisor_vm::api::VmConfig {
                cpus: spec.cpus,
                memory_mib: spec.memory_mib,
            },
            restore,
        )?
    } else {
        pvisor_vm::api::VmBuilder::new(spec.cpus, spec.memory_mib)?
    };
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    )))]
    let mut vm = pvisor_vm::api::VmBuilder::new(spec.cpus, spec.memory_mib)?;

    crate::util::startup_mark_run("runner.context_ready", &spec.run_id);
    let ram = std::env::var(RAM_FD_ENV)?.parse::<RawFd>()?;
    anyhow::ensure!(ram == RAM_CHILD_FD, "invalid RAM backing descriptor");
    if unsafe { libc::fcntl(ram, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if spec.restore.is_none() {
        vm.ram_backing(unsafe { std::fs::File::from_raw_fd(ram) })?;
    }
    if spec.checkpoint.is_some() {
        vm.snapshot_profile()?;
    }
    // Both cold launch and restore configure the same built-in supervisor.
    // Restore verifies its virtual inode and restores the saved consumed state;
    // disabling it would change the captured device configuration.

    // OverlayFs cannot service FUSE_SETUPMAPPING; keep DAX disabled.
    add_vm_overlay(
        &mut vm,
        "/dev/root",
        &spec.root,
        baseline_indexes.first().cloned().flatten(),
    )?;
    vm.virtual_file(
        "/dev/root",
        "/.pvisor-guest.json",
        guest_config,
        0o400,
        true,
    )?;
    if let Some(workspace) = &spec.workspace {
        add_vm_overlay(
            &mut vm,
            WORKSPACE_TAG,
            workspace,
            baseline_indexes.get(1).cloned().flatten(),
        )?;
    }
    if let Some(fd) = std::env::var_os(NETWORK_FD_ENV) {
        let fd = fd
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("invalid {NETWORK_FD_ENV}"))?
            .parse::<RawFd>()
            .with_context(|| format!("parse {NETWORK_FD_ENV}"))?;
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        vm.network(
            unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) },
            pvisor_overlaynet::vm::VM_MAC,
        )?;
    }
    // The Rust builder installs only zero-feature vsock, never implicit TSI.

    crate::util::startup_mark_run("runner.devices_configured", &spec.run_id);
    // Private parent/runner IPC: write visibility is sufficient. The parent
    // accepts the receipt only after a normal exit; it is not recovery metadata.
    #[cfg(target_os = "linux")]
    super::cpu_qos::write_attestation(
        &mut attestation,
        spec.cpu_qos
            .map(super::cpu_qos::observe)
            .transpose()?
            .as_ref(),
    )?;
    #[cfg(not(target_os = "linux"))]
    attestation.write_all(b"pvisor-vmm-installed-v1\n")?;
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
    let started = vm.run( move |handle| {
        crate::util::startup_mark_run("runner.vmm_built", &spec.run_id);
        if spec.ram_dedup {
            match handle.advise_ram_dedup() {
                Ok(report) => eprintln!(
                    "VM RAM dedup advice installation: accepted_bytes={} (not merged bytes or savings), mappings={:?}",
                    report.accepted_bytes, report.mappings
                ),
                Err(error) => eprintln!(
                    "VM RAM dedup advice installation failed; continuing VM: {error}"
                ),
            }
        }
        if spec.restore.is_some() {
            handle.resume().map_err(std::io::Error::other)?;
        }
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
                    #[cfg(any(all(target_os = "linux", target_arch = "x86_64"), all(target_os = "macos", target_arch = "aarch64")))]
                    if let Ok(capture) = serde_json::from_slice::<super::checkpoint::CaptureRequest>(&request) {
                        let reply = capture_checkpoint(&spec, &handle, &mut control, capture);
                        if write_control_reply(&mut control, &reply).is_err() { std::process::exit(1); }
                        continue;
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
                        Ok(_) => {
                            rejection_state = handle.is_paused().ok().map(|paused|
                                if paused { VmState::Paused } else { VmState::Running });
                            Err("unsupported VM control primitive".into())
                        }
                        Err(_) => Err("invalid VM control request".into()),
                    };
                    let reply = match result {
                        Ok((state, memory)) => super::control::ControlReply {
                            checkpoint: None,
                            capture: None,
                            state: Some(state),
                            memory,
                            error: None,
                        },
                        Err(error) => {
                            eprintln!("VM control failed: {error}");
                            super::control::ControlReply {
                                checkpoint: None,
                                capture: None,
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
    if let Err(error) = started {
        // A failed build cannot leave an accepted execution receipt.
        attestation.set_len(0)?;
        #[cfg(target_os = "macos")]
        return Err(error).context("VM entry failed; source-built macOS binaries require crates/pvisor/macos-hypervisor.entitlements");
        #[cfg(not(target_os = "macos"))]
        return Err(error.into());
    }
    Ok(())
}

fn add_vm_overlay(
    vm: &mut pvisor_vm::api::VmBuilder,
    tag: &str,
    overlay: &OverlayDeviceSpec,
    baseline_content_index: Option<pvisor_vm::api::BaselineContentIndex>,
) -> anyhow::Result<()> {
    use pvisor_vm::api::{OverlayConfig, PermissionSemantics};
    vm.overlay(
        tag,
        OverlayConfig {
            lower_dirs: overlay.lowers.clone(),
            upper_dir: overlay.upper.clone(),
            work_dir: overlay.work.clone(),
            preimage_dir: overlay.preimages.clone(),
            apply_target: overlay.apply_target.clone(),
            baseline_lower: overlay.baseline_lower.clone(),
            baseline_content_index,
            excluded_paths: overlay.excluded.clone(),
            access_policy: overlay.access_policy.clone(),
            semantics: PermissionSemantics::LinuxComplete,
        },
        0,
    )?;
    Ok(())
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
    // The memory budget sizes guest RAM in the runner specification. It is an
    // aggregate physical-memory boundary, not a guest virtual-address limit:
    // runtimes such as V8 reserve large sparse address ranges without using
    // corresponding RAM. Applying the same bytes as RLIMIT_AS rejects them.
    for (name, value) in [
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
        stdio_ports: None,
        temporary_filesystem: None,
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

#[cfg(test)]
mod tests {
    #[test]
    fn baseline_receipt_lease_requires_an_imported_readonly_generation() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let upper = temp.path().join("upper");
        let target = temp.path().join("target");
        for directory in [&source, &upper, &target] {
            std::fs::create_dir(directory).unwrap();
        }
        std::fs::write(source.join("file"), b"content").unwrap();
        let mut overlay: OverlayDeviceSpec = serde_json::from_value(serde_json::json!({
            "lowers": [source], "upper": upper, "preimages": temp.path().join("not-yet-created")
        }))
        .unwrap();
        assert!(lease_overlay_baseline(&overlay).unwrap().is_none());
        overlay.preimages = None;
        let store =
            crate::environment_snapshot::SnapshotStore::new(&temp.path().join("store")).unwrap();
        let base = store.import_base(&source).unwrap();
        overlay.lowers = vec![base.root()];
        overlay.baseline_lower = Some(base.root());
        overlay.apply_target = Some(target);
        let lease = lease_overlay_baseline(&overlay).unwrap().unwrap();
        assert_eq!(lease.content_index(), base.content_index());
        overlay.apply_target = Some(base.root());
        assert!(lease_overlay_baseline(&overlay).unwrap().is_none());
    }

    #[test]
    fn runner_checkpoint_binding_is_optional_and_roundtrips() {
        let legacy = serde_json::json!({
            "run_id": "run",
            "setup_attestation": "/private/setup.json",
            "root": {"lowers": ["/private/lower"], "upper": "/private/upper"},
            "workspace": null,
            "workspace_target": null,
            "guest": pvisor_guest::GuestConfig::default(),
            "cpus": 1,
            "memory_mib": 128,
            "library_dir": null
        });
        let mut spec: RunnerSpec = serde_json::from_value(legacy.clone()).unwrap();
        assert!(spec.checkpoint.is_none());
        assert!(!spec.ram_dedup);
        let encoded = serde_json::to_value(&spec).unwrap();
        assert!(encoded.get("checkpoint").is_none());

        spec.ram_dedup = true;
        spec.checkpoint = Some(super::super::checkpoint::LaunchBinding {
            store: "/private/snapshots".into(),
            filesystem_pool: None,
            readonly_lowers: vec![],
            private_roots: vec![],
            compatibility: crate::environment_snapshot::Compatibility {
                host_boot: "boot".into(),
                build: "build".into(),
                firmware: "firmware".into(),
                profile: "pvisor-job-owned-overlay-v1".into(),
            },
            run_id: "run".into(),
            attempt_id: "attempt".into(),
        });
        let encoded = serde_json::to_value(&spec).unwrap();
        let decoded: RunnerSpec = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(serde_json::to_value(&decoded).unwrap(), encoded);
        assert_eq!(decoded.checkpoint.unwrap().attempt_id, "attempt");
        assert!(decoded.ram_dedup);
    }

    #[test]
    fn executor_rejects_conflicting_ram_dedup_strategies() {
        for settings in [
            VmSettings {
                ram_dedup: true,
                ram_compression: true,
                ..Default::default()
            },
            VmSettings {
                ram_dedup: true,
                memory_pool: Some("/private/pool/socket".into()),
                ..Default::default()
            },
        ] {
            assert!(
                VmExecutor::new(settings)
                    .unwrap_err()
                    .to_string()
                    .contains("vm.ram_dedup")
            );
        }
    }

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
        for name in ["project[1]", "snapshot", "upper", "work", "root-upper"] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        let policy = pvisor_core::overlay::FileAccessPolicy::new(
            vec!["private.key".into()],
            vec![".env".into()],
        )
        .unwrap();
        for frozen in [false, true] {
            let baseline = if frozen {
                root.join("snapshot")
            } else {
                root.join("project[1]")
            };
            let workspace = OverlayDeviceSpec {
                lowers: vec![baseline.clone()],
                apply_target: Some(root.join("project[1]")),
                baseline_lower: frozen.then_some(baseline),
                upper: root.join("upper"),
                work: Some(root.join("work")),
                preimages: None,
                excluded: vec![],
                access_policy: policy.clone(),
            };
            let mut device = OverlayDeviceSpec {
                lowers: vec![root.clone()],
                apply_target: None,
                baseline_lower: None,
                upper: root.join("root-upper"),
                work: None,
                preimages: None,
                excluded: vec![],
                access_policy: policy.clone(),
            };
            protect_overlay_backing(&mut device, Some(&workspace)).unwrap();
            let access = &device.access_policy;
            assert!(access.denied(Path::new("project[1]/private.key")));
            if frozen {
                assert!(access.denied(Path::new("snapshot/private.key")));
            }
            assert!(!access.denied(Path::new("project1/private.key")));
            for name in ["root-upper", "upper", "work"] {
                assert!(device.excluded.contains(&PathBuf::from(name)));
            }
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
        assert!(!config.limits.contains_key("RLIMIT_AS"));
        assert_eq!(config.limits["RLIMIT_CPU"], (2, 2));
        assert!(config.network.is_some());
    }
}
