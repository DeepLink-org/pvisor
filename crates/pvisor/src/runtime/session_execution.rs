//! Ordinary CLI Sessions own native checkpoints; the VM executor owns capture/restore.
//! A sealed checkpoint is not suspension evidence. Only the terminal native
//! receipt grants a new Execution the right to resume.
use super::RunRecord;
use super::execution::{Execution, Session, SessionState, SessionTransition};
use super::registry::RunLease;
use crate::config::{RunConfig, RunExecutorKind};
use anyhow::{Context, ensure};
use pvisor_core::host_protocol::{
    AGENTCTL_HOST_VERSION, AgentCtlHostError, AgentCtlHostErrorCode, AgentCtlHostRequest,
    HostExecutionCommand,
};
use pvisor_core::operation::{ExecutionCheckpoint, OperationKind, SnapshotRamStorage, Value};
use pvisor_journal::api::{DurableFiles, Persistence};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

pub const STORE_KEY: &str = "pvisor.orchestration.execution_snapshot_store";

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

const STATE: &str = "execution-session.json";
const ROOT: &str = "execution-session-root.json";
const SESSION_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capture {
    pub checkpoint: ExecutionCheckpoint,
    pub branches: BTreeMap<String, PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub suspend: bool,
    pub ram_storage: SnapshotRamStorage,
    pub checkpoint: Option<String>,
    pub error: Option<String>,
}

pub const SESSION_SCHEMA_VERSION: u16 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeRequest {
    pub stage: PathBuf,
    pub eager_ram: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ForkOptions {
    pub checkpoint: Option<String>,
    pub stage: Option<PathBuf>,
    pub name: Option<String>,
    pub ram_storage: Option<SnapshotRamStorage>,
    pub eager_ram: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkRequest {
    pub options: ForkOptions,
    pub stage: PathBuf,
    pub session_id: String,
    pub checkpoint_id: String,
}

impl Session {
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
        let session: Self = serde_json::from_slice(&bytes)?;
        ensure!(
            session.version == SESSION_SCHEMA_VERSION
                && session.root == root
                && session.root.is_absolute()
                && session.active_stage.is_absolute()
                && session.previous_stage.is_absolute()
                && (session.active_stage == session.root
                    || session
                        .active_stage
                        .starts_with(session.root.join("executions"))),
            "invalid execution Session binding"
        );
        Ok(Some(session))
    }
    pub fn write(&self) -> anyhow::Result<()> {
        crate::util::write_run_json(
            &self.root.join(STATE),
            self,
            &self.run_id,
            "execution_session",
        )
    }
    pub fn lock(&self) -> anyhow::Result<RunLease> {
        RunLease::acquire(&self.root.join("execution-operation"))
    }
    async fn lock_wait(&self) -> anyhow::Result<RunLease> {
        self.lock_wait_until(tokio::time::Instant::now() + SESSION_LOCK_TIMEOUT)
            .await
    }

    async fn lock_wait_until(&self, deadline: tokio::time::Instant) -> anyhow::Result<RunLease> {
        // Metadata sections never await VM work. Retry contention without
        // treating a second client as a capture/storage failure.
        loop {
            match self.lock() {
                Ok(lease) => return Ok(lease),
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::WouldBlock) =>
                {
                    anyhow::ensure!(
                        tokio::time::Instant::now() < deadline,
                        "execution Session lease wait timed out; operation outcome may be unknown"
                    );
                    tokio::time::sleep_until(
                        deadline.min(tokio::time::Instant::now() + Duration::from_millis(10)),
                    )
                    .await;
                }
                Err(error) => return Err(error),
            }
        }
    }
    pub fn current(&self) -> anyhow::Result<Self> {
        let current = Self::read_stage(&self.root)?.context("execution Session disappeared")?;
        ensure!(
            current.run_id == self.run_id,
            "execution Session identity changed"
        );
        Ok(current)
    }
    /// Call on a freshly reread Session while holding its execution-operation lease.
    /// Selection-time validation is not a fence against a concurrent resume.
    pub fn validate_record_target(&self, record: &RunRecord) -> Result<(), AgentCtlHostError> {
        if self.run_id != record.run_id
            || record.execution_id.as_deref() != Some(self.active_execution.as_str())
        {
            return Err(AgentCtlHostError::new(
                AgentCtlHostErrorCode::Conflict,
                "stale Session/Execution record target",
            ));
        }
        Ok(())
    }

    pub fn checkpoint(&self, id: &str) -> anyhow::Result<ExecutionCheckpoint> {
        let capture = self
            .checkpoints
            .get(id)
            .context("execution checkpoint not found in this Session")?;
        capture.checkpoint.validate()?;
        ensure!(
            capture.checkpoint.source_run_id == self.run_id,
            "checkpoint owner mismatch"
        );
        ensure!(
            self.stores.contains(&capture.checkpoint.store),
            "checkpoint store is outside this Session's bound stores"
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
            "execution_session_root",
        )
    }
}

pub(crate) fn require_mutable(record: &RunRecord) -> anyhow::Result<()> {
    if let Some(session) = Session::read(record)? {
        ensure!(
            session.run_id == record.run_id,
            "execution Session owner mismatch"
        );
        ensure!(
            session.state() == SessionState::Terminal,
            "SESSION_BUSY: Session {} is {}; kill a suspended Session before changing its workspace",
            session.run_id,
            session.state()
        );
    }
    Ok(())
}

pub fn session(record: &RunRecord) -> anyhow::Result<Session> {
    let session = Session::read(record)?
        .context("CAPABILITY_UNSUPPORTED: this Session has no native execution handoff")?;
    ensure!(
        session.run_id == record.run_id && session.config.run.executor == RunExecutorKind::Vm,
        "execution Session owner/executor mismatch"
    );
    Ok(session)
}

pub(crate) fn capture_error(error: anyhow::Error) -> AgentCtlHostError {
    if let Some(error) = error.downcast_ref::<AgentCtlHostError>() {
        return error.clone();
    }
    let message = format!("{error:#}");
    let code = if message.contains("REQUEST_ID_CONFLICT")
        || message.contains("SESSION_BUSY")
        || message.contains("stale Session/Execution")
    {
        AgentCtlHostErrorCode::Conflict
    } else {
        // An admitted native operation may have committed; callers must reconcile
        // the durable request ledger rather than retrying with a new identity.
        AgentCtlHostErrorCode::Unavailable
    };
    AgentCtlHostError::new(code, message)
}

#[derive(Debug)]
struct ControlRequest {
    run_id: String,
    execution_id: String,
    request_id: String,
    suspend: bool,
    ram_storage: SnapshotRamStorage,
}

impl Session {
    pub(crate) fn attach(
        record: &RunRecord,
        mut config: RunConfig,
        spec: pvisor_core::RunSpec,
    ) -> anyhow::Result<Self> {
        let stage = record.stage_dir();
        if let Some(value) = &mut config.vm.library_dir {
            *value = value.canonicalize()?;
        }
        if Session::read(record)?.is_none()
            && let Some(root) = &mut config.vm.rootfs
        {
            *root = root.canonicalize()?;
        }
        let store = snapshot_store(&stage, &spec.metadata)?;
        if let Some(pool) = &mut config.vm.snapshot_filesystem_pool {
            // A configured pool can be new. Initialize it through the store's
            // private-directory validation before persisting a canonical path.
            if let Some(parent) = store.parent() {
                Persistence::create_dir_all_durable(parent)?;
            }
            if let Some(parent) = pool.parent() {
                Persistence::create_dir_all_durable(parent)?;
            }
            crate::environment_snapshot::SnapshotStore::with_filesystem_pool(&store, pool)?;
            *pool = pool.canonicalize()?;
        }
        let mut session = match Session::read(record)? {
            Some(mut session) => {
                session.apply(SessionTransition::ExecutionStarted { record })?;
                session
            }
            None => Session::new(
                record.run_id.clone(),
                stage.clone(),
                Some(
                    record
                        .execution_id
                        .clone()
                        .context("missing Execution identity")?,
                ),
                config,
                spec,
            )?,
        };
        session.stores.insert(store);
        session.write()?;
        session.link_stage(&stage)?;
        Ok(session)
    }
    pub(crate) async fn finish(&self, result: &pvisor_core::RunResult) -> anyhow::Result<()> {
        let session = self
            .current()
            .context("execution Session missing at completion")?;
        // The capture handler releases the metadata lock before awaiting native
        // exit, so terminal receipt persistence cannot deadlock the VM teardown.
        let _lease = session.lock_wait().await?;
        let mut session = session.current()?;
        session.apply(SessionTransition::Completed(result))?;
        session.write()
    }
}

fn validate_capture_binding(
    template: &Session,
    session: &Session,
    request: &ControlRequest,
) -> Result<(), AgentCtlHostError> {
    if request.run_id != session.run_id
        || request.execution_id != session.active_execution
        || template.active_execution != session.active_execution
        || template.active_stage != session.active_stage
    {
        return Err(AgentCtlHostError::new(
            AgentCtlHostErrorCode::Conflict,
            "stale Session/Execution control request",
        ));
    }
    Ok(())
}

async fn handle_request(
    template: &Session,
    controls: Execution,
    request: ControlRequest,
) -> anyhow::Result<ExecutionCheckpoint> {
    ensure!(
        !request.request_id.trim().is_empty() && request.request_id.len() <= 256,
        "invalid request id"
    );
    validate_capture_binding(template, &template.current()?, &request)?;
    controls.wait_ready().await?;
    let lease = template.lock_wait().await?;
    let mut session = template.current()?;
    validate_capture_binding(template, &session, &request)?;
    if let Some(previous) = session.requests.get(&request.request_id) {
        ensure!(
            previous.suspend == request.suspend && previous.ram_storage == request.ram_storage,
            "REQUEST_ID_CONFLICT: request options changed"
        );
        if let Some(id) = &previous.checkpoint {
            return session.checkpoint(id);
        }
        anyhow::bail!(
            "EXECUTION_UNKNOWN: request already admitted: {}",
            previous.error.as_deref().unwrap_or("pending")
        );
    }
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
    session.apply(SessionTransition::CaptureAdmitted {
        id: &request.request_id,
        suspend: request.suspend,
        ram_storage: request.ram_storage,
    })?;
    session.write()?;
    drop(lease);
    let result = controls
        .dispatch(HostExecutionCommand::Operation { kind })
        .await
        .map(|result| result.value)
        .map_err(anyhow::Error::from);
    let _lease = template.lock_wait().await?;
    let mut session = template.current()?;
    validate_capture_binding(template, &session, &request)?;
    match result {
        Ok(Some(Value::ExecutionCheckpoint { checkpoint })) => {
            session.apply(SessionTransition::CaptureAcknowledged {
                id: &request.request_id,
                checkpoint: &checkpoint,
            })?;
            session.write()?;
            Ok(checkpoint)
        }
        Ok(_) => anyhow::bail!("native control returned no execution checkpoint"),
        Err(error) => {
            session.apply(SessionTransition::CaptureRejected {
                id: &request.request_id,
                message: format!("{error:#}"),
            })?;
            session.write()?;
            Err(error)
        }
    }
}

/// Timeout abandons only the client's wait, never cancels or repeats capture.
pub(crate) async fn capture(
    context: &super::session_service::ServiceContext<'_>,
    record: &RunRecord,
    suspend: bool,
    ram_storage: SnapshotRamStorage,
    request_id: Option<String>,
    timeout: Duration,
) -> anyhow::Result<ExecutionCheckpoint> {
    let session = session(record)?;
    let request_id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    ensure!(
        !request_id.trim().is_empty() && request_id.len() <= 256,
        "invalid request id"
    );
    AgentCtlHostRequest {
        version: AGENTCTL_HOST_VERSION,
        request_id: request_id.clone(),
        target: None,
        command: (),
    }
    .validate()?;
    let work = async {
        // Snapshot the decision under the same lease as resume/fork/termination.
        // Never replace the caller's selected Execution with a newly active one.
        let session = {
            let _lease = session.lock_wait().await?;
            let current = session.current()?;
            current.validate_record_target(record)?;
            let current_record = RunRecord::read(&current.active_stage)?;
            super::session_service::check_selected_record(record, &current_record)?;
            context.check(&current_record)?;
            current
        };
        let checkpoint = if let Some(previous) = session.requests.get(&request_id) {
            ensure!(
                previous.suspend == suspend && previous.ram_storage == ram_storage,
                "REQUEST_ID_CONFLICT: request options changed"
            );
            loop {
                let lease = session.lock_wait().await?;
                let current = session.current()?;
                current.validate_record_target(record)?;
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
                        current.state(),
                        SessionState::Running | SessionState::Suspending | SessionState::Suspended
                    ),
                    "EXECUTION_UNKNOWN: Execution ended before capture acknowledgement"
                );
                drop(lease);
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        } else if session.state() == SessionState::Suspended {
            let _lease = session.lock_wait().await?;
            let mut current = session.current()?;
            current.validate_record_target(record)?;
            let current_record = RunRecord::read(&current.active_stage)?;
            super::session_service::check_selected_record(record, &current_record)?;
            context.check(&current_record)?;
            ensure!(
                current.state() == SessionState::Suspended,
                "SESSION_BUSY: suspended head changed during request"
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
                session.state() == SessionState::Running,
                "SESSION_BUSY: Session execution state is {}",
                session.state()
            );
            // Recheck caller authority after waiting for the Session lease. Accepted
            // work owns its task; timeout or disconnect only abandons the reply.
            let lease = session.lock_wait().await?;
            let current = session.current()?;
            current.validate_record_target(record)?;
            let current_record = RunRecord::read(&current.active_stage)?;
            super::session_service::check_selected_record(record, &current_record)?;
            context.check(&current_record)?;
            let checkpoint = if let Some(owner) = super::instance_control::local_owner(record) {
                let permit = owner.admit_capture()?;
                let request = ControlRequest {
                    run_id: record.run_id.clone(),
                    execution_id: record
                        .execution_id
                        .clone()
                        .context("missing Execution identity")?,
                    request_id: request_id.clone(),
                    suspend,
                    ram_storage,
                };
                let work = tokio::spawn(async move {
                    let _permit = permit;
                    handle_request(&current, owner.execution.clone(), request).await
                });
                drop(lease);
                work.await?.map_err(capture_error)?
            } else {
                let endpoint = current_record
                    .live_control
                    .clone()
                    .context("EXECUTION_UNKNOWN: owning Session control endpoint unavailable")?;
                ensure!(
                    endpoint.pid != std::process::id(),
                    "local Session owner unavailable"
                );
                let session_id = current_record.run_id;
                let execution_id = current_record
                    .execution_id
                    .context("missing Execution identity")?;
                let command = super::host_transport::InstanceCommand::Capture {
                    suspend,
                    ram_storage,
                    timeout_ms: timeout.as_millis().min(u64::MAX as u128) as u64,
                    request_id: request_id.clone(),
                };
                drop(lease);
                let reply = tokio::task::spawn_blocking(move || {
                    endpoint.exchange(&session_id, &execution_id, command)
                })
                .await??;
                let reply: super::session_service::CaptureResponse = serde_json::from_value(reply)?;
                ensure!(
                    reply.session_id == record.run_id
                        && reply.request_id == request_id
                        && reply.suspended == suspend,
                    "execution control capture receipt mismatch"
                );
                reply.checkpoint
            };
            checkpoint.validate()?;
            ensure!(
                checkpoint.source_run_id == session.run_id
                    && checkpoint.source_execution_id == session.active_execution,
                "execution control checkpoint identity mismatch"
            );
            checkpoint
        };
        if suspend {
            loop {
                let lease = session.lock_wait().await?;
                let current = session.current()?;
                current.validate_record_target(record)?;
                if current.state() == SessionState::Suspended
                    && current.head.as_deref() == Some(&checkpoint.snapshot_id)
                    && !super::is_live(&current.active_stage)?
                {
                    break;
                }
                ensure!(
                    matches!(
                        current.state(),
                        SessionState::Suspending | SessionState::Suspended
                    ),
                    "EXECUTION_UNKNOWN: suspend termination not confirmed ({})",
                    current.state()
                );
                drop(lease);
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
        Ok(checkpoint)
    };
    tokio::time::timeout(timeout, work).await.with_context(|| format!("EXECUTION_UNKNOWN: timed out waiting for request {request_id}; capture may still complete; inspect status or retry the same request id"))?
}

pub fn terminate_suspended(record: &RunRecord) -> anyhow::Result<bool> {
    let Some(template) = Session::read(record)? else {
        return Ok(false);
    };
    let _lease = template.lock()?;
    let mut session = template.current()?;
    session.validate_record_target(record)?;
    if session.state() != SessionState::Suspended {
        return Ok(false);
    }
    let _execution = RunLease::acquire(&session.active_stage)?;
    session.apply(SessionTransition::Terminated)?;
    session.write()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::super::host_transport::{read_host_frame, write_host_frame};
    use super::*;
    use pvisor_core::host_protocol::AgentCtlHostResponse;

    #[tokio::test(start_paused = true)]
    async fn session_lease_wait_has_a_deadline_and_does_not_steal_a_live_lease() {
        let temp = tempfile::tempdir().unwrap();
        let (_, session) = fixture(temp.path());
        let owner = session.lock().unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(25);
        let error = session
            .lock_wait_until(deadline)
            .await
            .err()
            .expect("deadline");
        assert!(error.to_string().contains("timed out"), "{error:#}");
        assert!(session.lock().is_err());
        drop(owner);
        assert!(session.lock_wait().await.is_ok());
    }

    fn fixture(root: &Path) -> (RunRecord, Session) {
        let record: RunRecord = serde_json::from_value(serde_json::json!({
            "schema_version":1,"run_id":"session","execution_id":"execution-original","session_id":"session",
            "agent":"probe","pid":0,"command":["probe"],"state":"hibernated",
            "started_at_unix_ms":1,"finished_at_unix_ms":2,"storage":root,
            "network":{},"gateway_listen":null,"overlay":null
        }))
        .unwrap();
        record.write().unwrap();
        let mut config = RunConfig::default();
        config.run.executor = RunExecutorKind::Vm;
        let session: Session = serde_json::from_value(serde_json::json!({
            "version":SESSION_SCHEMA_VERSION,"run_id":"session","root":root,"active_stage":root,
            "previous_stage":root,"active_execution":"execution-original","config":config,
            "spec":pvisor_core::RunSpec::process("session", "probe", "probe"),"state":"suspended",
            "head":null,"checkpoints":{},"requests":{},"resumes":{},"forks":{},"stores":[root.join("execution-snapshots")]
        })).unwrap();
        session.write().unwrap();
        (record, session)
    }
    fn checkpoint(root: &Path) -> ExecutionCheckpoint {
        ExecutionCheckpoint {
            snapshot_id: "a".repeat(64),
            store: root.join("execution-snapshots"),
            source_run_id: "session".into(),
            source_execution_id: "execution-original".into(),
            created_at_unix_ms: 2,
            ram_storage: SnapshotRamStorage::Raw,
        }
    }

    #[test]
    fn suspended_and_uncertain_executions_cannot_mutate_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let (record, mut session) = fixture(temp.path());
        for state in [
            SessionState::Running,
            SessionState::Suspending,
            SessionState::Suspended,
            SessionState::Restoring,
            SessionState::Unknown,
        ] {
            let mut stored = serde_json::to_value(&session).unwrap();
            stored["state"] = serde_json::to_value(state).unwrap();
            session = serde_json::from_value(stored).unwrap();
            session.write().unwrap();
            assert!(
                record
                    .require_stopped()
                    .unwrap_err()
                    .to_string()
                    .contains("SESSION_BUSY")
            );
        }
        let mut stored = serde_json::to_value(&session).unwrap();
        stored["state"] = serde_json::to_value(SessionState::Terminal).unwrap();
        session = serde_json::from_value(stored).unwrap();
        session.write().unwrap();
        record.require_stopped().unwrap();
    }

    #[test]
    fn execution_records_require_current_typed_bindings() {
        let temp = tempfile::tempdir().unwrap();
        let (record, session) = fixture(temp.path());
        let original = serde_json::to_value(&session).unwrap();
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
        value["resumes"] = serde_json::json!({"key": {"stage": "/private/execution"}});
        variants.push(value);
        for value in variants {
            std::fs::write(
                session.root.join(STATE),
                serde_json::to_vec(&value).unwrap(),
            )
            .unwrap();
            assert!(Session::read(&record).is_err(), "{value}");
        }
    }

    #[test]
    fn stable_session_selector_retains_execution_records_and_rejects_rebinding() {
        let temp = tempfile::tempdir().unwrap();
        let (record, mut session) = fixture(temp.path());
        let stage = temp.path().join("executions/new");
        session.active_stage = stage.clone();
        let mut stored = serde_json::to_value(&session).unwrap();
        stored["state"] = serde_json::to_value(SessionState::Restoring).unwrap();
        session = serde_json::from_value(stored).unwrap();
        session.write().unwrap();
        assert_eq!(
            RunRecord::read(temp.path())
                .unwrap()
                .execution_id
                .as_deref(),
            Some("execution-original")
        );
        std::fs::create_dir_all(&stage).unwrap();
        let mut successor = record.clone();
        successor.storage = stage.clone();
        successor.execution_id = Some("execution-new".into());
        successor.write().unwrap();
        session.link_stage(&stage).unwrap();
        assert_eq!(
            RunRecord::read(temp.path())
                .unwrap()
                .execution_id
                .as_deref(),
            Some("execution-new")
        );
        let archived: RunRecord =
            serde_json::from_slice(&std::fs::read(temp.path().join("run.json")).unwrap()).unwrap();
        assert_eq!(archived.execution_id.as_deref(), Some("execution-original"));
        successor.run_id = "other".into();
        successor.write().unwrap();
        assert!(RunRecord::read(temp.path()).is_err());
    }

    #[tokio::test]
    async fn only_native_terminal_receipt_promotes_sealed_snapshot_to_suspended_head() {
        let temp = tempfile::tempdir().unwrap();
        let (_, mut session) = fixture(temp.path());
        let checkpoint = checkpoint(temp.path());
        let mut stored = serde_json::to_value(&session).unwrap();
        stored["state"] = serde_json::to_value(SessionState::Suspending).unwrap();
        session = serde_json::from_value(stored).unwrap();
        session.requests.insert(
            "suspend".into(),
            Request {
                suspend: true,
                ram_storage: SnapshotRamStorage::Raw,
                checkpoint: None,
                error: None,
            },
        );
        session.write().unwrap();
        let server = session.clone();
        let mut result:pvisor_core::RunResult=serde_json::from_value(serde_json::json!({
            "run_id":"session","execution_id":"execution-original","state":"hibernated","started_at_unix_ms":1,"finished_at_unix_ms":3,
            "value":{"request_id":"suspend","checkpoint":checkpoint}
        })).unwrap();
        result.exit_code = Some(0);
        assert!(server.finish(&result).await.is_err());
        assert!(session.current().unwrap().head.is_none());
        result.exit_code = None;
        server.finish(&result).await.unwrap();
        let current = session.current().unwrap();
        assert_eq!(current.state(), SessionState::Suspended);
        assert_eq!(
            current.head.as_deref(),
            Some(checkpoint.snapshot_id.as_str())
        );
    }

    #[test]
    fn killing_suspended_session_releases_only_execution_right_not_history() {
        let temp = tempfile::tempdir().unwrap();
        let (record, mut session) = fixture(temp.path());
        let checkpoint = checkpoint(temp.path());
        session.head = Some(checkpoint.snapshot_id.clone());
        session.checkpoints.insert(
            checkpoint.snapshot_id.clone(),
            Capture {
                checkpoint,
                branches: BTreeMap::new(),
            },
        );
        session.write().unwrap();
        assert!(terminate_suspended(&record).unwrap());
        assert!(!terminate_suspended(&record).unwrap());
        let current = session.current().unwrap();
        assert!(current.head.is_none());
        assert_eq!(current.checkpoints.len(), 1);
        record.require_stopped().unwrap();
    }

    #[tokio::test]
    async fn capture_rechecks_selected_execution_after_waiting_for_operation_lock() {
        for state in [
            SessionState::Running,
            SessionState::Suspended,
            SessionState::Restoring,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let (record, mut session) = fixture(temp.path());
            let mut stored = serde_json::to_value(&session).unwrap();
            stored["state"] = serde_json::to_value(state).unwrap();
            session = serde_json::from_value(stored).unwrap();
            let checkpoint = checkpoint(temp.path());
            session.head = Some(checkpoint.snapshot_id.clone());
            session.checkpoints.insert(
                checkpoint.snapshot_id.clone(),
                Capture {
                    checkpoint: checkpoint.clone(),
                    branches: BTreeMap::new(),
                },
            );
            // Exercise completed replay as well as a fresh suspended-head request.
            if state != SessionState::Suspended {
                session.requests.insert(
                    "capture".into(),
                    Request {
                        suspend: false,
                        ram_storage: SnapshotRamStorage::Raw,
                        checkpoint: Some(checkpoint.snapshot_id),
                        error: None,
                    },
                );
            }
            session.write().unwrap();
            let lease = session.lock().unwrap();
            let context = super::super::session_service::ServiceContext::default();
            let mut pending = Box::pin(capture(
                &context,
                &record,
                false,
                SnapshotRamStorage::Raw,
                Some("capture".into()),
                Duration::from_secs(2),
            ));
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut pending)
                    .await
                    .is_err()
            );
            session.active_execution = "execution-replacement".into();
            session.write().unwrap();
            let before = std::fs::read(session.root.join(STATE)).unwrap();
            drop(lease);
            let error = pending.await.unwrap_err();
            assert_eq!(
                error.downcast_ref::<AgentCtlHostError>().unwrap().code,
                AgentCtlHostErrorCode::Conflict
            );
            assert_eq!(std::fs::read(session.root.join(STATE)).unwrap(), before);
        }
    }

    #[test]
    fn suspended_termination_rejects_stale_or_missing_record_identity_without_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let (record, mut session) = fixture(temp.path());
        session.active_execution = "execution-replacement".into();
        session.head = Some("retained-head".into());
        session.write().unwrap();
        let before = std::fs::read(session.root.join(STATE)).unwrap();
        for variant in 0..3 {
            let mut selected = record.clone();
            match variant {
                0 => {}
                1 => selected.execution_id = None,
                _ => selected.run_id = "other-session".into(),
            }
            let error = terminate_suspended(&selected).unwrap_err();
            assert_eq!(
                error.downcast_ref::<AgentCtlHostError>().unwrap().code,
                AgentCtlHostErrorCode::Conflict
            );
            assert_eq!(std::fs::read(session.root.join(STATE)).unwrap(), before);
        }
        let mut current = record;
        current.execution_id = Some(session.active_execution.clone());
        assert!(terminate_suspended(&current).unwrap());
    }

    #[test]
    fn old_endpoint_cannot_serve_replacement_execution_or_replay_its_request() {
        let temp = tempfile::tempdir().unwrap();
        let (_, template) = fixture(temp.path());
        let mut current = template.clone();
        current.active_execution = "execution-replacement".into();
        current.active_stage = temp.path().join("executions/new");
        // Even a caller naming the new durable target cannot use the old server's controls.
        let request = ControlRequest {
            run_id: current.run_id.clone(),
            execution_id: current.active_execution.clone(),
            request_id: "capture".into(),
            suspend: false,
            ram_storage: SnapshotRamStorage::Raw,
        };
        assert_eq!(
            validate_capture_binding(&template, &current, &request)
                .unwrap_err()
                .code,
            AgentCtlHostErrorCode::Conflict
        );
        let old_request = ControlRequest {
            execution_id: template.active_execution.clone(),
            ..request
        };
        assert!(validate_capture_binding(&current, &current, &old_request).is_err());
        assert!(current.requests.is_empty());
    }

    fn remote_endpoint(record: &mut RunRecord) -> (tokio::net::UnixListener, tempfile::TempDir) {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let directory =
            super::super::host_transport::allocate_host_directory("capture-test-").unwrap();
        let path = directory.path().join("control.sock");
        let listener = super::super::host_transport::bind_instance_socket(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        record.live_control = Some(super::super::host_transport::LiveControlRecord {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            protocol: super::super::host_transport::INSTANCE_CONTROL_PROTOCOL.into(),
            binary: std::env::current_exe().unwrap(),
            binary_digest: "test".into(),
            pid: std::os::unix::process::parent_id(),
        });
        record.write().unwrap();
        (
            tokio::net::UnixListener::from_std(listener).unwrap(),
            directory,
        )
    }

    #[tokio::test]
    async fn capture_client_rejects_uncorrelated_or_wrong_version_acknowledgement() {
        let temp = tempfile::tempdir().unwrap();
        let (mut record, mut session) = fixture(temp.path());
        let mut stored = serde_json::to_value(&session).unwrap();
        stored["state"] = serde_json::to_value(SessionState::Running).unwrap();
        session = serde_json::from_value(stored).unwrap();
        session.write().unwrap();
        for variant in 0..4 {
            let (listener, directory) = remote_endpoint(&mut record);
            if variant == 3 {
                record.live_control.as_mut().unwrap().pid = std::process::id();
                record.write().unwrap();
            }
            let mut checkpoint = checkpoint(temp.path());
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request: AgentCtlHostRequest<super::super::host_transport::InstanceCommand> =
                    read_host_frame(&mut stream).await.unwrap();
                assert_eq!(
                    request.target.unwrap().execution_id.as_deref(),
                    Some("execution-original")
                );
                if variant == 2 {
                    checkpoint.source_execution_id = "old-execution".into();
                }
                let response = AgentCtlHostResponse {
                    version: if variant == 0 {
                        99
                    } else {
                        AGENTCTL_HOST_VERSION
                    },
                    request_id: if variant == 1 {
                        "other".into()
                    } else {
                        request.request_id
                    },
                    result: Ok(super::super::session_service::CaptureResponse {
                        session_id: "session".into(),
                        request_id: "capture".into(),
                        suspended: false,
                        checkpoint,
                    }),
                };
                write_host_frame(&mut stream, &response).await.unwrap();
            });
            let error = capture(
                &super::super::session_service::ServiceContext::default(),
                &record,
                false,
                SnapshotRamStorage::Raw,
                Some("capture".into()),
                Duration::from_secs(1),
            )
            .await
            .unwrap_err();
            if variant == 3 {
                assert!(format!("{error:#}").contains("local Session owner unavailable"));
                assert!(!server.is_finished());
                server.abort();
                let _ = server.await;
            } else {
                server.await.unwrap();
            }
            drop(directory);
        }
    }

    #[tokio::test]
    async fn capture_keeps_the_acknowledgement_after_post_admission_cancellation() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let temp = tempfile::tempdir().unwrap();
        let (mut record, mut session) = fixture(temp.path());
        let mut stored = serde_json::to_value(&session).unwrap();
        stored["state"] = serde_json::to_value(SessionState::Running).unwrap();
        session = serde_json::from_value(stored).unwrap();
        session.write().unwrap();
        let (listener, _directory) = remote_endpoint(&mut record);
        let captured = checkpoint(temp.path());
        let expected = captured.snapshot_id.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let server_cancelled = cancelled.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: AgentCtlHostRequest<super::super::host_transport::InstanceCommand> =
                read_host_frame(&mut stream).await.unwrap();
            server_cancelled.store(true, Ordering::SeqCst);
            write_host_frame(
                &mut stream,
                &AgentCtlHostResponse {
                    version: AGENTCTL_HOST_VERSION,
                    request_id: request.request_id,
                    result: Ok(super::super::session_service::CaptureResponse {
                        session_id: "session".into(),
                        request_id: "admitted-capture".into(),
                        suspended: false,
                        checkpoint: captured,
                    }),
                },
            )
            .await
            .unwrap();
        });
        let check = || {
            ensure!(!cancelled.load(Ordering::SeqCst), "request cancelled");
            Ok(())
        };
        let context = super::super::session_service::ServiceContext {
            check_cancelled: Some(&check),
            ..Default::default()
        };
        let result = capture(
            &context,
            &record,
            false,
            SnapshotRamStorage::Raw,
            Some("admitted-capture".into()),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(result.snapshot_id, expected);
        assert!(cancelled.load(Ordering::SeqCst));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn request_replay_cannot_change_operation_or_ram_encoding() {
        let temp = tempfile::tempdir().unwrap();
        let (record, mut session) = fixture(temp.path());
        let checkpoint = checkpoint(temp.path());
        let id = checkpoint.snapshot_id.clone();
        let mut stored = serde_json::to_value(&session).unwrap();
        stored["state"] = serde_json::to_value(SessionState::Terminal).unwrap();
        session = serde_json::from_value(stored).unwrap();
        session.checkpoints.insert(
            id.clone(),
            Capture {
                checkpoint,
                branches: BTreeMap::new(),
            },
        );
        session.requests.insert(
            "capture".into(),
            Request {
                suspend: false,
                ram_storage: SnapshotRamStorage::Raw,
                checkpoint: Some(id.clone()),
                error: None,
            },
        );
        session.write().unwrap();
        let result = capture(
            &super::super::session_service::ServiceContext::default(),
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
            &super::super::session_service::ServiceContext::default(),
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
            &super::super::session_service::ServiceContext::default(),
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
