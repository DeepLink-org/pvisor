//! Ordinary CLI Jobs own native checkpoints; the VM executor owns capture/restore.
//! A sealed checkpoint is not suspension evidence. Only the terminal native
//! receipt grants a new Attempt the right to resume.
use super::RunRecord;
use super::registry::RunLease;
use super::run::RunControlHandle;
use crate::config::{RunConfig, RunExecutorKind};
use anyhow::{Context, ensure};
use pvisor_core::operation::{
    ExecutionCheckpoint, ExecutionSuspension, OperationKind, SnapshotRamStorage, Value,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub(crate) const STORE_KEY: &str = "pvisor.orchestration.execution_snapshot_store";

pub(crate) fn snapshot_store(
    stage: &Path,
    metadata: &BTreeMap<String, serde_json::Value>,
) -> anyhow::Result<PathBuf> {
    let path = match metadata.get(STORE_KEY) {
        Some(value) => PathBuf::from(value.as_str().context("invalid execution store binding")?),
        None => stage.join("execution-snapshots"),
    };
    ensure!(
        path.is_absolute()
            && path
                .components()
                .all(|part| !matches!(part, std::path::Component::ParentDir)),
        "execution store binding must be absolute"
    );
    Ok(path)
}

const STATE: &str = "execution-job.json";
const ROOT: &str = "execution-job-root.json";
const SOCKET: &str = "execution.sock";
const MAX_FRAME: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Capture {
    pub checkpoint: ExecutionCheckpoint,
    pub branches: BTreeMap<String, PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Request {
    pub suspend: bool,
    pub ram_storage: SnapshotRamStorage,
    pub checkpoint: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JobState {
    Running,
    Suspending,
    Suspended,
    Restoring,
    Terminal,
    Unknown,
}

impl std::fmt::Display for JobState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Running => "running",
            Self::Suspending => "suspending",
            Self::Suspended => "suspended",
            Self::Restoring => "restoring",
            Self::Terminal => "terminal",
            Self::Unknown => "unknown",
        })
    }
}

pub(crate) const JOB_SCHEMA_VERSION: u16 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResumeRequest {
    pub stage: PathBuf,
    pub eager_ram: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ForkOptions {
    pub checkpoint: Option<String>,
    pub stage: Option<PathBuf>,
    pub name: Option<String>,
    pub ram_storage: Option<SnapshotRamStorage>,
    pub eager_ram: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ForkRequest {
    pub options: ForkOptions,
    pub stage: PathBuf,
    pub job_id: String,
    pub checkpoint_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Job {
    pub version: u16,
    pub run_id: String,
    pub root: PathBuf,
    pub active_stage: PathBuf,
    pub previous_stage: PathBuf,
    pub active_attempt: String,
    pub config: RunConfig,
    pub spec: pvisor_core::RunSpec,
    pub state: JobState,
    pub head: Option<String>,
    pub checkpoints: BTreeMap<String, Capture>,
    pub requests: BTreeMap<String, Request>,
    pub resumes: BTreeMap<String, ResumeRequest>,
    pub forks: BTreeMap<String, ForkRequest>,
    pub stores: std::collections::BTreeSet<PathBuf>,
}

impl Job {
    pub fn read(record: &RunRecord) -> anyhow::Result<Option<Self>> {
        Self::read_stage(&record.stage_dir())
    }
    pub fn read_stage(stage: &Path) -> anyhow::Result<Option<Self>> {
        let root: PathBuf = match std::fs::read(stage.join(ROOT)) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => stage.to_owned(),
            Err(error) => return Err(error.into()),
        };
        let bytes = match std::fs::read(root.join(STATE)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let job: Self = serde_json::from_slice(&bytes)?;
        ensure!(
            job.version == JOB_SCHEMA_VERSION
                && job.root == root
                && job.root.is_absolute()
                && job.active_stage.is_absolute()
                && job.previous_stage.is_absolute()
                && (job.active_stage == job.root
                    || job.active_stage.starts_with(job.root.join("attempts"))),
            "invalid execution Job binding"
        );
        Ok(Some(job))
    }
    pub fn write(&self) -> anyhow::Result<()> {
        crate::util::write_run_json(&self.root.join(STATE), self, &self.run_id, "execution_job")
    }
    pub fn lock(&self) -> anyhow::Result<RunLease> {
        RunLease::acquire(&self.root.join("execution-operation"))
    }
    async fn lock_wait(&self) -> anyhow::Result<RunLease> {
        // Metadata sections never await VM work. Retry contention without
        // treating a second client as a capture/storage failure.
        loop {
            match self.lock() {
                Ok(lease) => return Ok(lease),
                Err(error) if error.to_string().contains("already leased") => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
    pub fn current(&self) -> anyhow::Result<Self> {
        let current = Self::read_stage(&self.root)?.context("execution Job disappeared")?;
        ensure!(
            current.run_id == self.run_id,
            "execution Job identity changed"
        );
        Ok(current)
    }
    pub fn checkpoint(&self, id: &str) -> anyhow::Result<ExecutionCheckpoint> {
        let capture = self
            .checkpoints
            .get(id)
            .context("execution checkpoint not found in this Job")?;
        capture.checkpoint.validate()?;
        ensure!(
            capture.checkpoint.source_run_id == self.run_id,
            "checkpoint owner mismatch"
        );
        ensure!(
            self.stores.contains(&capture.checkpoint.store),
            "checkpoint store is outside this Job's bound stores"
        );
        Ok(capture.checkpoint.clone())
    }
    pub fn primary_store(&self) -> anyhow::Result<PathBuf> {
        snapshot_store(&self.root, &self.spec.metadata)
    }
    pub fn link_stage(&self, stage: &Path) -> anyhow::Result<()> {
        crate::util::write_run_json(
            &stage.join(ROOT),
            &self.root,
            &self.run_id,
            "execution_job_root",
        )
    }
}

pub(crate) fn require_mutable(record: &RunRecord) -> anyhow::Result<()> {
    if let Some(job) = Job::read(record)? {
        ensure!(job.run_id == record.run_id, "execution Job owner mismatch");
        ensure!(
            job.state == JobState::Terminal,
            "JOB_BUSY: Job {} is {}; kill a suspended Job before changing its workspace",
            job.run_id,
            job.state
        );
    }
    Ok(())
}

pub(crate) fn job(record: &RunRecord) -> anyhow::Result<Job> {
    let job = Job::read(record)?
        .context("CAPABILITY_UNSUPPORTED: this Job has no native execution handoff")?;
    ensure!(
        job.run_id == record.run_id && job.config.run.executor == RunExecutorKind::Vm,
        "execution Job owner/executor mismatch"
    );
    Ok(job)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlRequest {
    run_id: String,
    attempt_id: String,
    request_id: String,
    suspend: bool,
    ram_storage: SnapshotRamStorage,
}

fn bind_control_endpoint(
    stage: &Path,
) -> anyhow::Result<(tokio::net::UnixListener, tempfile::TempDir)> {
    use std::os::unix::fs::PermissionsExt;
    // Attempt paths include UUIDs beneath user-selected storage. Keep the Unix
    // address short and discover it through the private stage, as with Run control.
    let directory = tempfile::Builder::new()
        .prefix("pvisor-execution-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")?;
    let socket = directory.path().join(SOCKET);
    let listener = tokio::net::UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    std::os::unix::fs::symlink(&socket, stage.join(SOCKET))?;
    Ok((listener, directory))
}

pub(crate) struct Server {
    task: tokio::task::JoinHandle<()>,
    stage: PathBuf,
    _socket_directory: tempfile::TempDir,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(self.stage.join(SOCKET));
    }
}

impl Server {
    pub fn start(
        record: &RunRecord,
        mut config: RunConfig,
        spec: pvisor_core::RunSpec,
        controls: RunControlHandle,
    ) -> anyhow::Result<Self> {
        let stage = record.stage_dir();
        if let Some(value) = &mut config.vm.library_dir {
            *value = value.canonicalize()?;
        }
        if Job::read(record)?.is_none()
            && let Some(root) = &mut config.vm.rootfs
        {
            *root = root.canonicalize()?;
        }
        let store = snapshot_store(&stage, &spec.metadata)?;
        if let Some(pool) = &mut config.vm.snapshot_filesystem_pool {
            // A configured pool can be new. Initialize it through the store's
            // private-directory validation before persisting a canonical path.
            if let Some(parent) = store.parent() {
                crate::util::create_dir_all_durable(parent)?;
            }
            if let Some(parent) = pool.parent() {
                crate::util::create_dir_all_durable(parent)?;
            }
            crate::environment_snapshot::SnapshotStore::with_filesystem_pool(&store, pool)?;
            *pool = pool.canonicalize()?;
        }
        let mut job = match Job::read(record)? {
            Some(mut job) => {
                ensure!(
                    job.run_id == record.run_id
                        && job.active_stage == stage
                        && job.state == JobState::Restoring,
                    "invalid resumed Job ownership"
                );
                job.active_attempt = record
                    .attempt_id
                    .clone()
                    .context("missing Attempt identity")?;
                job.state = JobState::Running;
                job.head = None;
                job
            }
            None => Job {
                version: JOB_SCHEMA_VERSION,
                run_id: record.run_id.clone(),
                root: stage.clone(),
                active_stage: stage.clone(),
                previous_stage: stage.clone(),
                active_attempt: record
                    .attempt_id
                    .clone()
                    .context("missing Attempt identity")?,
                config,
                spec,
                state: JobState::Running,
                head: None,
                checkpoints: BTreeMap::new(),
                requests: BTreeMap::new(),
                resumes: BTreeMap::new(),
                forks: BTreeMap::new(),
                stores: Default::default(),
            },
        };
        job.stores.insert(store);
        job.write()?;
        job.link_stage(&stage)?;
        let (listener, socket_directory) = bind_control_endpoint(&stage)?;
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let job = job.clone();
                let controls = controls.clone();
                tokio::spawn(async move {
                    let (reader, mut writer) = stream.into_split();
                    let result = async {
                        use tokio::io::AsyncReadExt;
                        let mut line = String::new();
                        BufReader::new(reader.take(MAX_FRAME))
                            .read_line(&mut line)
                            .await?;
                        ensure!(line.ends_with('\n'), "invalid execution control frame");
                        let request: ControlRequest = serde_json::from_str(&line)?;
                        handle_request(&job, controls, request).await
                    }
                    .await;
                    let reply = match result {
                        Ok(checkpoint) => serde_json::json!({"checkpoint":checkpoint}),
                        Err(error) => serde_json::json!({"error":format!("{error:#}")}),
                    };
                    if let Ok(mut bytes) = serde_json::to_vec(&reply) {
                        bytes.push(b'\n');
                        let _ = writer.write_all(&bytes).await;
                    }
                });
            }
        });
        Ok(Self {
            task,
            stage,
            _socket_directory: socket_directory,
        })
    }
    pub async fn finish(&self, result: &pvisor_core::RunResult) -> anyhow::Result<()> {
        let job = Job::read_stage(&self.stage)?.context("execution Job missing at completion")?;
        // The capture handler releases the metadata lock before awaiting native
        // exit, so terminal receipt persistence cannot deadlock the VM teardown.
        let _lease = job.lock_wait().await?;
        let mut job = job.current()?;
        ensure!(
            job.active_attempt == result.attempt_id.as_str(),
            "terminal Attempt identity mismatch"
        );
        if result.state == pvisor_core::RunState::Hibernated {
            let receipt = ExecutionSuspension::from_result(result)?;
            let id = receipt.checkpoint.snapshot_id.clone();
            job.checkpoints.entry(id.clone()).or_insert(Capture {
                checkpoint: receipt.checkpoint,
                branches: BTreeMap::new(),
            });
            job.requests
                .get_mut(&receipt.request_id)
                .context("suspend receipt has no Job request")?
                .checkpoint = Some(id.clone());
            job.head = Some(id);
            job.state = JobState::Suspended;
        } else {
            job.state = JobState::Terminal;
            job.head = None;
        }
        job.write()
    }
}

async fn handle_request(
    template: &Job,
    mut controls: RunControlHandle,
    request: ControlRequest,
) -> anyhow::Result<ExecutionCheckpoint> {
    ensure!(
        !request.request_id.trim().is_empty() && request.request_id.len() <= 256,
        "invalid request id"
    );
    controls.wait_ready().await?;
    let lease = template.lock_wait().await?;
    let mut job = template.current()?;
    ensure!(
        request.run_id == job.run_id && request.attempt_id == job.active_attempt,
        "stale Job/Attempt control request"
    );
    if let Some(previous) = job.requests.get(&request.request_id) {
        ensure!(
            previous.suspend == request.suspend && previous.ram_storage == request.ram_storage,
            "REQUEST_ID_CONFLICT: request options changed"
        );
        if let Some(id) = &previous.checkpoint {
            return job.checkpoint(id);
        }
        anyhow::bail!(
            "EXECUTION_UNKNOWN: request already admitted: {}",
            previous.error.as_deref().unwrap_or("pending")
        );
    }
    ensure!(
        job.state == JobState::Running,
        "JOB_BUSY: execution state is {}",
        job.state
    );
    let kind = if request.suspend {
        OperationKind::RunSuspend {
            request_id: request.request_id.clone(),
            ram_storage: request.ram_storage,
        }
    } else {
        OperationKind::RunCheckpoint {
            request_id: request.request_id.clone(),
            ram_storage: request.ram_storage,
        }
    };
    job.requests.insert(
        request.request_id.clone(),
        Request {
            suspend: request.suspend,
            ram_storage: request.ram_storage,
            checkpoint: None,
            error: None,
        },
    );
    if request.suspend {
        job.state = JobState::Suspending;
    }
    job.write()?;
    drop(lease);
    let result = controls.control(kind).await;
    let _lease = template.lock_wait().await?;
    let mut job = template.current()?;
    match result {
        Ok(Value::ExecutionCheckpoint { checkpoint }) => {
            ensure!(
                checkpoint.source_run_id == job.run_id
                    && checkpoint.source_attempt_id == request.attempt_id,
                "native capture owner mismatch"
            );
            job.checkpoints
                .entry(checkpoint.snapshot_id.clone())
                .or_insert(Capture {
                    checkpoint: checkpoint.clone(),
                    branches: BTreeMap::new(),
                });
            job.requests
                .get_mut(&request.request_id)
                .context("request disappeared")?
                .checkpoint = Some(checkpoint.snapshot_id.clone());
            job.write()?;
            Ok(checkpoint)
        }
        Ok(_) => anyhow::bail!("native control returned no execution checkpoint"),
        Err(error) => {
            job.requests
                .get_mut(&request.request_id)
                .context("request disappeared")?
                .error = Some(format!("{error:#}"));
            if request.suspend && job.state == JobState::Suspending {
                job.state = JobState::Unknown;
            }
            job.write()?;
            Err(error)
        }
    }
}

/// Timeout abandons only the client's wait, never cancels or repeats capture.
pub(crate) async fn capture(
    record: &RunRecord,
    suspend: bool,
    ram_storage: SnapshotRamStorage,
    request_id: Option<String>,
    timeout: Duration,
) -> anyhow::Result<ExecutionCheckpoint> {
    let job = job(record)?;
    let request_id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    ensure!(
        !request_id.trim().is_empty() && request_id.len() <= 256,
        "invalid request id"
    );
    let work = async {
        let checkpoint = if let Some(previous) = job.requests.get(&request_id) {
            ensure!(
                previous.suspend == suspend && previous.ram_storage == ram_storage,
                "REQUEST_ID_CONFLICT: request options changed"
            );
            loop {
                let current = job.current()?;
                let previous = current
                    .requests
                    .get(&request_id)
                    .context("request disappeared")?;
                if let Some(id) = &previous.checkpoint {
                    break current.checkpoint(id)?;
                }
                if let Some(error) = &previous.error {
                    anyhow::bail!("EXECUTION_UNKNOWN: admitted request failed: {error}");
                }
                ensure!(
                    matches!(
                        current.state,
                        JobState::Running | JobState::Suspending | JobState::Suspended
                    ),
                    "EXECUTION_UNKNOWN: Attempt ended before capture acknowledgement"
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        } else if job.state == JobState::Suspended {
            let _lease = job.lock_wait().await?;
            let mut current = job.current()?;
            ensure!(
                current.state == JobState::Suspended,
                "JOB_BUSY: suspended head changed during request"
            );
            let checkpoint =
                current.checkpoint(current.head.as_deref().context("missing suspended head")?)?;
            if let Some(previous) = current.requests.get(&request_id) {
                ensure!(
                    previous.suspend == suspend
                        && previous.ram_storage == ram_storage
                        && previous.checkpoint.as_deref() == Some(&checkpoint.snapshot_id),
                    "REQUEST_ID_CONFLICT: request options changed"
                );
            } else {
                current.requests.insert(
                    request_id.clone(),
                    Request {
                        suspend,
                        ram_storage,
                        checkpoint: Some(checkpoint.snapshot_id.clone()),
                        error: None,
                    },
                );
                current.write()?;
            }
            checkpoint
        } else {
            ensure!(
                job.state == JobState::Running,
                "JOB_BUSY: Job execution state is {}",
                job.state
            );
            let socket = std::fs::canonicalize(job.active_stage.join(SOCKET))
                .context("EXECUTION_UNKNOWN: owning Job execution control endpoint unavailable")?;
            let stream = tokio::net::UnixStream::connect(socket)
                .await
                .context("EXECUTION_UNKNOWN: owning Job execution control endpoint unavailable")?;
            let (reader, mut writer) = stream.into_split();
            let request = ControlRequest {
                run_id: job.run_id.clone(),
                attempt_id: job.active_attempt.clone(),
                request_id: request_id.clone(),
                suspend,
                ram_storage,
            };
            let mut bytes = serde_json::to_vec(&request)?;
            bytes.push(b'\n');
            writer.write_all(&bytes).await?;
            use tokio::io::AsyncReadExt;
            let mut reply = String::new();
            BufReader::new(reader.take(MAX_FRAME))
                .read_line(&mut reply)
                .await?;
            ensure!(
                reply.ends_with('\n'),
                "execution control disconnected before acknowledgement"
            );
            let value: serde_json::Value = serde_json::from_str(&reply)?;
            if let Some(error) = value["error"].as_str() {
                anyhow::bail!("{error}");
            }
            serde_json::from_value(value["checkpoint"].clone())?
        };
        if suspend {
            loop {
                let current = job.current()?;
                if current.state == JobState::Suspended
                    && current.head.as_deref() == Some(&checkpoint.snapshot_id)
                    && !super::is_live(&current.active_stage)?
                {
                    break;
                }
                ensure!(
                    matches!(current.state, JobState::Suspending | JobState::Suspended),
                    "EXECUTION_UNKNOWN: suspend termination not confirmed ({})",
                    current.state
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
        Ok(checkpoint)
    };
    tokio::time::timeout(timeout, work).await.with_context(|| format!("EXECUTION_UNKNOWN: timed out waiting for request {request_id}; capture may still complete; inspect status or retry the same request id"))?
}

pub(crate) fn terminate_suspended(record: &RunRecord) -> anyhow::Result<bool> {
    let Some(template) = Job::read(record)? else {
        return Ok(false);
    };
    let _lease = template.lock()?;
    let mut job = template.current()?;
    if job.state != JobState::Suspended {
        return Ok(false);
    }
    let _attempt = RunLease::acquire(&job.active_stage)?;
    job.head = None;
    job.state = JobState::Terminal;
    job.write()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn long_attempt_paths_use_a_private_short_socket_and_clean_up() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("a".repeat(80)).join("b".repeat(80));
        std::fs::create_dir_all(&stage).unwrap();
        let locator = stage.join(SOCKET);
        assert!(locator.as_os_str().as_encoded_bytes().len() > 108);
        let (listener, directory) = bind_control_endpoint(&stage).unwrap();
        let address = std::fs::canonicalize(&locator).unwrap();
        assert!(address.as_os_str().as_encoded_bytes().len() < 104);
        assert_eq!(
            std::fs::metadata(&address).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(directory.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let client = tokio::net::UnixStream::connect(&address).await.unwrap();
        let (peer, _) = listener.accept().await.unwrap();
        drop(client);
        drop(peer);
        let directory_path = directory.path().to_owned();
        let server = Server {
            task: tokio::spawn(async move { std::future::pending::<()>().await }),
            stage,
            _socket_directory: directory,
        };
        drop(server);
        assert!(!locator.is_symlink());
        assert!(!address.exists());
        assert!(!directory_path.exists());
    }

    #[tokio::test]
    async fn an_existing_stage_endpoint_is_not_replaced() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(SOCKET), b"existing owner").unwrap();
        assert!(bind_control_endpoint(temp.path()).is_err());
        assert_eq!(
            std::fs::read(temp.path().join(SOCKET)).unwrap(),
            b"existing owner"
        );
    }

    fn fixture(root: &Path) -> (RunRecord, Job) {
        let record: RunRecord = serde_json::from_value(serde_json::json!({
            "schema_version":1,"run_id":"job","attempt_id":"attempt-original","session_id":"job",
            "agent":"probe","pid":0,"command":["probe"],"state":"hibernated",
            "started_at_unix_ms":1,"finished_at_unix_ms":2,"storage":root,
            "network":{},"gateway_listen":null,"overlay":null
        }))
        .unwrap();
        record.write().unwrap();
        let mut config = RunConfig::default();
        config.run.executor = RunExecutorKind::Vm;
        let job = Job {
            version: JOB_SCHEMA_VERSION,
            run_id: "job".into(),
            root: root.into(),
            active_stage: root.into(),
            previous_stage: root.into(),
            active_attempt: "attempt-original".into(),
            config,
            spec: pvisor_core::RunSpec::process("job", "probe", "probe"),
            state: JobState::Suspended,
            head: None,
            checkpoints: BTreeMap::new(),
            requests: BTreeMap::new(),
            resumes: BTreeMap::new(),
            forks: BTreeMap::new(),
            stores: [root.join("execution-snapshots")].into(),
        };
        job.write().unwrap();
        (record, job)
    }
    fn checkpoint(root: &Path) -> ExecutionCheckpoint {
        ExecutionCheckpoint {
            snapshot_id: "a".repeat(64),
            store: root.join("execution-snapshots"),
            source_run_id: "job".into(),
            source_attempt_id: "attempt-original".into(),
            created_at_unix_ms: 2,
            ram_storage: SnapshotRamStorage::Raw,
        }
    }

    #[test]
    fn suspended_and_uncertain_attempts_cannot_mutate_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let (record, mut job) = fixture(temp.path());
        for state in [
            JobState::Running,
            JobState::Suspending,
            JobState::Suspended,
            JobState::Restoring,
            JobState::Unknown,
        ] {
            job.state = state;
            job.write().unwrap();
            assert!(
                record
                    .require_stopped()
                    .unwrap_err()
                    .to_string()
                    .contains("JOB_BUSY")
            );
        }
        job.state = JobState::Terminal;
        job.write().unwrap();
        record.require_stopped().unwrap();
    }

    #[test]
    fn execution_records_require_current_typed_bindings() {
        let temp = tempfile::tempdir().unwrap();
        let (record, job) = fixture(temp.path());
        let original = serde_json::to_value(&job).unwrap();
        let mut variants = Vec::new();
        for field in ["forks", "stores", "resumes"] {
            let mut value = original.clone();
            value.as_object_mut().unwrap().remove(field);
            variants.push(value);
        }
        let mut value = original.clone();
        value["state"] = serde_json::json!("misspelled-state");
        variants.push(value);
        let mut value = original.clone();
        value["version"] = serde_json::json!(1);
        variants.push(value);
        let mut value = original;
        value["resumes"] = serde_json::json!({"key": {"stage": "/private/attempt"}});
        variants.push(value);
        for value in variants {
            std::fs::write(job.root.join(STATE), serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(Job::read(&record).is_err(), "{value}");
        }
    }

    #[test]
    fn stable_job_selector_retains_attempt_records_and_rejects_rebinding() {
        let temp = tempfile::tempdir().unwrap();
        let (record, mut job) = fixture(temp.path());
        let stage = temp.path().join("attempts/new");
        job.active_stage = stage.clone();
        job.state = JobState::Restoring;
        job.write().unwrap();
        assert_eq!(
            RunRecord::read(temp.path()).unwrap().attempt_id.as_deref(),
            Some("attempt-original")
        );
        std::fs::create_dir_all(&stage).unwrap();
        let mut successor = record.clone();
        successor.storage = stage.clone();
        successor.attempt_id = Some("attempt-new".into());
        successor.write().unwrap();
        job.link_stage(&stage).unwrap();
        assert_eq!(
            RunRecord::read(temp.path()).unwrap().attempt_id.as_deref(),
            Some("attempt-new")
        );
        let archived: RunRecord =
            serde_json::from_slice(&std::fs::read(temp.path().join("run.json")).unwrap()).unwrap();
        assert_eq!(archived.attempt_id.as_deref(), Some("attempt-original"));
        successor.run_id = "other".into();
        successor.write().unwrap();
        assert!(RunRecord::read(temp.path()).is_err());
    }

    #[tokio::test]
    async fn only_native_terminal_receipt_promotes_sealed_snapshot_to_suspended_head() {
        let temp = tempfile::tempdir().unwrap();
        let (_, mut job) = fixture(temp.path());
        let checkpoint = checkpoint(temp.path());
        job.state = JobState::Suspending;
        job.requests.insert(
            "suspend".into(),
            Request {
                suspend: true,
                ram_storage: SnapshotRamStorage::Raw,
                checkpoint: None,
                error: None,
            },
        );
        job.write().unwrap();
        let server = Server {
            task: tokio::spawn(std::future::pending()),
            stage: temp.path().into(),
            _socket_directory: tempfile::tempdir().unwrap(),
        };
        let mut result:pvisor_core::RunResult=serde_json::from_value(serde_json::json!({
            "run_id":"job","attempt_id":"attempt-original","state":"hibernated","started_at_unix_ms":1,"finished_at_unix_ms":3,
            "value":{"request_id":"suspend","checkpoint":checkpoint}
        })).unwrap();
        result.exit_code = Some(0);
        assert!(server.finish(&result).await.is_err());
        assert!(job.current().unwrap().head.is_none());
        result.exit_code = None;
        server.finish(&result).await.unwrap();
        let current = job.current().unwrap();
        assert_eq!(current.state, JobState::Suspended);
        assert_eq!(
            current.head.as_deref(),
            Some(checkpoint.snapshot_id.as_str())
        );
    }

    #[test]
    fn killing_suspended_job_releases_only_execution_right_not_history() {
        let temp = tempfile::tempdir().unwrap();
        let (record, mut job) = fixture(temp.path());
        let checkpoint = checkpoint(temp.path());
        job.head = Some(checkpoint.snapshot_id.clone());
        job.checkpoints.insert(
            checkpoint.snapshot_id.clone(),
            Capture {
                checkpoint,
                branches: BTreeMap::new(),
            },
        );
        job.write().unwrap();
        assert!(terminate_suspended(&record).unwrap());
        assert!(!terminate_suspended(&record).unwrap());
        let current = job.current().unwrap();
        assert!(current.head.is_none());
        assert_eq!(current.checkpoints.len(), 1);
        record.require_stopped().unwrap();
    }

    #[tokio::test]
    async fn request_replay_cannot_change_operation_or_ram_encoding() {
        let temp = tempfile::tempdir().unwrap();
        let (record, mut job) = fixture(temp.path());
        let checkpoint = checkpoint(temp.path());
        let id = checkpoint.snapshot_id.clone();
        job.state = JobState::Terminal;
        job.checkpoints.insert(
            id.clone(),
            Capture {
                checkpoint,
                branches: BTreeMap::new(),
            },
        );
        job.requests.insert(
            "capture".into(),
            Request {
                suspend: false,
                ram_storage: SnapshotRamStorage::Raw,
                checkpoint: Some(id.clone()),
                error: None,
            },
        );
        job.write().unwrap();
        let result = capture(
            &record,
            false,
            SnapshotRamStorage::Raw,
            Some("capture".into()),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(result.snapshot_id, id);
        let error = capture(
            &record,
            true,
            SnapshotRamStorage::Raw,
            Some("capture".into()),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("REQUEST_ID_CONFLICT"));
        let error = capture(
            &record,
            false,
            SnapshotRamStorage::Compressed,
            Some("capture".into()),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("REQUEST_ID_CONFLICT"));
    }
}
