//! Durable Run identity, project association, and liveness metadata.

use super::host_transport::{
    allocate_host_directory, authorize_host_peer, host_authority_root, read_host_frame,
    validate_host_target, write_host_frame,
};
use super::overlay::{
    OverlayRecord, ReadOnlyOverlayMount, load_overlay_record, mount_overlay_record_read_only,
    overlay_status,
};
use crate::util::create_dir_all_durable;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use pvisor_core::host_protocol::{
    AGENTCTL_HOST_VERSION, AgentCtlHostError, AgentCtlHostErrorCode, AgentCtlHostRequest,
    AgentCtlHostResponse, AgentCtlTarget,
};
pub use pvisor_core::overlay::OverlayStatus as ControlOverlayStatus;
use pvisor_core::{ExecutorIdentity, ExecutorPlan, ResourceLimits};

pub const RUN_META_FILENAME: &str = "run.json";
pub const LEASE_FILENAME: &str = "lease.lock";
pub const CONTROL_FILENAME: &str = "control.sock";
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);
const CONTROL_MAX_INSPECT_MOUNTS: usize = 16;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum HostOverlayCommand {
    Ping {},
    OverlayStatus {},
    Observations {},
    MountInspect {},
    UnmountInspect { id: String },
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostOverlayResult {
    id: Option<String>,
    mountpoint: Option<PathBuf>,
    overlay_status: Option<ControlOverlayStatus>,
    observations: Option<serde_json::Value>,
}

fn control_envelope(
    record: &RunRecord,
    command: HostOverlayCommand,
) -> anyhow::Result<AgentCtlHostRequest<HostOverlayCommand>> {
    let request = AgentCtlHostRequest {
        version: AGENTCTL_HOST_VERSION,
        request_id: uuid::Uuid::new_v4().to_string(),
        target: Some(AgentCtlTarget {
            job_id: record.run_id.clone(),
            attempt_id: Some(
                record
                    .attempt_id
                    .clone()
                    .context("control requires an Attempt identity")?,
            ),
            generation: None,
        }),
        command,
    };
    request.validate()?;
    Ok(request)
}

pub fn default_run_home() -> PathBuf {
    if let Some(root) = std::env::var_os("PVISOR_RUN_HOME") {
        return PathBuf::from(root);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".pvisor").join("runs");
    }
    std::env::temp_dir().join("pvisor-runs")
}

/// Provenance for a Run started from a logical or full execution checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunLineage {
    pub parent_run_id: String,
    pub checkpoint_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvironmentProjection {
    #[serde(default)]
    pub inherits_host: bool,
    #[serde(default)]
    pub projected_keys: Vec<String>,
    #[serde(default)]
    pub runtime_injected_keys: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunRecordState {
    Running,
    Completed,
    Cancelled,
    Failed,
    Terminated,
    Hibernated,
}

impl RunRecordState {
    pub fn is_stopped(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Cancelled | Self::Failed | Self::Terminated | Self::Hibernated
        )
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Terminated => "terminated",
            Self::Hibernated => "hibernated",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub schema_version: u32,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub session_id: String,
    pub agent: String,
    pub pid: u32,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<ExecutorIdentity>,
    /// Admission-only data carried in memory to the authoritative Bundle.
    #[serde(skip)]
    pub executor_plan: Option<ExecutorPlan>,
    pub state: RunRecordState,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: Option<u64>,
    pub storage: PathBuf,
    /// Reusable project workspace associated with this Run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    #[serde(default)]
    pub overlaynet_listen: Option<String>,
    #[serde(default)]
    pub network_interception: Option<pvisor_overlaynet::InterceptionProfile>,
    #[serde(default)]
    pub network_interception_metrics: Option<pvisor_overlaynet::InterceptionSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem_observation: Option<pvisor_core::operation::FilesystemObservation>,
    pub gateway_listen: Option<String>,
    pub network: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_policy: Option<serde_json::Value>,
    #[serde(default)]
    pub environment: EnvironmentProjection,
    #[serde(default)]
    pub resource_limits: ResourceLimits,
    pub overlay: Option<OverlayRecord>,
    #[serde(default)]
    pub overlay_lowers: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<RunLineage>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub orchestration: std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(skip)]
    pub operation: Option<pvisor_core::operation::Operation>,
}

impl RunRecord {
    /// A missing process/lease is insufficient evidence of a completed Attempt.
    pub fn require_stopped(&self) -> anyhow::Result<()> {
        super::job_execution::require_mutable(self)?;
        anyhow::ensure!(
            self.state.is_stopped() && self.finished_at_unix_ms.is_some(),
            "EXECUTION_UNKNOWN: Job {} has no confirmed stopped Attempt (state={}); refuse workspace mutation or capture",
            self.run_id,
            self.state.as_str()
        );
        Ok(())
    }
    /// Read the authoritative record only after obtaining mutation ownership.
    /// A selector's earlier snapshot is not safe input to apply/drop/recovery.
    pub fn lock_current(&self) -> anyhow::Result<(Self, RunLease)> {
        let stage = self.stage_dir();
        let lease = RunLease::acquire(&stage)?;
        let current = Self::read(&stage)?;
        anyhow::ensure!(
            current.run_id == self.run_id,
            "Run identity changed while acquiring its lease"
        );
        anyhow::ensure!(
            current.stage_dir() == stage,
            "Run backing changed while acquiring its lease"
        );
        Ok((current, lease))
    }
    pub fn stage_dir(&self) -> PathBuf {
        self.overlay
            .as_ref()
            .map(|record| record.stage_dir.clone())
            .unwrap_or_else(|| self.storage.clone())
    }

    pub fn write(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.schema_version == 1,
            "unsupported Run record schema {}",
            self.schema_version
        );
        let stage = self.stage_dir();
        let path = stage.join(RUN_META_FILENAME);
        crate::util::write_run_json(&path, self, &self.run_id, "run_record")?;

        let index_dir = self.storage.join(".pvisor").join("runs");
        let index_path = index_dir.join(format!(
            "{}.json",
            crate::util::encode_hex(self.run_id.as_bytes())
        ));
        let contents =
            crate::util::persistence_step(&self.run_id, "run_index", "serialize", || {
                serde_json::to_vec_pretty(&RunIndex {
                    run_id: self.run_id.clone(),
                    stage_dir: stage,
                })
            })?;
        // State updates do not move the Run. Keep the existing index inode,
        // but still confirm durability (including a prior failed directory sync).
        let unchanged = match fs::symlink_metadata(&index_path) {
            Ok(metadata)
                if metadata.is_file() && metadata.permissions().mode() & 0o777 == 0o600 =>
            {
                fs::read(&index_path)? == contents
            }
            Ok(_) => false,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if unchanged {
            crate::util::persistence_step(&self.run_id, "run_index", "file_sync", || {
                File::open(&index_path)?.sync_all()
            })?;
            crate::util::persistence_step(&self.run_id, "run_index", "directory_sync", || {
                crate::util::sync_directory(&index_dir)
            })?;
        } else {
            crate::util::write_run_bytes(&index_path, &contents, &self.run_id, "run_index")?;
        }
        Ok(())
    }

    pub fn read(stage: &Path) -> anyhow::Result<Self> {
        // A Job's original stage remains a stable selector across Attempts.
        // The active Attempt owns its own run.json, Bundle and storage lease.
        let active = super::job_execution::Job::read_stage(stage)?;
        let stage = active.as_ref().map_or(stage, |job| {
            if job.state == super::job_execution::JobState::Restoring
                && !job.active_stage.join(RUN_META_FILENAME).exists()
            {
                job.previous_stage.as_path()
            } else {
                job.active_stage.as_path()
            }
        });
        let path = stage.join(RUN_META_FILENAME);
        let mut record: Self = serde_json::from_slice(&fs::read(&path)?)?;
        anyhow::ensure!(
            record.schema_version == 1,
            "unsupported Run record schema {}",
            record.schema_version
        );
        if let Some(job) = active {
            anyhow::ensure!(
                record.run_id == job.run_id,
                "execution Job record owner mismatch"
            );
        }
        if let Some(overlay) = record.overlay.as_mut()
            && pvisor_overlay_core::apply::reconcile_terminal_overlay(overlay)?
        {
            // Applied is published before ledger commit. Preserve its original
            // generation while pending so locked recovery can still match it.
            if !pvisor_overlay_core::apply::has_pending_applies(overlay)? {
                overlay.generation = overlay
                    .generation
                    .checked_add(1)
                    .context("workspace generation exhausted")?;
            }
            if let Some(lower) = record.overlay_lowers.last_mut() {
                *lower = overlay.target.clone();
            }
        }
        Ok(record)
    }

    pub fn remove_index(&self) -> anyhow::Result<()> {
        let path = self.storage.join(".pvisor").join("runs").join(format!(
            "{}.json",
            crate::util::encode_hex(self.run_id.as_bytes())
        ));
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunIndex {
    run_id: String,
    stage_dir: PathBuf,
}

/// Exclusive process-lifetime lease. Its file is intentionally retained.
pub struct RunLease {
    file: File,
}

impl RunLease {
    /// Reserve a new Run's backing before any mount or metadata mutation.
    /// Invalid existing metadata is an error, never permission to overwrite it.
    pub fn acquire_new(stage_dir: &Path) -> anyhow::Result<Self> {
        let lease = Self::acquire(stage_dir)?;
        if stage_dir.join(RUN_META_FILENAME).try_exists()? {
            let existing = RunRecord::read(stage_dir)?;
            anyhow::bail!(
                "Run storage {} already belongs to Run {}; choose unique storage",
                stage_dir.display(),
                existing.run_id
            );
        }
        Ok(lease)
    }
    pub fn acquire(stage_dir: &Path) -> anyhow::Result<Self> {
        create_dir_all_durable(stage_dir)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(stage_dir.join(LEASE_FILENAME))?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            anyhow::bail!("Run storage is already leased: {}", stage_dir.display());
        }
        Ok(Self { file })
    }
}

impl Drop for RunLease {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Attempt-scoped local control endpoint. The owning pVisor creates read-only
/// views so a second CLI process never interferes with a live writable mount.
pub struct RunControlServer {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    _locator: ControlLocator,
    _socket_dir: tempfile::TempDir,
}

// Retain ownership of discovery separately from the temporary socket directory.
// A failed startup or teardown must not remove a replacement supplied by a user.
struct ControlLocator {
    path: PathBuf,
    target: PathBuf,
    metadata: fs::Metadata,
}

impl ControlLocator {
    fn create(path: PathBuf, target: &Path) -> anyhow::Result<Self> {
        // Do not replace even a dangling legacy symlink or a stopped socket.
        std::os::unix::fs::symlink(target, &path)?;
        let metadata = fs::symlink_metadata(&path)?;
        Ok(Self {
            path,
            target: target.to_path_buf(),
            metadata,
        })
    }
}

impl Drop for ControlLocator {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.file_type().is_symlink()
                && metadata.dev() == self.metadata.dev()
                && metadata.ino() == self.metadata.ino()
        }) && fs::read_link(&self.path).is_ok_and(|target| target == self.target)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn control_socket_path(stage: &Path) -> anyhow::Result<PathBuf> {
    let path = fs::read_link(stage.join(CONTROL_FILENAME))?;
    let root = host_authority_root()?;
    let directory = path.parent().context("control socket missing parent")?;
    anyhow::ensure!(
        path.file_name() == Some(std::ffi::OsStr::new(CONTROL_FILENAME))
            && directory.parent() == Some(root.as_path()),
        "control socket must be inside the host authority root"
    );
    let metadata = fs::symlink_metadata(directory)?;
    anyhow::ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o7777 == 0o700
            && directory.canonicalize()? == directory,
        "control socket directory must be same-UID, non-symlink and 0700"
    );
    let metadata = fs::symlink_metadata(&path)?;
    anyhow::ensure!(
        metadata.file_type().is_socket()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o7777 == 0o600,
        "control socket must be same-UID, non-symlink and 0600"
    );
    Ok(path)
}

impl RunControlServer {
    pub fn start(record: &RunRecord) -> anyhow::Result<Option<Self>> {
        Self::start_observed(record, None, None)
    }

    pub fn start_observed(
        record: &RunRecord,
        filesystem: Option<pvisor_overlayfs::FsMetrics>,
        network: Option<pvisor_overlaynet::InterceptionMetrics>,
    ) -> anyhow::Result<Option<Self>> {
        let Some(overlay) = record.overlay.clone() else {
            return Ok(None);
        };
        let identity = control_envelope(record, HostOverlayCommand::Ping {})?;
        let target = identity.target.unwrap();
        let lowers = if record.overlay_lowers.is_empty() {
            vec![overlay.target.clone()]
        } else {
            record.overlay_lowers.clone()
        };
        let locator_path = record.stage_dir().join(CONTROL_FILENAME);
        // Keep host authority under the shared guest-excluded root, with short
        // paths for macOS sockaddr_un. The stage contains only a discovery link.
        let socket_dir = allocate_host_directory("overlay-")?;
        let socket_path = socket_dir.path().join("control.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let locator = ControlLocator::create(locator_path, &socket_path)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let stage = record.stage_dir();
        let join = std::thread::Builder::new()
            .name(format!("pvisor-core-{}", record.run_id))
            .spawn(move || {
                let mut mounts: HashMap<String, ReadOnlyOverlayMount> = HashMap::new();
                while !thread_stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            serve_control(
                                stream,
                                ControlContext {
                                    stage: &stage,
                                    target: &target,
                                    overlay: &overlay,
                                    lowers: &lowers,
                                    mounts: &mut mounts,
                                    filesystem: filesystem.as_ref(),
                                    network: network.as_ref(),
                                },
                            );
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Some(Self {
            stop,
            join: Some(join),
            _locator: locator,
            _socket_dir: socket_dir,
        }))
    }
}

impl Drop for RunControlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        // Field owners clean up discovery and the private socket directory.
    }
}

struct ControlContext<'a> {
    stage: &'a Path,
    target: &'a AgentCtlTarget,
    overlay: &'a OverlayRecord,
    lowers: &'a [PathBuf],
    mounts: &'a mut HashMap<String, ReadOnlyOverlayMount>,
    filesystem: Option<&'a pvisor_overlayfs::FsMetrics>,
    network: Option<&'a pvisor_overlaynet::InterceptionMetrics>,
}

fn serve_control(stream: std::os::unix::net::UnixStream, context: ControlContext<'_>) {
    let ControlContext {
        stage,
        target,
        overlay,
        lowers,
        mounts,
        filesystem,
        network,
    } = context;
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    runtime.block_on(async {
        if stream.set_nonblocking(true).is_err() {
            return;
        }
        let Ok(mut stream) = tokio::net::UnixStream::from_std(stream) else {
            return;
        };
        if authorize_host_peer(&stream).is_err() {
            return;
        }
        // Malformed frames have no trustworthy correlation ID.
        let Ok(Ok(request)) = tokio::time::timeout(
            CONTROL_TIMEOUT,
            read_host_frame::<AgentCtlHostRequest<HostOverlayCommand>>(&mut stream),
        )
        .await
        else {
            return;
        };
        let result = request
            .validate()
            .and_then(|()| {
                validate_host_target(
                    request.target.as_ref(),
                    &target.job_id,
                    target.attempt_id.as_deref().unwrap(),
                )
            })
            .and_then(|()| {
                execute_control(
                    request.command,
                    stage,
                    overlay,
                    lowers,
                    mounts,
                    filesystem,
                    network,
                )
            });
        let response = AgentCtlHostResponse {
            version: AGENTCTL_HOST_VERSION,
            request_id: request.request_id,
            result,
        };
        let _ =
            tokio::time::timeout(CONTROL_TIMEOUT, write_host_frame(&mut stream, &response)).await;
    });
}

fn execute_control(
    command: HostOverlayCommand,
    stage: &Path,
    overlay: &OverlayRecord,
    lowers: &[PathBuf],
    mounts: &mut HashMap<String, ReadOnlyOverlayMount>,
    filesystem: Option<&pvisor_overlayfs::FsMetrics>,
    network: Option<&pvisor_overlaynet::InterceptionMetrics>,
) -> Result<HostOverlayResult, AgentCtlHostError> {
    match command {
        HostOverlayCommand::Ping {} => Ok(HostOverlayResult::default()),
        HostOverlayCommand::OverlayStatus {} => overlay_status(overlay)
            .map(|status| HostOverlayResult {
                overlay_status: Some(status),
                ..Default::default()
            })
            .map_err(control_error),
        HostOverlayCommand::Observations {} => Ok(HostOverlayResult {
            observations: Some(serde_json::json!({
                "filesystem": filesystem.map(|metrics| metrics.snapshot()),
                "network": network.map(|metrics| metrics.snapshot()),
            })),
            ..Default::default()
        }),
        HostOverlayCommand::MountInspect {} => {
            if mounts.len() >= CONTROL_MAX_INSPECT_MOUNTS {
                return Err(AgentCtlHostError::new(
                    AgentCtlHostErrorCode::Unavailable,
                    "inspect session limit reached",
                ));
            }
            let id = uuid::Uuid::new_v4().to_string();
            let mountpoint = stage.join("inspect").join(&id).join("merged");
            match mount_overlay_record_read_only(overlay, lowers, &mountpoint) {
                Ok(mount) => {
                    let mountpoint = mount.mountpoint().to_path_buf();
                    mounts.insert(id.clone(), mount);
                    Ok(HostOverlayResult {
                        id: Some(id),
                        mountpoint: Some(mountpoint),
                        ..Default::default()
                    })
                }
                Err(error) => Err(control_error(error)),
            }
        }
        HostOverlayCommand::UnmountInspect { id } => {
            if let Some(mount) = mounts.remove(&id) {
                match mount.unmount() {
                    Ok(()) => Ok(HostOverlayResult::default()),
                    Err(error) => Err(control_error(error)),
                }
            } else {
                Err(AgentCtlHostError::new(
                    AgentCtlHostErrorCode::InvalidRequest,
                    format!("unknown inspect session {id}"),
                ))
            }
        }
    }
}

fn control_error(error: impl std::fmt::Display) -> AgentCtlHostError {
    AgentCtlHostError::new(AgentCtlHostErrorCode::Internal, error.to_string())
}

fn control_request(stage: &Path, command: HostOverlayCommand) -> anyhow::Result<HostOverlayResult> {
    let request = control_envelope(&RunRecord::read(stage)?, command)?;
    let socket_path = control_socket_path(stage)?;
    // These synchronous APIs are also called from Tokio contexts. Keep the
    // private transport runtime on a separate thread rather than nesting it.
    std::thread::scope(|scope| {
        scope
            .spawn(|| -> anyhow::Result<HostOverlayResult> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(async {
                    tokio::time::timeout(CONTROL_TIMEOUT, async {
                        let mut stream = tokio::net::UnixStream::connect(&socket_path).await?;
                        authorize_host_peer(&stream)?;
                        write_host_frame(&mut stream, &request).await?;
                        let response: AgentCtlHostResponse<HostOverlayResult> =
                            read_host_frame(&mut stream).await?;
                        response.validate(&request.request_id)?;
                        Ok(response.result?)
                    })
                    .await
                    .map_err(|_| anyhow::anyhow!("control request timed out"))?
                })
            })
            .join()
            .map_err(|_| anyhow::anyhow!("control transport thread panicked"))?
    })
}

pub fn control_ping(stage: &Path) -> bool {
    control_request(stage, HostOverlayCommand::Ping {}).is_ok()
}

pub fn control_mount_inspect(stage: &Path) -> anyhow::Result<(String, PathBuf)> {
    let response = control_request(stage, HostOverlayCommand::MountInspect {})?;
    Ok((
        response.id.context("control response missing inspect id")?,
        response
            .mountpoint
            .context("control response missing inspect mountpoint")?,
    ))
}

pub fn control_overlay_status(stage: &Path) -> anyhow::Result<ControlOverlayStatus> {
    control_request(stage, HostOverlayCommand::OverlayStatus {})?
        .overlay_status
        .context("control response missing OverlayFS status")
}

pub fn control_observations(stage: &Path) -> anyhow::Result<serde_json::Value> {
    control_request(stage, HostOverlayCommand::Observations {})?
        .observations
        .context("control response missing observations")
}

pub fn control_unmount_inspect(stage: &Path, id: String) -> anyhow::Result<()> {
    control_request(stage, HostOverlayCommand::UnmountInspect { id })?;
    Ok(())
}

pub fn is_live(stage_dir: &Path) -> anyhow::Result<bool> {
    let path = stage_dir.join(LEASE_FILENAME);
    if !path.exists() {
        return Ok(false);
    }
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        Ok(false)
    } else {
        let error = std::io::Error::last_os_error();
        if error
            .raw_os_error()
            .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN)
        {
            Ok(true)
        } else {
            Err(error.into())
        }
    }
}

/// Resolve a Run from a run id, stage, upper directory, database, or a path
/// inside the target/merged workspace.
pub fn resolve_run(selector: Option<&Path>, storage: &Path) -> anyhow::Result<RunRecord> {
    if let Some(selector) = selector {
        if selector == Path::new("last") {
            return resolve_last(storage, std::env::current_dir().ok().as_deref());
        }
        if selector.exists() || selector.components().count() > 1 {
            return resolve_path(selector);
        }
        let id = selector.to_string_lossy();
        let index = storage
            .join(".pvisor")
            .join("runs")
            .join(format!("{}.json", crate::util::encode_hex(id.as_bytes())));
        if index.exists() {
            let index: RunIndex = serde_json::from_slice(&fs::read(index)?)?;
            return RunRecord::read(&index.stage_dir);
        }
        if let Some(record) = default_runs()?
            .into_iter()
            .find(|record| record.run_id == id)
        {
            return Ok(record);
        }
        anyhow::bail!("pVisor Run not found: {}", selector.display());
    }

    // No explicit selector means "the latest Run for this workspace", with the
    // same safety as `last`: never silently return another workspace's Job.
    resolve_last(storage, std::env::current_dir().ok().as_deref())
}

fn resolve_last(storage: &Path, current: Option<&Path>) -> anyhow::Result<RunRecord> {
    if let Some(current) = current {
        if let Ok(record) = resolve_path(current) {
            return Ok(record);
        }
        if let Ok(record) = latest_workspace_run(current) {
            return Ok(record);
        }
        // Do not silently fall back to the newest Run from another workspace:
        // `last` would then point `status`/`apply`/`drop` at an unrelated Job,
        // and `apply` would write that Job's changes back into its own target.
        anyhow::bail!(
            "no pVisor Run found for the current workspace ({}); `last` resolves only \
             Runs registered for this workspace. Pass the Job id or an explicit stage \
             path, for example `pvisor status --review PATH`",
            current.display()
        );
    }
    latest_run(storage).or_else(|_| latest_default_run())
}

fn default_runs() -> anyhow::Result<Vec<RunRecord>> {
    let mut records = Vec::new();
    let mut roots = vec![default_run_home(), std::env::temp_dir().join("pvisor-runs")];
    roots.sort();
    roots.dedup();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for entry in fs::read_dir(root)? {
            let storage = entry?.path();
            if let Ok(record) = RunRecord::read(&storage) {
                records.push(record);
            } else {
                records.extend(all_runs(&storage)?);
            }
        }
    }
    records.sort_by_key(|record| std::cmp::Reverse(record.started_at_unix_ms));
    Ok(records)
}

fn latest_default_run() -> anyhow::Result<RunRecord> {
    default_runs()?.into_iter().next().ok_or_else(|| {
        anyhow::anyhow!(
            "no pVisor Runs found under {}",
            default_run_home().display()
        )
    })
}

fn latest_workspace_run(workspace: &Path) -> anyhow::Result<RunRecord> {
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    default_runs()?
        .into_iter()
        .find(|record| {
            record.workspace.as_ref().is_some_and(|root| {
                let root = root.canonicalize().unwrap_or_else(|_| root.clone());
                workspace.starts_with(root)
            })
        })
        .ok_or_else(|| {
            anyhow::anyhow!("no pVisor Runs found for workspace {}", workspace.display())
        })
}

fn resolve_path(path: &Path) -> anyhow::Result<RunRecord> {
    let absolute = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if absolute.join(".pvisor").join("runs").is_dir() {
        return latest_run(&absolute);
    }
    let mut candidates = Vec::new();
    if absolute.is_file() {
        if absolute
            .file_name()
            .is_some_and(|name| name == RUN_META_FILENAME)
        {
            candidates.push(absolute.parent().unwrap_or(Path::new(".")).to_path_buf());
        } else if let Some(parent) = absolute.parent() {
            candidates.push(parent.to_path_buf());
        }
    } else {
        candidates.push(absolute.clone());
        if absolute.file_name().is_some_and(|name| name == "upper")
            && let Some(parent) = absolute.parent()
        {
            candidates.push(parent.to_path_buf());
        }
    }
    candidates.extend(absolute.ancestors().map(Path::to_path_buf));
    for stage in candidates {
        if stage.join(RUN_META_FILENAME).is_file() {
            return RunRecord::read(&stage);
        }
        if stage.join("overlay.json").is_file() {
            let overlay = load_overlay_record(&stage)?;
            if let Ok(record) = RunRecord::read(&overlay.stage_dir) {
                return Ok(record);
            }
        }
    }

    if let Ok(record) = latest_workspace_run(&absolute) {
        return Ok(record);
    }

    // A target or merged path is not necessarily below stage_dir. Scan the
    // nearest project storage and compare canonical roots.
    for ancestor in absolute.ancestors() {
        let storage = ancestor.join(".pvisor").join("capture");
        if storage.is_dir() {
            for record in all_runs(&storage)? {
                if record.overlay.as_ref().is_some_and(|overlay| {
                    path_within(&absolute, &overlay.target)
                        || path_within(&absolute, &overlay.merged_dir)
                        || path_within(&absolute, overlay.upper.path())
                }) {
                    return Ok(record);
                }
            }
        }
    }
    anyhow::bail!("no pVisor Run metadata found for {}", path.display())
}

fn path_within(path: &Path, root: &Path) -> bool {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    path.starts_with(root)
}

pub fn all_runs(storage: &Path) -> anyhow::Result<Vec<RunRecord>> {
    let dir = storage.join(".pvisor").join("runs");
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let index: RunIndex = match serde_json::from_slice(&fs::read(path)?) {
                Ok(index) => index,
                Err(_) => continue,
            };
            if let Ok(record) = RunRecord::read(&index.stage_dir) {
                records.push(record);
            }
        }
    }
    records.sort_by_key(|record| std::cmp::Reverse(record.started_at_unix_ms));
    Ok(records)
}

fn latest_run(storage: &Path) -> anyhow::Result<RunRecord> {
    all_runs(storage)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no pVisor Runs found under {}", storage.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::OverlayUpper;

    fn record(storage: &Path, stage: &Path, upper: &Path) -> RunRecord {
        RunRecord {
            attempt_id: None,
            schema_version: 1,
            run_id: "run-test".into(),
            parent_run_id: None,
            task_id: None,
            session_id: "session-test".into(),
            agent: "test".into(),
            pid: 1,
            command: vec!["true".into()],
            executor: None,
            executor_plan: None,
            state: RunRecordState::Completed,
            started_at_unix_ms: 1,
            finished_at_unix_ms: Some(2),
            storage: storage.to_path_buf(),
            workspace: None,
            overlaynet_listen: None,
            network_interception: None,
            network_interception_metrics: None,
            filesystem_observation: None,
            gateway_listen: None,
            network: serde_json::json!({"mode": "ambient"}),
            network_policy: None,
            environment: Default::default(),
            resource_limits: Default::default(),
            overlay: Some(OverlayRecord {
                id: "session-test".into(),
                generation: 0,
                target: storage.join("target"),
                baseline_lower: None,
                upper: OverlayUpper {
                    upper_dir: upper.to_path_buf(),
                    work_dir: stage.join("work"),
                },
                merged_dir: stage.join("merged"),
                stage_dir: stage.to_path_buf(),
                excluded_paths: Vec::new(),
                access_policy: Default::default(),
                auto_apply: false,
                auto_discard: false,
                protect_target: false,
                state: super::super::overlay::OverlayState::Staged,
            }),
            overlay_lowers: vec![storage.join("target")],
            lineage: None,
            orchestration: Default::default(),
            operation: None,
        }
    }

    #[test]
    fn state_updates_reuse_index_and_moves_republish_it() {
        use std::os::unix::fs::MetadataExt;
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("stage");
        let mut run = record(temp.path(), &stage, &stage.join("upper"));
        run.write().unwrap();
        let index = temp.path().join(".pvisor/runs").join(format!(
            "{}.json",
            crate::util::encode_hex(run.run_id.as_bytes())
        ));
        let original = fs::metadata(&index).unwrap().ino();
        run.state = RunRecordState::Failed;
        run.write().unwrap();
        assert_eq!(fs::metadata(&index).unwrap().ino(), original);
        assert_eq!(
            RunRecord::read(&stage).unwrap().state,
            RunRecordState::Failed
        );
        let moved = temp.path().join("moved");
        run.overlay.as_mut().unwrap().stage_dir = moved.clone();
        run.write().unwrap();
        let updated: RunIndex = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
        assert_eq!(updated.stage_dir, moved);
        // An otherwise identical index with public permissions must be repaired.
        fs::set_permissions(&index, fs::Permissions::from_mode(0o644)).unwrap();
        run.write().unwrap();
        assert_eq!(
            fs::metadata(&index).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::write(&index, b"broken").unwrap();
        run.write().unwrap();
        assert!(serde_json::from_slice::<RunIndex>(&fs::read(&index).unwrap()).is_ok());
    }

    #[test]
    fn local_control_serves_shared_overlay_contracts() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("stage");
        let upper = stage.join("upper");
        fs::create_dir_all(&upper).unwrap();
        fs::write(upper.join("new.txt"), b"new").unwrap();
        fs::write(upper.join(".wh.removed.txt"), b"").unwrap();
        let mut record = record(temp.path(), &stage, &upper);
        record.attempt_id = Some("attempt-test".into());
        record.write().unwrap();
        let _server = RunControlServer::start(&record).unwrap().unwrap();

        assert!(control_ping(&stage));
        let response = control_request(&stage, HostOverlayCommand::OverlayStatus {}).unwrap();
        let status: pvisor_core::overlay::OverlayStatus = response.overlay_status.unwrap();
        assert_eq!(status.changed_files, 1);
        assert_eq!(status.whiteouts, 1);
        assert!(status.sample_paths.contains(&"new.txt".to_string()));
        assert!(
            control_unmount_inspect(&stage, "missing".into())
                .unwrap_err()
                .to_string()
                .contains("unknown inspect session missing")
        );
    }

    #[test]
    fn local_control_waits_for_a_complete_request_on_a_nonblocking_connection() {
        use std::io::{BufRead, Write};
        use std::os::unix::net::UnixStream;
        use std::sync::mpsc;

        let temp = tempfile::tempdir().unwrap();
        let mut record = record(temp.path(), temp.path(), &temp.path().join("upper"));
        record.attempt_id = Some("attempt-test".into());
        let envelope = control_envelope(&record, HostOverlayCommand::Ping {}).unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = serde_json::to_vec(&envelope).unwrap();
        request.push(b'\n');
        let split = request.len() / 2;
        client.write_all(&request[..split]).unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                serve_control(
                    server,
                    ControlContext {
                        stage: temp.path(),
                        target: envelope.target.as_ref().unwrap(),
                        overlay: record.overlay.as_ref().unwrap(),
                        lowers: &[],
                        mounts: &mut HashMap::new(),
                        filesystem: None,
                        network: None,
                    },
                );
                done_tx.send(()).unwrap();
            });
            // An incomplete packet must neither be rejected nor close the connection.
            assert!(matches!(
                done_rx.recv_timeout(Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            client.write_all(&request[split..]).unwrap();
            let mut line = String::new();
            std::io::BufReader::new(&client)
                .read_line(&mut line)
                .unwrap();
            let response: AgentCtlHostResponse<HostOverlayResult> =
                serde_json::from_str(&line).unwrap();
            response.validate(&envelope.request_id).unwrap();
            response.result.unwrap();
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        });
    }

    #[test]
    fn lease_reports_live_only_while_held() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!is_live(temp.path()).unwrap());
        let lease = RunLease::acquire(temp.path()).unwrap();
        assert!(is_live(temp.path()).unwrap());
        drop(lease);
        assert!(!is_live(temp.path()).unwrap());
    }

    #[test]
    fn ownership_rereads_current_state_and_rejects_existing_or_invalid_storage() {
        let temp = tempfile::tempdir().unwrap();
        let mut current = record(temp.path(), temp.path(), &temp.path().join("upper"));
        current.write().unwrap();
        let selected = current.clone();
        current.state = RunRecordState::Failed;
        current.write().unwrap();
        let (locked, lease) = selected.lock_current().unwrap();
        assert_eq!(locked.state, RunRecordState::Failed);
        assert!(selected.lock_current().is_err());
        drop(lease);
        assert!(RunLease::acquire_new(temp.path()).is_err());

        let path = temp.path().join(RUN_META_FILENAME);
        let mut wire = serde_json::to_value(&current).unwrap();
        wire["state"] = "unknown".into();
        fs::write(&path, serde_json::to_vec(&wire).unwrap()).unwrap();
        assert!(RunRecord::read(temp.path()).is_err());
        wire["state"] = "completed".into();
        wire["schema_version"] = 99.into();
        fs::write(&path, serde_json::to_vec(&wire).unwrap()).unwrap();
        assert!(RunRecord::read(temp.path()).is_err());
        assert!(RunLease::acquire_new(temp.path()).is_err());
    }

    #[test]
    fn control_frames_require_a_newline_and_enforce_the_size_limit() {
        use pvisor_core::host_protocol::AGENTCTL_HOST_MAX_FRAME_BYTES;
        use std::io::{Read, Write};
        let temp = tempfile::tempdir().unwrap();
        let mut record = record(temp.path(), temp.path(), &temp.path().join("upper"));
        record.attempt_id = Some("attempt-test".into());
        let target = control_envelope(&record, HostOverlayCommand::Ping {})
            .unwrap()
            .target
            .unwrap();
        for variant in 0..3 {
            let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
            client
                .set_read_timeout(Some(CONTROL_TIMEOUT + Duration::from_secs(2)))
                .unwrap();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    serve_control(
                        server,
                        ControlContext {
                            stage: temp.path(),
                            target: &target,
                            overlay: record.overlay.as_ref().unwrap(),
                            lowers: &[],
                            mounts: &mut HashMap::new(),
                            filesystem: None,
                            network: None,
                        },
                    )
                });
                match variant {
                    0 => {
                        let request =
                            control_envelope(&record, HostOverlayCommand::Ping {}).unwrap();
                        client
                            .write_all(&serde_json::to_vec(&request).unwrap())
                            .unwrap();
                        client.shutdown(std::net::Shutdown::Write).unwrap();
                    }
                    1 => {
                        let _ = client.write_all(&vec![b'x'; AGENTCTL_HOST_MAX_FRAME_BYTES + 1]);
                    }
                    _ => {}
                }
                let started = std::time::Instant::now();
                let mut reply = Vec::new();
                let _ = client.read_to_end(&mut reply);
                assert!(reply.is_empty());
                assert!(started.elapsed() < CONTROL_TIMEOUT + Duration::from_secs(2));
            });
        }
    }

    #[test]
    fn host_control_rejects_stale_targets_versions_and_guest_tokens() {
        use std::io::{BufRead, Write};
        let temp = tempfile::tempdir().unwrap();
        let mut record = record(temp.path(), temp.path(), &temp.path().join("upper"));
        assert!(RunControlServer::start(&record).is_err());
        record.attempt_id = Some("attempt-test".into());
        record.write().unwrap();
        let _server = RunControlServer::start(&record).unwrap().unwrap();
        for variant in 0..10 {
            let request = control_envelope(&record, HostOverlayCommand::Ping {}).unwrap();
            let mut wire = serde_json::to_value(&request).unwrap();
            let expected = match variant {
                0 => {
                    wire["version"] = 99.into();
                    Some(AgentCtlHostErrorCode::VersionMismatch)
                }
                1 => {
                    wire["target"] = serde_json::Value::Null;
                    Some(AgentCtlHostErrorCode::Conflict)
                }
                2 => {
                    wire["target"]["job_id"] = "other-run".into();
                    Some(AgentCtlHostErrorCode::Conflict)
                }
                3 => {
                    wire["target"]["attempt_id"] = "old-attempt".into();
                    Some(AgentCtlHostErrorCode::Conflict)
                }
                4 => {
                    wire["target"]["attempt_id"] = serde_json::Value::Null;
                    Some(AgentCtlHostErrorCode::Conflict)
                }
                5 => {
                    wire["target"]["generation"] = "old".into();
                    Some(AgentCtlHostErrorCode::Conflict)
                }
                6 => {
                    wire["token"] = "guest-token".into();
                    None
                }
                7 => {
                    wire["command"]["token"] = "guest-token".into();
                    None
                }
                8 => {
                    wire["command"] = serde_json::json!({"op":"apply"});
                    None
                }
                _ => {
                    wire = serde_json::json!({"op":"ping"});
                    None
                }
            };
            let mut stream =
                std::os::unix::net::UnixStream::connect(temp.path().join(CONTROL_FILENAME))
                    .unwrap();
            stream.set_read_timeout(Some(CONTROL_TIMEOUT)).unwrap();
            let mut bytes = serde_json::to_vec(&wire).unwrap();
            bytes.push(b'\n');
            stream.write_all(&bytes).unwrap();
            let mut line = String::new();
            std::io::BufReader::new(&stream)
                .read_line(&mut line)
                .unwrap();
            if let Some(code) = expected {
                let response: AgentCtlHostResponse<HostOverlayResult> =
                    serde_json::from_str(&line).unwrap();
                response.validate(&request.request_id).unwrap();
                assert_eq!(response.result.unwrap_err().code, code);
            } else {
                assert!(line.is_empty());
            }
        }
        assert!(control_ping(temp.path()));
        assert_eq!(
            control_observations(temp.path()).unwrap(),
            serde_json::json!({"filesystem":null,"network":null})
        );
        // Discovery may now point at a replacement Attempt, but the old endpoint
        // remains bound to the identity captured at startup.
        record.attempt_id = Some("attempt-new".into());
        record.write().unwrap();
        assert!(!control_ping(temp.path()));
    }

    #[test]
    fn control_endpoint_uses_private_host_root_and_owned_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let mut record = record(temp.path(), temp.path(), &temp.path().join("upper"));
        record.attempt_id = Some("attempt-test".into());
        record.write().unwrap();
        let server = RunControlServer::start(&record).unwrap().unwrap();
        let locator = temp.path().join(CONTROL_FILENAME);
        let socket = fs::read_link(&locator).unwrap();
        let directory = socket.parent().unwrap().to_path_buf();
        let root = host_authority_root().unwrap();
        assert_eq!(directory.parent(), Some(root.as_path()));
        assert_eq!(
            root,
            fs::canonicalize("/tmp")
                .unwrap()
                .join(format!("pvisor-host-{}", unsafe { libc::geteuid() }))
        );
        for path in [&root, &directory] {
            let metadata = fs::symlink_metadata(path).unwrap();
            assert!(metadata.is_dir());
            assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
            assert_eq!(metadata.mode() & 0o7777, 0o700);
        }
        assert_eq!(
            fs::symlink_metadata(&socket).unwrap().mode() & 0o7777,
            0o600
        );
        assert_eq!(control_socket_path(temp.path()).unwrap(), socket);
        assert!(control_ping(temp.path()));
        // Discovery contains only a host path, not cooperative credentials.
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        let wire =
            serde_json::to_value(control_envelope(&record, HostOverlayCommand::Ping {}).unwrap())
                .unwrap();
        assert!(wire.get("token").is_none());
        assert!(wire["command"].get("token").is_none());
        assert!(RunControlServer::start(&record).is_err());
        assert_eq!(fs::read_link(&locator).unwrap(), socket);
        assert!(control_ping(temp.path()));
        drop(server);
        assert!(!locator.is_symlink());
        assert!(!directory.exists());
        assert!(root.is_dir());
    }

    #[test]
    fn control_discovery_denies_legacy_paths_without_deleting_them() {
        let temp = tempfile::tempdir().unwrap();
        let mut record = record(temp.path(), temp.path(), &temp.path().join("upper"));
        record.attempt_id = Some("attempt-test".into());
        record.write().unwrap();
        let legacy = tempfile::Builder::new()
            .prefix("pvisor-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = legacy.path().join(CONTROL_FILENAME);
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let locator = temp.path().join(CONTROL_FILENAME);
        std::os::unix::fs::symlink(&socket, &locator).unwrap();
        assert!(
            control_socket_path(temp.path())
                .unwrap_err()
                .to_string()
                .contains("host authority root")
        );
        assert!(!control_ping(temp.path()));
        assert!(RunControlServer::start(&record).is_err());
        assert_eq!(fs::read_link(&locator).unwrap(), socket);
        assert!(socket.exists());
        // A dangling legacy link is still user-owned and must not be replaced.
        fs::remove_file(&socket).unwrap();
        assert!(RunControlServer::start(&record).is_err());
        assert_eq!(fs::read_link(&locator).unwrap(), socket);
        fs::remove_file(&locator).unwrap();
        fs::write(&locator, b"user-owned").unwrap();
        assert!(RunControlServer::start(&record).is_err());
        assert_eq!(fs::read(&locator).unwrap(), b"user-owned");
        assert!(!control_ping(temp.path()));
    }

    #[test]
    fn control_discovery_rejects_public_and_symlinked_source_paths() {
        let temp = tempfile::tempdir().unwrap();
        let directory = allocate_host_directory("overlay-test-").unwrap();
        let socket = directory.path().join(CONTROL_FILENAME);
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let locator = temp.path().join(CONTROL_FILENAME);
        std::os::unix::fs::symlink(&socket, &locator).unwrap();
        assert_eq!(control_socket_path(temp.path()).unwrap(), socket);
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(control_socket_path(temp.path()).is_err());
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(control_socket_path(temp.path()).is_err());
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();

        let alias_owner = allocate_host_directory("overlay-test-").unwrap();
        let alias = alias_owner.path().join("alias");
        std::os::unix::fs::symlink(directory.path(), &alias).unwrap();
        fs::remove_file(&locator).unwrap();
        std::os::unix::fs::symlink(alias.join(CONTROL_FILENAME), &locator).unwrap();
        assert!(control_socket_path(temp.path()).is_err());
        fs::remove_file(&locator).unwrap();
        // A socket symlink inside the root must not redirect authority elsewhere.
        let redirected = alias_owner.path().join(CONTROL_FILENAME);
        std::os::unix::fs::symlink(&socket, &redirected).unwrap();
        std::os::unix::fs::symlink(&redirected, &locator).unwrap();
        assert!(control_socket_path(temp.path()).is_err());
        assert!(socket.exists());
    }

    #[test]
    fn control_teardown_preserves_replaced_discovery_and_sibling_endpoints() {
        let temp = tempfile::tempdir().unwrap();
        let mut record = record(temp.path(), temp.path(), &temp.path().join("upper"));
        record.attempt_id = Some("attempt-test".into());
        record.write().unwrap();
        let sibling = allocate_host_directory("overlay-test-").unwrap();
        let marker = sibling.path().join("user-owned");
        fs::write(&marker, b"keep").unwrap();
        for symlink in [false, true] {
            let server = RunControlServer::start(&record).unwrap().unwrap();
            let locator = temp.path().join(CONTROL_FILENAME);
            let socket = fs::read_link(&locator).unwrap();
            fs::remove_file(&locator).unwrap();
            if symlink {
                std::os::unix::fs::symlink(&marker, &locator).unwrap();
            } else {
                fs::write(&locator, b"replacement").unwrap();
            }
            drop(server);
            assert!(!socket.parent().unwrap().exists());
            assert_eq!(fs::read(&marker).unwrap(), b"keep");
            if symlink {
                assert_eq!(fs::read_link(&locator).unwrap(), marker);
            } else {
                assert_eq!(fs::read(&locator).unwrap(), b"replacement");
            }
            fs::remove_file(&locator).unwrap();
        }
    }

    #[test]
    fn control_client_rejects_uncorrelated_and_wrong_version_responses() {
        use std::io::{BufRead, Write};
        for variant in 0..3 {
            let temp = tempfile::tempdir().unwrap();
            let mut record = record(temp.path(), temp.path(), &temp.path().join("upper"));
            record.attempt_id = Some("attempt-test".into());
            record.write().unwrap();
            let directory = allocate_host_directory("overlay-test-").unwrap();
            let socket = directory.path().join(CONTROL_FILENAME);
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
            let _locator =
                ControlLocator::create(temp.path().join(CONTROL_FILENAME), &socket).unwrap();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut line = String::new();
                    std::io::BufReader::new(&stream)
                        .read_line(&mut line)
                        .unwrap();
                    let request: AgentCtlHostRequest<HostOverlayCommand> =
                        serde_json::from_str(&line).unwrap();
                    assert_eq!(
                        request.target.unwrap().attempt_id.as_deref(),
                        Some("attempt-test")
                    );
                    let response = AgentCtlHostResponse {
                        version: if variant == 0 {
                            99
                        } else {
                            AGENTCTL_HOST_VERSION
                        },
                        request_id: if variant == 1 {
                            "wrong".into()
                        } else {
                            request.request_id
                        },
                        result: if variant == 2 {
                            Err(AgentCtlHostError::new(
                                AgentCtlHostErrorCode::Unavailable,
                                "not available",
                            ))
                        } else {
                            Ok(HostOverlayResult::default())
                        },
                    };
                    let mut bytes = serde_json::to_vec(&response).unwrap();
                    bytes.push(b'\n');
                    stream.write_all(&bytes).unwrap();
                });
                assert!(control_request(temp.path(), HostOverlayCommand::Ping {}).is_err());
            });
        }
    }

    #[test]
    fn run_resolves_from_id_stage_and_upper() {
        let temp = tempfile::tempdir().unwrap();
        let storage = temp.path().join("store");
        let stage = storage.join(".overlay/session-test");
        let upper = stage.join("upper");
        fs::create_dir_all(&upper).unwrap();
        let record = record(&storage, &stage, &upper);
        record.write().unwrap();

        assert_eq!(
            resolve_run(Some(Path::new("run-test")), &storage)
                .unwrap()
                .run_id,
            "run-test"
        );
        assert_eq!(resolve_path(&stage).unwrap().run_id, "run-test");
        assert_eq!(resolve_path(&upper).unwrap().run_id, "run-test");
        assert_eq!(resolve_path(&storage).unwrap().run_id, "run-test");
        assert_eq!(resolve_last(&storage, None).unwrap().run_id, "run-test");
    }

    #[test]
    fn last_does_not_fall_back_to_a_run_from_another_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let storage = temp.path().join("store");
        fs::create_dir_all(&storage).unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        // No Run is registered for `workspace`; `last` must not silently return
        // a Run that belongs to some other workspace.
        let error = resolve_last(&storage, Some(&workspace))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("no pVisor Run found for the current workspace"),
            "{error}"
        );
    }
}
