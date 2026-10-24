//! Attempt completion commits the Bundle before terminal event publication.
//! The receipt binds the result to immutable Bundle bytes; Run/Job state is a projection.

use super::{RunRecord, RunRecordState};
use anyhow::{Context, ensure};
use pvisor_core::{RunResult, RunState};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub(crate) const VERSION: u16 = 1;
const FILENAME: &str = "completion.json";
const MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Completion {
    version: u16,
    pub(crate) result: RunResult,
    bundle_blake3: String,
}

pub(crate) fn read_file(path: &Path) -> anyhow::Result<Vec<u8>> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "completion data must be a regular file");
    ensure!(
        metadata.len() <= MAX_BYTES,
        "completion data exceeds size limit"
    );
    // Length is an allocation hint; the bounded read still catches later growth.
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "completion data exceeds size limit"
    );
    Ok(bytes)
}

impl Completion {
    pub(crate) fn commit(record: &RunRecord, result: &RunResult) -> anyhow::Result<()> {
        if let Some(committed) = Self::load(record)? {
            ensure!(
                serde_json::to_value(&committed.result)? == serde_json::to_value(result)?,
                "Attempt completion is already committed with a different result"
            );
            return Ok(());
        }
        let stage = record.stage_dir();
        let bundle_bytes = read_file(&super::bundle::RunBundle::path(&stage))?;
        let receipt = Self {
            version: VERSION,
            result: result.clone(),
            bundle_blake3: blake3::hash(&bundle_bytes).to_hex().to_string(),
        };
        receipt.validate(record)?;
        let bundle: super::bundle::RunBundle = serde_json::from_slice(&bundle_bytes)?;
        receipt.validate_bundle(&bundle)?;
        crate::util::write_private_json(&stage.join(FILENAME), &receipt)
    }

    pub(crate) fn load(record: &RunRecord) -> anyhow::Result<Option<Self>> {
        let bytes = match read_file(&record.stage_dir().join(FILENAME)) {
            Ok(bytes) => bytes,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let receipt: Self = serde_json::from_slice(&bytes)?;
        receipt.validate(record)?;
        let bundle = read_file(&super::bundle::RunBundle::path(&record.stage_dir()))?;
        ensure!(
            blake3::hash(&bundle).to_hex().as_str() == receipt.bundle_blake3,
            "completion Bundle digest mismatch"
        );
        let bundle: super::bundle::RunBundle = serde_json::from_slice(&bundle)?;
        receipt.validate_bundle(&bundle)?;
        Ok(Some(receipt))
    }

    fn validate_bundle(&self, bundle: &super::bundle::RunBundle) -> anyhow::Result<()> {
        bundle.validate()?;
        ensure!(
            bundle.schema_version == super::bundle::RUN_BUNDLE_SCHEMA_VERSION,
            "completion requires a protocol-aware Run Bundle"
        );
        ensure!(
            bundle.run.run_id == self.result.run_id.as_str()
                && bundle.run.attempt_id == self.result.attempt_id.as_str()
                && bundle.run.state == self.result.state
                && bundle.run.exit_code == self.result.exit_code
                && bundle.run.started_at_unix_ms == self.result.started_at_unix_ms
                && bundle.run.finished_at_unix_ms == self.result.finished_at_unix_ms,
            "completion result contradicts Bundle"
        );
        Ok(())
    }

    fn validate(&self, record: &RunRecord) -> anyhow::Result<()> {
        ensure!(
            self.version == VERSION && record.completion_protocol == Some(VERSION),
            "unsupported completion protocol"
        );
        ensure!(
            self.result.run_id.as_str() == record.run_id
                && Some(self.result.attempt_id.as_str()) == record.attempt_id.as_deref(),
            "completion Attempt identity mismatch"
        );
        record_state(self.result.state)?;
        if self.result.state == RunState::Hibernated {
            pvisor_core::operation::ExecutionSuspension::from_result(&self.result)?;
        }
        Ok(())
    }

    pub(crate) fn project(&self, record: &mut RunRecord) -> anyhow::Result<()> {
        record.state = record_state(self.result.state)?;
        record.finished_at_unix_ms = Some(self.result.finished_at_unix_ms);
        record.live_control = None;
        Ok(())
    }
}

fn record_state(state: RunState) -> anyhow::Result<RunRecordState> {
    Ok(match state {
        RunState::Completed => RunRecordState::Completed,
        RunState::Failed => RunRecordState::Failed,
        RunState::Cancelled => RunRecordState::Cancelled,
        RunState::Hibernated => RunRecordState::Hibernated,
        _ => anyhow::bail!("completion result is not terminal"),
    })
}

/// Called only under both the Job metadata lock (if present) and Attempt lease.
pub(crate) fn reconcile(record: &mut RunRecord) -> anyhow::Result<()> {
    if record.completion_protocol.is_none() {
        return Ok(());
    }
    let previous = serde_json::to_value(&*record)?;
    let completion = Completion::load(record).context("read committed Attempt completion")?;
    match &completion {
        Some(receipt) => receipt.project(record)?,
        None => {
            record.state = RunRecordState::Unknown;
            record.finished_at_unix_ms = None;
            record.live_control = None;
        }
    }
    if let Some(mut job) = super::job_execution::Job::read(record)? {
        // A Job can move beyond this Attempt (kill a suspended head or begin
        // restore). Completion must never resurrect those later transitions.
        if job.active_stage == record.stage_dir() {
            job.validate_record_target(record)?;
            match &completion {
                Some(receipt)
                    if matches!(
                        job.state,
                        super::job_execution::JobState::Running
                            | super::job_execution::JobState::Suspending
                            | super::job_execution::JobState::Unknown
                    ) =>
                {
                    job.finish_result(&receipt.result)?
                }
                Some(_) => {}
                None => {
                    if job.state != super::job_execution::JobState::Unknown {
                        job.state = super::job_execution::JobState::Unknown;
                        job.write()?;
                    }
                }
            }
        }
    }
    if serde_json::to_value(&*record)? != previous {
        record.write()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RunBundle;

    #[test]
    fn bounded_reader_preserves_bytes_and_rejects_oversized_or_nonregular_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("data");
        for size in [0, 653, 8502, 65536] {
            let contents: Vec<_> = (0..size).map(|index| (index % 251) as u8).collect();
            std::fs::write(&path, &contents).unwrap();
            assert_eq!(read_file(&path).unwrap(), contents);
        }
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        assert!(
            read_file(&path)
                .unwrap_err()
                .to_string()
                .contains("size limit")
        );
        std::fs::write(&path, b"regular file").unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_file(&link).is_err());
        assert!(read_file(root.path()).is_err());
        let fifo = root.path().join("fifo");
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(
            read_file(&fifo)
                .unwrap_err()
                .to_string()
                .contains("regular file")
        );
    }

    fn fixture(root: &Path) -> (RunRecord, RunResult) {
        let record: RunRecord = serde_json::from_value(serde_json::json!({
            "schema_version":2,"completion_protocol":VERSION,"run_id":"run-completion",
            "attempt_id":"attempt-completion","session_id":"run-completion","agent":"test",
            "pid":0,"command":["true"],"state":"running","started_at_unix_ms":1,
            "finished_at_unix_ms":null,"storage":root,"gateway_listen":null,
            "network":{},"overlay":null
        }))
        .unwrap();
        record.write().unwrap();
        let result: RunResult = serde_json::from_value(serde_json::json!({
            "run_id":record.run_id,"attempt_id":record.attempt_id,"state":"completed",
            "started_at_unix_ms":1,"finished_at_unix_ms":2,"exit_code":0
        }))
        .unwrap();
        let agentctl = serde_json::from_value(serde_json::json!({
            "run_id":record.run_id,"attempt_id":record.attempt_id,
            "directive":crate::AgentDirective::Continue,"clients":[]
        }))
        .unwrap();
        RunBundle::capture(&record, &result, agentctl, false)
            .unwrap()
            .write(root)
            .unwrap();
        (record, result)
    }

    #[test]
    fn committed_receipt_is_idempotent_and_cannot_be_replaced_by_failure() {
        let root = tempfile::tempdir().unwrap();
        let (record, mut result) = fixture(root.path());
        Completion::commit(&record, &result).unwrap();
        let bytes = std::fs::read(root.path().join(FILENAME)).unwrap();
        Completion::commit(&record, &result).unwrap();
        result.state = RunState::Failed;
        assert!(Completion::commit(&record, &result).is_err());
        assert_eq!(std::fs::read(root.path().join(FILENAME)).unwrap(), bytes);
        assert_eq!(
            RunBundle::read(root.path()).unwrap().run.state,
            RunState::Completed
        );
    }

    #[test]
    fn absent_commit_never_turns_a_bundle_or_terminal_record_into_success() {
        for state in [RunRecordState::Running, RunRecordState::Completed] {
            let root = tempfile::tempdir().unwrap();
            let (mut record, _) = fixture(root.path());
            record.state = state;
            record.write().unwrap();
            assert_eq!(
                RunRecord::read(root.path()).unwrap().state,
                RunRecordState::Unknown
            );
            assert!(RunBundle::read(root.path()).is_err());
            let (locked, _lease) = record.lock_current().unwrap();
            assert_eq!(locked.state, RunRecordState::Unknown);
            assert!(locked.require_stopped().is_err());
            reconcile(&mut record).unwrap();
            assert_eq!(record.state, RunRecordState::Unknown);
            assert!(record.require_stopped().is_err());
        }
    }

    #[test]
    fn committed_result_recovers_stale_record_and_rejects_changed_bundle_or_attempt() {
        let root = tempfile::tempdir().unwrap();
        let (record, result) = fixture(root.path());
        Completion::commit(&record, &result).unwrap();
        let recovered = RunRecord::read(root.path()).unwrap();
        assert_eq!(recovered.state, RunRecordState::Completed);
        assert_eq!(recovered.finished_at_unix_ms, Some(2));
        RunBundle::read(root.path()).unwrap();
        let mut foreign = record.clone();
        foreign.attempt_id = Some("foreign-attempt".into());
        assert!(Completion::load(&foreign).is_err());
        let path = RunBundle::path(root.path());
        let original = std::fs::read(&path).unwrap();
        let mut changed = original.clone();
        changed.push(b'\n');
        std::fs::write(&path, changed).unwrap();
        assert!(
            Completion::load(&record)
                .unwrap_err()
                .to_string()
                .contains("digest")
        );
        std::fs::write(path, original).unwrap();
        assert!(Completion::load(&record).unwrap().is_some());
    }

    #[test]
    fn protocol_bundle_requires_its_binding_and_rejects_schema_downgrade() {
        let root = tempfile::tempdir().unwrap();
        let (record, result) = fixture(root.path());
        Completion::commit(&record, &result).unwrap();
        RunBundle::read(root.path()).unwrap();
        let record_path = root.path().join(super::super::registry::RUN_META_FILENAME);
        std::fs::remove_file(&record_path).unwrap();
        assert!(RunBundle::read(root.path()).is_err());
        let mut legacy = record.clone();
        legacy.schema_version = 1;
        legacy.completion_protocol = None;
        legacy.write().unwrap();
        assert!(RunBundle::read(root.path()).is_err());
        record.write().unwrap();
        let path = RunBundle::path(root.path());
        let mut bundle: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        bundle["schema_version"] = serde_json::json!(4);
        std::fs::write(&path, serde_json::to_vec(&bundle).unwrap()).unwrap();
        assert!(RunBundle::read(root.path()).is_err());
    }

    #[test]
    fn live_attempt_without_completion_is_not_reconciled_as_dead() {
        let root = tempfile::tempdir().unwrap();
        let (_, _) = fixture(root.path());
        let _lease = super::super::registry::RunLease::acquire(root.path()).unwrap();
        assert_eq!(
            RunRecord::read(root.path()).unwrap().state,
            RunRecordState::Running
        );
    }

    #[test]
    fn status_reconciles_job_state_from_the_receipt_or_marks_it_unknown() {
        use super::super::job_execution::{JOB_SCHEMA_VERSION, Job, JobState};
        use crate::job_service::{JobSelection, RuntimeJobService, ServiceContext, StatusRequest};
        for committed in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (record, result) = fixture(root.path());
            let mut config = crate::RunConfig::default();
            config.run.executor = crate::RunExecutorKind::Vm;
            let job: Job = serde_json::from_value(serde_json::json!({
                "version":JOB_SCHEMA_VERSION,"run_id":record.run_id,"root":root.path(),
                "active_stage":root.path(),"previous_stage":root.path(),"active_attempt":record.attempt_id,
                "config":config,"spec":pvisor_core::RunSpec::process("run-completion", "test", "true"),
                "state":"running","head":null,"checkpoints":{},"requests":{},"resumes":{},"forks":{},"stores":[]
            })).unwrap();
            job.write().unwrap();
            if committed {
                Completion::commit(&record, &result).unwrap();
            }
            for _ in 0..2 {
                let status = RuntimeJobService::status(
                    &ServiceContext::default(),
                    StatusRequest {
                        job: JobSelection {
                            selector: Some(root.path().into()),
                            storage: root.path().into(),
                        },
                    },
                )
                .unwrap();
                assert_eq!(
                    status.record.state,
                    if committed {
                        RunRecordState::Completed
                    } else {
                        RunRecordState::Unknown
                    }
                );
                assert_eq!(
                    Job::read(&record).unwrap().unwrap().state,
                    if committed {
                        JobState::Terminal
                    } else {
                        JobState::Unknown
                    }
                );
            }
            if committed {
                let mut successor = Job::read(&record).unwrap().unwrap();
                successor.state = JobState::Restoring;
                successor.active_stage = root.path().join("attempts/successor");
                successor.previous_stage = root.path().into();
                successor.write().unwrap();
                let _job = successor.lock().unwrap();
                let _attempt = super::super::registry::RunLease::acquire(root.path()).unwrap();
                let mut original = record.clone();
                reconcile(&mut original).unwrap();
                let unchanged = Job::read(&record).unwrap().unwrap();
                assert_eq!(unchanged.state, JobState::Restoring);
                assert_eq!(unchanged.active_stage, successor.active_stage);
            }
        }
    }
}
