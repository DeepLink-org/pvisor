//! Durable Run identity, project association, and liveness metadata.
use super::execution::RunRecordState;

use super::overlay::{OverlayRecord, load_overlay_record};
use anyhow::Context;
use pvisor_journal::api::{DurableFiles, Persistence};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub use pvisor_core::overlay::OverlayStatus as ControlOverlayStatus;
use pvisor_core::{ExecutorIdentity, ExecutorPlan, ResourceLimits};

pub const RUN_META_FILENAME: &str = "run.json";
pub const LEASE_FILENAME: &str = "lease.lock";
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub schema_version: u32,
    /// Completion protocol required by this Attempt. Legacy records omit it.
    /// Protocol 1 requires a result/Bundle receipt before a terminal state is trusted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_protocol: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_control: Option<super::host_transport::LiveControlRecord>,
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
    /// Explicit launch-time project writeback authority, bound to the completion Bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_apply_scope: Option<super::job_service::ProjectApplyScope>,
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

/// Exclusive caller-side ownership of a durable Job operation. The root's
/// `job.lock` is never unlinked, so concurrent binaries lock the same inode.
/// Mutation ownership clears its diagnostic record on drop while holding flock.
/// Observation only releases flock by closing its read-only descriptor.
pub struct JobDirectoryLock {
    file: File,
    clear_owner: bool,
}

#[derive(Serialize, Deserialize)]
struct JobLockOwner {
    protocol: String,
    pid: u32,
    operation: String,
}

impl JobDirectoryLock {
    /// Lock a recognized Job without following a lock symlink or accepting hard
    /// links/foreign ownership. Refuses a live owner, busy lock, unknown schema,
    /// or residual diagnostic owner whose process has not died. No retries.
    pub fn acquire(record: &RunRecord, operation: &str) -> anyhow::Result<Self> {
        Self::lock(record, operation, false)
    }

    /// Exclusively observe an existing Job lock without creating it, updating its
    /// owner record, syncing or truncating it on release. Missing locks fail closed.
    /// Uses the same inode, validation and lock order as mutation ownership.
    pub fn observe_existing(record: &RunRecord) -> anyhow::Result<Self> {
        Self::lock(record, "apply-preview", true)
    }

    fn lock(record: &RunRecord, operation: &str, observe: bool) -> anyhow::Result<Self> {
        use std::io::{Read, Seek, Write};
        use std::os::unix::fs::OpenOptionsExt;
        const PROTOCOL: &str = "pvisor-job-lock-1";
        let current = RunRecord::read(&record.stage_dir())?;
        super::job_service::check_selected_record(record, &current)?;
        anyhow::ensure!(
            !current
                .live_control
                .as_ref()
                .is_some_and(|endpoint| endpoint.is_available()),
            "Job owner still holds stage; operation {operation} refused"
        );
        let root = crate::JobExecution::read(&current)?
            .map(|job| job.root)
            .unwrap_or_else(|| current.stage_dir());
        let path = root.join("job.lock");
        let mut file = OpenOptions::new()
            .read(true)
            .write(!observe)
            .create(!observe)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&path)
            .with_context(|| {
                format!(
                    "open existing Job operation lock {} (preview never initializes locks)",
                    path.display()
                )
            })?;
        let metadata = file.metadata()?;
        anyhow::ensure!(
            metadata.is_file()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o7777 == 0o600
                && metadata.nlink() == 1,
            "job.lock must be an owned 0600 file with one link"
        );
        anyhow::ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "Job operation already in progress: {}",
            std::io::Error::last_os_error()
        );
        let checked = fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            checked.dev() == metadata.dev() && checked.ino() == metadata.ino(),
            "job.lock changed while locking"
        );
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(4097)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= 4096, "invalid job.lock owner record");
        if !bytes.is_empty() {
            let owner: JobLockOwner = serde_json::from_slice(&bytes)?;
            anyhow::ensure!(owner.protocol == PROTOCOL, "unknown job.lock protocol");
            anyhow::ensure!(
                owner.pid > 0
                    && owner.pid <= i32::MAX as u32
                    && unsafe { libc::kill(owner.pid as i32, 0) } < 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
                "residual Job lock owner {} may still be alive",
                owner.pid
            );
        }
        let latest = RunRecord::read(&record.stage_dir())?;
        super::job_service::check_selected_record(record, &latest)?;
        anyhow::ensure!(
            !latest
                .live_control
                .as_ref()
                .is_some_and(|endpoint| endpoint.is_available()),
            "Job owner still holds stage"
        );
        if !observe {
            file.rewind()?;
            file.set_len(0)?;
            file.write_all(&serde_json::to_vec(&JobLockOwner {
                protocol: PROTOCOL.into(),
                pid: std::process::id(),
                operation: operation.into(),
            })?)?;
            file.sync_all()?;
        }
        Ok(Self {
            file,
            clear_owner: !observe,
        })
    }
}
impl Drop for JobDirectoryLock {
    fn drop(&mut self) {
        if self.clear_owner {
            let _ = self.file.set_len(0);
            let _ = self.file.sync_all();
        }
    }
}

impl RunRecord {
    pub(super) fn validate_schema(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(
                (self.schema_version, self.completion_protocol),
                (1, None) | (2, Some(super::completion::VERSION))
            ),
            "unsupported Run record schema {} / completion protocol {:?}",
            self.schema_version,
            self.completion_protocol
        );
        Ok(())
    }
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
        self.lock_current_mode(false)
    }
    /// Observe the current Attempt under its existing exclusive lease, without
    /// creating directories or lease files. Completion validation remains mandatory.
    pub fn observe_current(&self) -> anyhow::Result<(Self, RunLease)> {
        self.lock_current_mode(true)
    }
    fn lock_current_mode(&self, observe: bool) -> anyhow::Result<(Self, RunLease)> {
        let stage = self.stage_dir();
        let lease = if observe {
            RunLease::observe_existing(&stage)?
        } else {
            RunLease::acquire(&stage)?
        };
        let mut current = Self::read(&stage)?;
        // Our own exclusive lease makes is_live true. It must not suppress the
        // completion gate for a stopped Attempt about to be reviewed or mutated.
        super::completion::Completion::observe_record(&mut current)?;
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

    /// Initialize preview lock inodes during explicit project-scope launch only.
    /// The caller owns the new Attempt lease; previews must never call this method.
    pub(crate) fn initialize_apply_observation_locks(&self) -> anyhow::Result<()> {
        if self.project_apply_scope.is_none() {
            return Ok(());
        }
        use std::os::unix::fs::OpenOptionsExt;
        let stage = self.stage_dir();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(stage.join("job.lock"))?;
        let metadata = file.metadata()?;
        anyhow::ensure!(
            metadata.is_file()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o7777 == 0o600
                && metadata.nlink() == 1,
            "job.lock must be an owned 0600 file with one link"
        );
        file.sync_all()?;
        // All native ExecutionJob mutators use this same RunLease inode. Initialize
        // it before Job publication, not on the first read-only command.
        drop(RunLease::acquire(&stage.join("execution-operation"))?);
        Persistence::sync_directory(&stage)?;
        Ok(())
    }

    pub fn write(&self) -> anyhow::Result<()> {
        self.validate_schema()?;
        let stage = self.stage_dir();
        let path = stage.join(RUN_META_FILENAME);
        crate::util::write_run_json(&path, self, &self.run_id, "run_record")?;

        let index_path = run_index_path(&self.storage, &self.run_id);
        let index_dir = index_path.parent().expect("Run index has a parent");
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
                Persistence::sync_directory(index_dir)
            })?;
        } else {
            crate::util::write_run_bytes(&index_path, &contents, &self.run_id, "run_index")?;
        }
        Ok(())
    }

    pub fn read(stage: &Path) -> anyhow::Result<Self> {
        // A Job's original stage remains a stable selector across Attempts.
        // The active Attempt owns its own run.json, Bundle and storage lease.
        let active = crate::JobExecution::read_stage(stage)?;
        let stage = active.as_ref().map_or(stage, |job| {
            if job.state() == crate::JobState::Restoring
                && !job.active_stage.join(RUN_META_FILENAME).exists()
            {
                job.previous_stage.as_path()
            } else {
                job.active_stage.as_path()
            }
        });
        let path = stage.join(RUN_META_FILENAME);
        let mut record: Self = serde_json::from_slice(&fs::read(&path)?)?;
        record.validate_schema()?;
        if let Some(job) = &active {
            anyhow::ensure!(
                record.run_id == job.run_id,
                "execution Job record owner mismatch"
            );
        }
        // A writer may have published the terminal projection immediately before
        // committing its receipt. A live lease is not proof of that completion.
        if record.completion_protocol.is_some() && (record.state.is_stopped() || !is_live(stage)?) {
            super::completion::Completion::observe_record(&mut record)?;
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
        let path = run_index_path(&self.storage, &self.run_id);
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
    /// Exclusively lock an already initialized lease using a read-only descriptor.
    /// Never mkdir/create/truncate/sync. Validates inode ownership, mode and binding;
    /// missing or unsafe leases fail closed rather than initializing a preview.
    pub fn observe_existing(stage_dir: &Path) -> anyhow::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = stage_dir.join(LEASE_FILENAME);
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&path)
            .with_context(|| {
                format!(
                    "open existing observation lease {} (preview never initializes locks)",
                    path.display()
                )
            })?;
        let metadata = file.metadata()?;
        anyhow::ensure!(
            metadata.is_file()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o022 == 0
                && metadata.nlink() == 1,
            "observation lease must be an owned regular file without shared write access or hard links"
        );
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("acquire existing observation lease");
        }
        let checked = fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            checked.dev() == metadata.dev() && checked.ino() == metadata.ino(),
            "observation lease changed while locking"
        );
        Ok(Self { file })
    }

    pub fn acquire(stage_dir: &Path) -> anyhow::Result<Self> {
        Persistence::create_dir_all_durable(stage_dir)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(stage_dir.join(LEASE_FILENAME))?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("acquire Run storage lease: {}", stage_dir.display()));
        }
        Ok(Self { file })
    }
}

impl Drop for RunLease {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn control_request(
    stage: &Path,
    command: super::host_transport::InstanceCommand,
) -> anyhow::Result<serde_json::Value> {
    let record = RunRecord::read(stage)?;
    if let Some(owner) = super::instance_control::local_owner(&record) {
        return Ok(owner.workspace(command)?);
    }
    let endpoint = record
        .live_control
        .as_ref()
        .context("owning Job control endpoint unavailable")?;
    endpoint.exchange(
        &record.run_id,
        record
            .attempt_id
            .as_deref()
            .context("control requires an Attempt identity")?,
        command,
    )
}

pub fn control_ping(stage: &Path) -> bool {
    control_request(stage, super::host_transport::InstanceCommand::Ping {}).is_ok()
}

pub fn control_mount_inspect(stage: &Path) -> anyhow::Result<(String, PathBuf)> {
    let value = control_request(
        stage,
        super::host_transport::InstanceCommand::Inspect { checkpoint: None },
    )?;
    Ok((
        value["id"]
            .as_str()
            .context("control response missing inspect id")?
            .into(),
        serde_json::from_value(value["mountpoint"].clone())?,
    ))
}

pub fn control_overlay_status(stage: &Path) -> anyhow::Result<ControlOverlayStatus> {
    Ok(serde_json::from_value(control_request(
        stage,
        super::host_transport::InstanceCommand::OverlayStatus {},
    )?)?)
}

pub fn control_observations(stage: &Path) -> anyhow::Result<serde_json::Value> {
    control_request(
        stage,
        super::host_transport::InstanceCommand::Observations {},
    )
}

pub(crate) fn control_status_observations(
    stage: &Path,
) -> anyhow::Result<super::job_service::status::Activity> {
    Ok(serde_json::from_value(control_request(
        stage,
        super::host_transport::InstanceCommand::StatusObservations {},
    )?)?)
}

pub fn control_unmount_inspect(stage: &Path, id: String) -> anyhow::Result<()> {
    control_request(
        stage,
        super::host_transport::InstanceCommand::UnmountInspect { id },
    )?;
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

/// Failure to establish a unique Job identity. Never permits container fallback.
///
/// Includes corrupt records/indexes, conflicting identities, ambiguity and I/O
/// failures. The source retains diagnostic context; no discovery state is changed.
#[derive(Debug)]
pub struct RunLookupError(anyhow::Error);

impl std::fmt::Display for RunLookupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot safely resolve Job identity: {:#}", self.0)
    }
}

impl std::error::Error for RunLookupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

/// Strictly discover an exact Job ID, without interpreting paths or `last`.
///
/// Searches the selected storage and both default roots (including storage
/// indexes beneath those roots). `Ok(None)` alone establishes absence and
/// permits a caller to try another namespace. All discoverable records must be
/// readable; index and record owners must agree. Multiple canonical stages for
/// one ID are errors, even if selected storage contains a matching index.
/// This read-only lookup neither locks nor reserves the result against changes.
pub fn lookup_run_id(id: &Path, storage: &Path) -> Result<Option<RunRecord>, RunLookupError> {
    let roots = default_run_roots();
    lookup_run_id_in(id, storage, &roots).map_err(RunLookupError)
}

fn default_run_roots() -> Vec<PathBuf> {
    let mut roots = vec![default_run_home(), std::env::temp_dir().join("pvisor-runs")];
    roots.sort();
    roots.dedup();
    roots
}

fn run_index_path(storage: &Path, id: &str) -> PathBuf {
    storage
        .join(".pvisor/runs")
        .join(format!("{}.json", crate::util::encode_hex(id.as_bytes())))
}

fn discovery_present(path: &Path) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn lookup_run_id_in(
    id: &Path,
    storage: &Path,
    roots: &[PathBuf],
) -> anyhow::Result<Option<RunRecord>> {
    let id = id.to_str().context("Job ID must be UTF-8")?;
    anyhow::ensure!(
        !id.is_empty() && Path::new(id).file_name().and_then(|name| name.to_str()) == Some(id),
        "expected an exact Job ID"
    );
    let mut stores = std::collections::BTreeSet::from([storage.to_path_buf()]);
    for root in roots {
        if !discovery_present(root)? {
            continue;
        }
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                stores.insert(entry.path());
            }
        }
    }
    let mut matches = std::collections::BTreeMap::new();
    for store in stores {
        let metadata = store.join(RUN_META_FILENAME);
        if discovery_present(&metadata)? {
            let record =
                RunRecord::read(&store).with_context(|| format!("read Job {}", store.display()))?;
            if record.run_id == id {
                matches.insert(record.stage_dir().canonicalize()?, record);
            }
        }
        let path = run_index_path(&store, id);
        if discovery_present(&path)? {
            let index: RunIndex = serde_json::from_slice(&fs::read(&path)?)
                .with_context(|| format!("read Job index {}", path.display()))?;
            anyhow::ensure!(index.run_id == id, "Job index owner mismatch");
            let record = RunRecord::read(&index.stage_dir)?;
            anyhow::ensure!(record.run_id == id, "Job record owner mismatch");
            matches.insert(record.stage_dir().canonicalize()?, record);
        }
    }
    anyhow::ensure!(matches.len() <= 1, "ambiguous existing Job selector: {id}");
    Ok(matches.into_values().next())
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
        let index = run_index_path(storage, &id);
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
    for root in default_run_roots() {
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
    use crate::host_transport::allocate_host_directory;
    use crate::runtime::OverlayUpper;

    fn record(storage: &Path, stage: &Path, upper: &Path) -> RunRecord {
        RunRecord {
            attempt_id: None,
            schema_version: 1,
            completion_protocol: None,
            live_control: None,
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
            project_apply_scope: None,
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
    fn stale_control_with_a_reused_live_pid_reports_disk_state() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("stage");
        fs::create_dir_all(stage.join("upper")).unwrap();
        fs::create_dir_all(temp.path().join("target")).unwrap();
        let private = allocate_host_directory("stale-control-").unwrap();
        let path = private.path().join("control.sock");
        let listener = super::super::host_transport::bind_instance_socket(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();
        drop(listener);
        for pid in [std::process::id(), unsafe { libc::getppid() } as u32] {
            let mut run = record(temp.path(), &stage, &stage.join("upper"));
            run.state = RunRecordState::Running;
            run.finished_at_unix_ms = None;
            run.pid = pid;
            run.attempt_id = Some("old-attempt".into());
            run.live_control = Some(super::super::host_transport::LiveControlRecord {
                path: path.clone(),
                device: metadata.dev(),
                inode: metadata.ino(),
                pid,
                protocol: super::super::host_transport::INSTANCE_CONTROL_PROTOCOL.into(),
                binary: PathBuf::from("/old/pvisor"),
                binary_digest: "old-owner".into(),
            });
            run.write().unwrap();
            let status = super::super::job_service::RuntimeJobService::status(
                &Default::default(),
                super::super::job_service::StatusRequest {
                    job: super::super::job_service::JobSelection {
                        selector: Some(stage.clone()),
                        storage: temp.path().to_owned(),
                    },
                },
            )
            .unwrap();
            assert!(!status.live);
            assert!(status.live_attempt.is_none());
            assert_eq!(status.record.state, RunRecordState::Running);
        }
    }

    #[test]
    fn job_directory_lock_serializes_and_rejects_unsafe_or_unknown_records() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("stage");
        let run = record(temp.path(), &stage, &stage.join("upper"));
        run.write().unwrap();
        let path = stage.join("job.lock");
        let held = JobDirectoryLock::acquire(&run, "apply").unwrap();
        let owner: JobLockOwner = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(owner.operation, "apply");
        assert!(JobDirectoryLock::acquire(&run, "drop").is_err());
        drop(held);
        assert!(fs::read(&path).unwrap().is_empty());
        drop(JobDirectoryLock::acquire(&run, "review").unwrap());
        fs::hard_link(&path, stage.join("alias.lock")).unwrap();
        assert!(JobDirectoryLock::acquire(&run, "apply").is_err());
        fs::remove_file(stage.join("alias.lock")).unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(stage.join("run.json"), &path).unwrap();
        assert!(JobDirectoryLock::acquire(&run, "drop").is_err());
        fs::remove_file(&path).unwrap();
        let mut unknown = run.clone();
        unknown.schema_version = 99;
        fs::write(
            stage.join(RUN_META_FILENAME),
            serde_json::to_vec(&unknown).unwrap(),
        )
        .unwrap();
        assert!(JobDirectoryLock::acquire(&run, "apply").is_err());
    }

    #[test]
    fn observation_preserves_residual_owner_and_lease_identity_without_initialization() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("stage");
        let run = record(temp.path(), &stage, &stage.join("upper"));
        run.write().unwrap();
        assert!(JobDirectoryLock::observe_existing(&run).is_err());
        assert!(!stage.join("job.lock").exists());
        assert!(RunLease::observe_existing(&stage.join("missing")).is_err());
        assert!(!stage.join("missing").exists());
        drop(JobDirectoryLock::acquire(&run, "seed").unwrap());
        drop(RunLease::acquire(&stage).unwrap());
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let dead_pid = child.id();
        child.wait().unwrap();
        let path = stage.join("job.lock");
        fs::write(
            &path,
            serde_json::to_vec(&JobLockOwner {
                protocol: "pvisor-job-lock-1".into(),
                pid: dead_pid,
                operation: "interrupted".into(),
            })
            .unwrap(),
        )
        .unwrap();
        fn snapshot(path: &Path) -> (Vec<u8>, u64, u64, i64, i64, i64, i64) {
            let m = fs::symlink_metadata(path).unwrap();
            (
                fs::read(path).unwrap(),
                m.dev(),
                m.ino(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        }
        let lease_path = stage.join(LEASE_FILENAME);
        let owner_before = snapshot(&path);
        let lease_before = snapshot(&lease_path);
        {
            let _owner = JobDirectoryLock::observe_existing(&run).unwrap();
            let _lease = RunLease::observe_existing(&stage).unwrap();
            assert!(JobDirectoryLock::acquire(&run, "contender").is_err());
            assert!(RunLease::acquire(&stage).is_err());
        }
        assert_eq!(owner_before, snapshot(&path));
        assert_eq!(lease_before, snapshot(&lease_path));
        // Observation retains legacy owner validation, without clearing bad bytes.
        fs::write(&path, b"unknown-owner").unwrap();
        assert!(JobDirectoryLock::observe_existing(&run).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"unknown-owner");
        fs::remove_file(&lease_path).unwrap();
        std::os::unix::fs::symlink(&path, &lease_path).unwrap();
        assert!(RunLease::observe_existing(&stage).is_err());
        fs::remove_file(&lease_path).unwrap();
        fs::hard_link(&path, &lease_path).unwrap();
        assert!(RunLease::observe_existing(&stage).is_err());
    }

    #[test]
    fn residual_job_lock_is_cleared_only_after_its_process_dies() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("stage");
        let run = record(temp.path(), &stage, &stage.join("upper"));
        run.write().unwrap();
        let path = stage.join("job.lock");
        drop(JobDirectoryLock::acquire(&run, "seed").unwrap());
        let mut owner = JobLockOwner {
            protocol: "pvisor-job-lock-1".into(),
            pid: std::process::id(),
            operation: "apply".into(),
        };
        fs::write(&path, serde_json::to_vec(&owner).unwrap()).unwrap();
        assert!(JobDirectoryLock::acquire(&run, "drop").is_err());
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        owner.pid = child.id();
        child.wait().unwrap();
        fs::write(&path, serde_json::to_vec(&owner).unwrap()).unwrap();
        drop(JobDirectoryLock::acquire(&run, "drop").unwrap());
        owner.protocol = "future-lock".into();
        fs::write(&path, serde_json::to_vec(&owner).unwrap()).unwrap();
        assert!(JobDirectoryLock::acquire(&run, "drop").is_err());
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

    #[tokio::test]
    async fn local_control_serves_shared_overlay_contracts_without_a_socket() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("stage");
        let mut spec = pvisor_core::RunSpec::process("local-overlay", "test", "/bin/sleep");
        let pvisor_core::RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["60".into()];
        let handle = crate::PVisor::builder()
            .storage(&stage)
            .instance_control(false)
            .network(crate::NetworkDriverConfig {
                mode: crate::OverlayNetMode::Off,
                ..Default::default()
            })
            .build()
            .run(spec)
            .await
            .unwrap();
        let upper = stage.join("upper");
        fs::create_dir_all(&upper).unwrap();
        fs::write(upper.join("new.txt"), b"new").unwrap();
        fs::write(upper.join(".wh.removed.txt"), b"").unwrap();
        let mut selected = RunRecord::read(&stage).unwrap();
        selected.overlay = record(temp.path(), &stage, &upper).overlay;
        selected.write().unwrap();
        assert!(selected.live_control.is_none());
        assert!(!stage.join("control.sock").exists());
        assert!(control_ping(&stage));
        let status = control_overlay_status(&stage).unwrap();
        assert_eq!(status.changed_files, 1);
        assert_eq!(status.whiteouts, 1);
        assert!(status.sample_paths.contains(&"new.txt".to_string()));
        assert!(
            control_unmount_inspect(&stage, "missing".into())
                .unwrap_err()
                .to_string()
                .contains("unknown inspect session missing")
        );
        handle.execution().cancel();
        handle.wait().await.unwrap();
        assert!(!control_ping(&stage));
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
        wire["state"] = "unrecognized-state".into();
        fs::write(&path, serde_json::to_vec(&wire).unwrap()).unwrap();
        assert!(RunRecord::read(temp.path()).is_err());
        wire["state"] = "completed".into();
        wire["schema_version"] = 99.into();
        fs::write(&path, serde_json::to_vec(&wire).unwrap()).unwrap();
        assert!(RunRecord::read(temp.path()).is_err());
        assert!(RunLease::acquire_new(temp.path()).is_err());
    }

    #[test]
    fn strict_job_lookup_rejects_path_inputs_and_unreadable_discovery_roots() {
        let temp = tempfile::tempdir().unwrap();
        for id in [
            "",
            ".",
            "..",
            "/",
            "/run-test",
            "run-test/",
            "nested/run-test",
        ] {
            assert!(
                lookup_run_id_in(Path::new(id), temp.path(), &[]).is_err(),
                "{id}"
            );
        }
        let root = temp.path().join("not-a-directory");
        fs::write(&root, b"not a discovery directory").unwrap();
        assert!(lookup_run_id_in(Path::new("absent"), temp.path(), &[root]).is_err());
    }

    #[test]
    fn strict_job_lookup_distinguishes_absence_corruption_and_collisions() {
        let temp = tempfile::tempdir().unwrap();
        let storage = temp.path().join("storage");
        let root = temp.path().join("default");
        let roots = [root.clone(), root.clone()];
        let id = Path::new("run-test");
        assert!(lookup_run_id_in(id, &storage, &roots).unwrap().is_none());

        let stage = storage.join("stage");
        fs::create_dir_all(stage.join("upper")).unwrap();
        record(&storage, &stage, &stage.join("upper"))
            .write()
            .unwrap();
        assert_eq!(
            lookup_run_id_in(id, &storage, &roots)
                .unwrap()
                .unwrap()
                .stage_dir(),
            stage
        );
        let other = root.join("other");
        fs::create_dir_all(other.join("upper")).unwrap();
        record(&other, &other, &other.join("upper"))
            .write()
            .unwrap();
        assert!(lookup_run_id_in(id, &storage, &roots).is_err());
        fs::write(other.join(RUN_META_FILENAME), b"corrupt").unwrap();
        assert!(lookup_run_id_in(Path::new("absent"), &storage, &roots).is_err());
    }

    #[test]
    fn strict_job_lookup_rejects_corrupt_dangling_and_wrong_owner_indexes() {
        let temp = tempfile::tempdir().unwrap();
        let storage = temp.path().join("storage");
        let path = run_index_path(&storage, "run-test");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let id = Path::new("run-test");
        fs::write(&path, b"corrupt").unwrap();
        assert!(lookup_run_id_in(id, &storage, &[]).is_err());
        let index = RunIndex {
            run_id: "run-test".into(),
            stage_dir: temp.path().join("missing"),
        };
        fs::write(&path, serde_json::to_vec(&index).unwrap()).unwrap();
        assert!(lookup_run_id_in(id, &storage, &[]).is_err());
        let stage = temp.path().join("stage");
        fs::create_dir_all(stage.join("upper")).unwrap();
        let mut other = record(&storage, &stage, &stage.join("upper"));
        other.run_id = "other".into();
        other.write().unwrap();
        let index = RunIndex {
            stage_dir: stage,
            ..index
        };
        fs::write(&path, serde_json::to_vec(&index).unwrap()).unwrap();
        assert!(lookup_run_id_in(id, &storage, &[]).is_err());
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
