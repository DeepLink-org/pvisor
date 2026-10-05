//! Durable Run identity, project association, and liveness metadata.

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
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

pub use pvisor_core::overlay::OverlayStatus as ControlOverlayStatus;
use pvisor_core::overlay::{RunControlRequest, RunControlResponse};
use pvisor_core::{ExecutorIdentity, ExecutorPlan, ResourceLimits};

pub const RUN_META_FILENAME: &str = "run.json";
pub const LEASE_FILENAME: &str = "lease.lock";
pub const CONTROL_FILENAME: &str = "control.sock";
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);
const CONTROL_MAX_FRAME_BYTES: usize = 1024 * 1024;

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
    pub(crate) fn require_stopped(&self) -> anyhow::Result<()> {
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
        let record: Self = serde_json::from_slice(&fs::read(&path)?)?;
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
    socket_path: PathBuf,
    locator_path: PathBuf,
    _socket_dir: tempfile::TempDir,
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
        let lowers = if record.overlay_lowers.is_empty() {
            vec![overlay.target.clone()]
        } else {
            record.overlay_lowers.clone()
        };
        let locator_path = record.stage_dir().join(CONTROL_FILENAME);
        if locator_path.exists() || locator_path.is_symlink() {
            fs::remove_file(&locator_path)?;
        }
        // macOS sockaddr_un paths are short. Bind in the fixed, short `/tmp`
        // directory rather than `std::env::temp_dir()` (which can point at a deep
        // per-user path) and expose a stable stage-local symlink for discovery.
        let socket_dir = tempfile::Builder::new()
            .prefix("pvisor-")
            .tempdir_in("/tmp")?;
        let socket_path = socket_dir.path().join("control.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        std::os::unix::fs::symlink(&socket_path, &locator_path)?;
        listener.set_nonblocking(true)?;
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
                                &stage,
                                &overlay,
                                &lowers,
                                &mut mounts,
                                filesystem.as_ref(),
                                network.as_ref(),
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
            socket_path,
            locator_path,
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
        let _ = fs::remove_file(&self.socket_path);
        let _ = fs::remove_file(&self.locator_path);
    }
}

fn serve_control(
    mut stream: std::os::unix::net::UnixStream,
    stage: &Path,
    overlay: &OverlayRecord,
    lowers: &[PathBuf],
    mounts: &mut HashMap<String, ReadOnlyOverlayMount>,
    filesystem: Option<&pvisor_overlayfs::FsMetrics>,
    network: Option<&pvisor_overlaynet::InterceptionMetrics>,
) {
    use std::io::Write;
    let request = (|| -> anyhow::Result<RunControlRequest> {
        // macOS accept inherits O_NONBLOCK from the listener. The line-based
        // protocol must wait for the complete request, including its newline.
        stream.set_nonblocking(false)?;
        stream.set_write_timeout(Some(CONTROL_TIMEOUT))?;
        Ok(serde_json::from_slice(&read_control_frame(&mut stream)?)?)
    })();
    let response = match request {
        Ok(RunControlRequest::Ping) => RunControlResponse {
            ok: true,
            id: None,
            mountpoint: None,
            error: None,
            overlay_status: None,
            observations: None,
        },
        Ok(RunControlRequest::OverlayStatus) => match overlay_status(overlay) {
            Ok(status) => RunControlResponse {
                ok: true,
                id: None,
                mountpoint: None,
                error: None,
                overlay_status: Some(status),
                observations: None,
            },
            Err(error) => control_error(error),
        },
        Ok(RunControlRequest::Observations) => RunControlResponse {
            ok: true,
            id: None,
            mountpoint: None,
            error: None,
            overlay_status: None,
            observations: Some(serde_json::json!({
                "filesystem": filesystem.map(|metrics| metrics.snapshot()),
                "network": network.map(|metrics| metrics.snapshot()),
            })),
        },
        Ok(RunControlRequest::MountInspect) => {
            let id = uuid::Uuid::new_v4().to_string();
            let mountpoint = stage.join("inspect").join(&id).join("merged");
            match mount_overlay_record_read_only(overlay, lowers, &mountpoint) {
                Ok(mount) => {
                    let mountpoint = mount.mountpoint().to_path_buf();
                    mounts.insert(id.clone(), mount);
                    RunControlResponse {
                        ok: true,
                        id: Some(id),
                        mountpoint: Some(mountpoint),
                        error: None,
                        overlay_status: None,
                        observations: None,
                    }
                }
                Err(error) => control_error(error),
            }
        }
        Ok(RunControlRequest::UnmountInspect { id }) => {
            if let Some(mount) = mounts.remove(&id) {
                match mount.unmount() {
                    Ok(()) => RunControlResponse {
                        ok: true,
                        id: None,
                        mountpoint: None,
                        error: None,
                        overlay_status: None,
                        observations: None,
                    },
                    Err(error) => control_error(error),
                }
            } else {
                control_error(anyhow::anyhow!("unknown inspect session {id}"))
            }
        }
        Err(error) => control_error(error),
    };
    if let Ok(mut body) = serde_json::to_vec(&response) {
        body.push(b'\n');
        let _ = stream.write_all(&body);
    }
}

fn control_error(error: impl std::fmt::Display) -> RunControlResponse {
    RunControlResponse {
        ok: false,
        id: None,
        mountpoint: None,
        error: Some(error.to_string()),
        overlay_status: None,
        observations: None,
    }
}

fn control_request(
    stage: &Path,
    request: &RunControlRequest,
) -> anyhow::Result<RunControlResponse> {
    use std::io::Write;
    let mut stream = std::os::unix::net::UnixStream::connect(stage.join(CONTROL_FILENAME))?;
    stream.set_write_timeout(Some(CONTROL_TIMEOUT))?;
    let mut body = serde_json::to_vec(request)?;
    anyhow::ensure!(
        body.len() <= CONTROL_MAX_FRAME_BYTES,
        "control frame too large"
    );
    body.push(b'\n');
    stream.write_all(&body)?;
    let response: RunControlResponse = serde_json::from_slice(&read_control_frame(&mut stream)?)?;
    if !response.ok {
        anyhow::bail!(
            "pVisor control request failed: {}",
            response.error.as_deref().unwrap_or("unknown error")
        );
    }
    Ok(response)
}

// One connection carries one frame. A total deadline also bounds slow trickle
// clients; a timeout on each individual read would not bound teardown latency.
fn read_control_frame(stream: &mut std::os::unix::net::UnixStream) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;
    let deadline = std::time::Instant::now() + CONTROL_TIMEOUT;
    let mut frame = Vec::new();
    loop {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|duration| !duration.is_zero())
            .context("control frame deadline exceeded")?;
        stream.set_read_timeout(Some(remaining))?;
        let mut chunk = [0; 1024];
        let count = stream.read(&mut chunk)?;
        anyhow::ensure!(count != 0, "control frame ended before newline");
        let newline = chunk[..count].iter().position(|byte| *byte == b'\n');
        let length = newline.unwrap_or(count);
        anyhow::ensure!(
            frame.len() + length <= CONTROL_MAX_FRAME_BYTES,
            "control frame too large"
        );
        frame.extend_from_slice(&chunk[..length]);
        if newline.is_some() {
            return Ok(frame);
        }
    }
}

pub fn control_ping(stage: &Path) -> bool {
    control_request(stage, &RunControlRequest::Ping).is_ok()
}

pub fn control_mount_inspect(stage: &Path) -> anyhow::Result<(String, PathBuf)> {
    let response = control_request(stage, &RunControlRequest::MountInspect)?;
    Ok((
        response.id.context("control response missing inspect id")?,
        response
            .mountpoint
            .context("control response missing inspect mountpoint")?,
    ))
}

pub fn control_overlay_status(stage: &Path) -> anyhow::Result<ControlOverlayStatus> {
    control_request(stage, &RunControlRequest::OverlayStatus)?
        .overlay_status
        .context("control response missing OverlayFS status")
}

pub fn control_observations(stage: &Path) -> anyhow::Result<serde_json::Value> {
    control_request(stage, &RunControlRequest::Observations)?
        .observations
        .context("control response missing observations")
}

pub fn control_unmount_inspect(stage: &Path, id: String) -> anyhow::Result<()> {
    control_request(stage, &RunControlRequest::UnmountInspect { id })?;
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
        let record = record(temp.path(), &stage, &upper);
        let _server = RunControlServer::start(&record).unwrap().unwrap();

        assert!(control_ping(&stage));
        let response = control_request(&stage, &RunControlRequest::OverlayStatus).unwrap();
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
        let record = record(temp.path(), temp.path(), &temp.path().join("upper"));
        let (mut client, server) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = serde_json::to_vec(&RunControlRequest::Ping).unwrap();
        request.push(b'\n');
        let split = request.len() / 2;
        client.write_all(&request[..split]).unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                serve_control(
                    server,
                    temp.path(),
                    record.overlay.as_ref().unwrap(),
                    &[],
                    &mut HashMap::new(),
                    None,
                    None,
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
            let response: RunControlResponse = serde_json::from_str(&line).unwrap();
            assert!(response.ok, "{:?}", response.error);
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
        use std::io::Write;
        let (mut reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        writer.write_all(b"{\"type\":\"ping\"}\n").unwrap();
        assert_eq!(
            read_control_frame(&mut reader).unwrap(),
            b"{\"type\":\"ping\"}"
        );
        drop(writer);
        assert!(read_control_frame(&mut reader).is_err());

        let (mut reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let sending = std::thread::spawn(move || {
            let _ = writer.write_all(&vec![b'x'; CONTROL_MAX_FRAME_BYTES + 1]);
        });
        assert!(read_control_frame(&mut reader).is_err());
        drop(reader);
        sending.join().unwrap();

        let (mut reader, _idle_client) = std::os::unix::net::UnixStream::pair().unwrap();
        let started = std::time::Instant::now();
        assert!(read_control_frame(&mut reader).is_err());
        assert!(started.elapsed() < CONTROL_TIMEOUT + Duration::from_secs(2));
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
