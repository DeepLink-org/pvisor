//! Execution completion commits the Bundle before terminal event publication.
//! The receipt binds the result to immutable Bundle bytes; Run/Session state is a projection.

use super::RunRecord;
#[cfg(test)]
use super::RunRecordState;
use anyhow::{Context, ensure};
use pvisor_core::{RunResult, RunState};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Mutex;

pub(crate) const VERSION: u16 = 2;
const FILENAME: &str = "completion.json";
const MAX_BYTES: u64 = 64 * 1024 * 1024;
// Reuse only pure semantic validation of exact bytes, never file or record checks.
static VALIDATED_COMPLETION: Mutex<Option<ValidatedCompletion>> = Mutex::new(None);

#[derive(Clone)]
struct ValidatedCompletion {
    key: (blake3::Hash, blake3::Hash),
    project_apply_scope: Option<super::session_service::ProjectApplyScope>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Completion {
    version: u16,
    pub(crate) result: RunResult,
    bundle_blake3: String,
}

pub(crate) fn read_file(path: &Path) -> anyhow::Result<Vec<u8>> {
    let mut profile = crate::startup_profile::Span::start("completion.read_file");
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
    profile.success(Some(bytes.len() as u64));
    Ok(bytes)
}

impl Completion {
    pub(crate) fn commit(record: &RunRecord, result: &RunResult) -> anyhow::Result<()> {
        if let Some(committed) = Self::load(record)? {
            ensure!(
                serde_json::to_value(&committed.result)? == serde_json::to_value(result)?,
                "Execution completion is already committed with a different result"
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
        let mut profile = crate::startup_profile::Span::start("completion.load");
        let bytes = match read_file(&record.stage_dir().join(FILENAME)) {
            Ok(bytes) => bytes,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                profile.success(None);
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let receipt: Self = serde_json::from_slice(&bytes)?;
        receipt.validate(record)?;
        let bundle = read_file(&super::bundle::RunBundle::path(&record.stage_dir()))?;
        let mut validation = crate::startup_profile::Span::start("completion.validate_bundle");
        let bundle_hash = blake3::hash(&bundle);
        ensure!(
            bundle_hash.to_hex().as_str() == receipt.bundle_blake3,
            "completion Bundle digest mismatch"
        );
        let key = (blake3::hash(&bytes), bundle_hash);
        let validated = VALIDATED_COMPLETION
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let scope = match validated {
            Some(validated) if validated.key == key => validated.project_apply_scope,
            _ => {
                let bundle: super::bundle::RunBundle = serde_json::from_slice(&bundle)?;
                receipt.validate_bundle(&bundle)?;
                let scope = bundle.project_apply_scope;
                *VALIDATED_COMPLETION
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(ValidatedCompletion {
                    key,
                    project_apply_scope: scope.clone(),
                });
                scope
            }
        };
        // Only the exact Bundle bytes' scope is memoized. Compare the current
        // record on every load, including removal or insertion of authority.
        ensure!(
            record.project_apply_scope == scope,
            "project apply scope contradicts completion Bundle"
        );
        validation.success(None);
        profile.success(None);
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
                && bundle.run.execution_id == self.result.execution_id.as_str()
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
                && Some(self.result.execution_id.as_str()) == record.execution_id.as_deref(),
            "completion Execution identity mismatch"
        );
        super::execution::record_state(self.result.state)?;
        if self.result.state == RunState::Hibernated {
            pvisor_core::operation::ExecutionSuspension::from_result(&self.result)?;
        }
        Ok(())
    }

    /// Load and validate receipt bytes, then apply the single execution projection.
    pub(crate) fn observe_record(record: &mut RunRecord) -> anyhow::Result<Option<Self>> {
        if record.completion_protocol.is_none() {
            return Ok(None);
        }
        let receipt = Self::load(record)?;
        super::execution::project_record(record, receipt.as_ref().map(|value| &value.result))?;
        Ok(receipt)
    }
}

/// Called only under both the Session metadata lock (if present) and Execution lease.
pub(crate) fn reconcile(record: &mut RunRecord) -> anyhow::Result<()> {
    if record.completion_protocol.is_none() {
        return Ok(());
    }
    let previous = serde_json::to_value(&*record)?;
    let completion =
        Completion::observe_record(record).context("read committed Execution completion")?;
    if let Some(mut session) = crate::Session::read(record)? {
        let changed = session.apply(super::execution::SessionTransition::CompletionObserved {
            record,
            result: completion.as_ref().map(|receipt| &receipt.result),
        })?;
        if changed {
            session.write()?;
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
            "schema_version":3,"completion_protocol":VERSION,"run_id":"run-completion",
            "execution_id":"execution-completion","session_id":"run-completion","agent":"test",
            "pid":0,"command":["true"],"state":"running","started_at_unix_ms":1,
            "finished_at_unix_ms":null,"storage":root,"gateway_listen":null,
            "network":{},"overlay":null
        }))
        .unwrap();
        record.write().unwrap();
        let result: RunResult = serde_json::from_value(serde_json::json!({
            "run_id":record.run_id,"execution_id":record.execution_id,"state":"completed",
            "started_at_unix_ms":1,"finished_at_unix_ms":2,"exit_code":0
        }))
        .unwrap();
        let agentctl = serde_json::from_value(serde_json::json!({
            "run_id":record.run_id,"execution_id":record.execution_id,
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
    fn record_reads_and_locked_reconciliation_share_completion_projection() {
        for committed in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (record, result) = fixture(root.path());
            if committed {
                Completion::commit(&record, &result).unwrap();
            }
            let read = RunRecord::read(root.path()).unwrap();
            let mut reconciled = record;
            reconcile(&mut reconciled).unwrap();
            assert_eq!(
                serde_json::to_value(read).unwrap(),
                serde_json::to_value(reconciled).unwrap()
            );
        }
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
    fn committed_result_recovers_stale_record_and_rejects_changed_bundle_or_execution() {
        let root = tempfile::tempdir().unwrap();
        let (record, result) = fixture(root.path());
        Completion::commit(&record, &result).unwrap();
        let recovered = RunRecord::read(root.path()).unwrap();
        assert_eq!(recovered.state, RunRecordState::Completed);
        assert_eq!(recovered.finished_at_unix_ms, Some(2));
        RunBundle::read(root.path()).unwrap();
        let mut foreign = record.clone();
        foreign.execution_id = Some("foreign-execution".into());
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
    fn repeated_reads_revalidate_changed_receipt_and_matching_digest_bundle() {
        let root = tempfile::tempdir().unwrap();
        let (record, result) = fixture(root.path());
        Completion::commit(&record, &result).unwrap();
        let receipt_path = root.path().join(FILENAME);
        let bundle_path = RunBundle::path(root.path());
        let receipt_bytes = std::fs::read(&receipt_path).unwrap();
        let bundle_bytes = std::fs::read(&bundle_path).unwrap();
        for _ in 0..2 {
            Completion::load(&record).unwrap().unwrap();
        }

        let mut receipt: serde_json::Value = serde_json::from_slice(&receipt_bytes).unwrap();
        receipt["result"]["state"] = serde_json::json!("failed");
        std::fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        assert!(
            Completion::load(&record)
                .unwrap_err()
                .to_string()
                .contains("contradicts")
        );
        std::fs::write(&receipt_path, &receipt_bytes).unwrap();
        Completion::load(&record).unwrap().unwrap();

        let mut bundle: serde_json::Value = serde_json::from_slice(&bundle_bytes).unwrap();
        bundle["safety"]["network_non_bypassable"] = serde_json::json!(
            !bundle["safety"]["network_non_bypassable"]
                .as_bool()
                .unwrap()
        );
        let changed = serde_json::to_vec(&bundle).unwrap();
        let mut receipt: serde_json::Value = serde_json::from_slice(&receipt_bytes).unwrap();
        receipt["bundle_blake3"] = serde_json::json!(blake3::hash(&changed).to_hex().as_str());
        std::fs::write(&bundle_path, changed).unwrap();
        std::fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        for _ in 0..2 {
            assert!(
                Completion::load(&record)
                    .unwrap_err()
                    .to_string()
                    .contains("safety summary")
            );
        }
        std::fs::write(&bundle_path, &bundle_bytes).unwrap();
        std::fs::write(&receipt_path, &receipt_bytes).unwrap();
        Completion::load(&record).unwrap().unwrap();
    }

    #[test]
    fn repeated_reads_recheck_current_project_scope_against_bound_bundle() {
        let root = tempfile::tempdir().unwrap();
        let (mut record, result) = fixture(root.path());
        let scope = super::super::session_service::ProjectApplyScope::capture(root.path()).unwrap();
        for bound_scope in [None, Some(scope.clone())] {
            record.project_apply_scope = bound_scope.clone();
            record.write().unwrap();
            let path = RunBundle::path(root.path());
            let mut bundle: RunBundle =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            bundle.project_apply_scope = bound_scope.clone();
            let bytes = serde_json::to_vec(&bundle).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            let receipt = Completion {
                version: VERSION,
                result: result.clone(),
                bundle_blake3: blake3::hash(&bytes).to_hex().to_string(),
            };
            crate::util::write_private_json(&root.path().join(FILENAME), &receipt).unwrap();
            Completion::load(&record).unwrap().unwrap();
            let mut changed = scope.clone();
            changed.inode += 1;
            for current_scope in [None, Some(scope.clone()), Some(changed)] {
                record.project_apply_scope = current_scope.clone();
                record.write().unwrap();
                for _ in 0..2 {
                    let loaded = Completion::load(&record);
                    if current_scope == bound_scope {
                        loaded.unwrap().unwrap();
                        RunRecord::read(root.path()).unwrap();
                    } else {
                        assert!(
                            loaded
                                .unwrap_err()
                                .to_string()
                                .contains("project apply scope")
                        );
                        assert!(RunRecord::read(root.path()).is_err());
                    }
                }
            }
        }
    }

    #[test]
    fn repeated_reads_still_open_current_files_and_validate_record_identity() {
        let root = tempfile::tempdir().unwrap();
        let (mut record, result) = fixture(root.path());
        Completion::commit(&record, &result).unwrap();
        Completion::load(&record).unwrap().unwrap();
        record.execution_id = Some("foreign-execution".into());
        assert!(Completion::load(&record).is_err());
        record.execution_id = Some(result.execution_id.to_string());

        for path in [RunBundle::path(root.path()), root.path().join(FILENAME)] {
            let moved = path.with_extension("original");
            std::fs::rename(&path, &moved).unwrap();
            std::os::unix::fs::symlink(&moved, &path).unwrap();
            assert!(Completion::load(&record).is_err());
            std::fs::remove_file(&path).unwrap();
            std::fs::rename(moved, path).unwrap();
            Completion::load(&record).unwrap().unwrap();
        }
        std::fs::remove_file(root.path().join(FILENAME)).unwrap();
        assert!(Completion::load(&record).unwrap().is_none());
    }

    #[test]
    fn live_execution_without_completion_is_not_reconciled_as_dead() {
        let root = tempfile::tempdir().unwrap();
        let (_, _) = fixture(root.path());
        let _lease = super::super::registry::RunLease::acquire(root.path()).unwrap();
        assert_eq!(
            RunRecord::read(root.path()).unwrap().state,
            RunRecordState::Running
        );
    }

    #[test]
    fn terminal_projection_with_a_live_writer_still_requires_a_receipt() {
        let root = tempfile::tempdir().unwrap();
        let (mut record, result) = fixture(root.path());
        let _lease = super::super::registry::RunLease::acquire(root.path()).unwrap();
        record.state = RunRecordState::Completed;
        record.finished_at_unix_ms = Some(result.finished_at_unix_ms);
        record.write().unwrap();
        assert_eq!(
            RunRecord::read(root.path()).unwrap().state,
            RunRecordState::Unknown
        );
        Completion::commit(&record, &result).unwrap();
        assert_eq!(
            RunRecord::read(root.path()).unwrap().state,
            RunRecordState::Completed
        );
    }

    #[test]
    fn status_reconciles_session_state_from_the_receipt_or_marks_it_unknown() {
        use super::super::session_execution::SESSION_SCHEMA_VERSION;
        use crate::session_service::{
            ServiceContext, SessionSelection, SessionService, StatusRequest,
        };
        use crate::{Session, SessionState};
        use std::fs;
        for committed in [false, true] {
            let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
            let (record, result) = fixture(root.path());
            let mut config = crate::RunConfig::default();
            config.run.executor = crate::RunExecutorKind::Vm;
            let session: Session = serde_json::from_value(serde_json::json!({
                "version":SESSION_SCHEMA_VERSION,"run_id":record.run_id,"root":root.path(),
                "active_stage":root.path(),"previous_stage":root.path(),"active_execution":record.execution_id,
                "config":config,"spec":pvisor_core::RunSpec::process("run-completion", "test", "true"),
                "state":"running","head":null,"checkpoints":{},"requests":{},"resumes":{},"forks":{},"stores":[]
            })).unwrap();
            session.write().unwrap();
            let session_path = root.path().join("execution-session.json");
            let receipt_path = root.path().join(FILENAME);
            let bundle_path = RunBundle::path(root.path());
            let bundle_bytes = fs::read(&bundle_path).unwrap();
            if committed {
                Completion::commit(&record, &result).unwrap();
                let receipt_bytes = fs::read(&receipt_path).unwrap();
                let mut completed = session.clone();
                completed
                    .apply(super::super::execution::SessionTransition::Completed(
                        &result,
                    ))
                    .unwrap();
                // Exercise finish's private publication step after its transition,
                // without a production hook between Session reread and write.
                let saved = root.path().join("session-before-completion.json");
                fs::rename(&session_path, &saved).unwrap();
                fs::create_dir(&session_path).unwrap();
                fs::write(session_path.join("owned-sentinel"), b"obstruction").unwrap();
                let error = completed.write().unwrap_err();
                assert!(
                    error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.raw_os_error() == Some(libc::EISDIR)),
                    "{error:#}"
                );
                assert_eq!(
                    fs::read(session_path.join("owned-sentinel")).unwrap(),
                    b"obstruction"
                );
                assert!(Session::read(&record).is_err());
                // Root selectors consult Session metadata first and fail closed
                // while it is unreadable, rather than bypassing the obstruction.
                assert!(RunRecord::read(root.path()).is_err());
                assert_eq!(fs::read(&receipt_path).unwrap(), receipt_bytes);
                assert_eq!(fs::read(&bundle_path).unwrap(), bundle_bytes);
                assert!(!fs::read_dir(root.path()).unwrap().any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".pvisor.")
                }));
                fs::remove_file(session_path.join("owned-sentinel")).unwrap();
                fs::remove_dir(&session_path).unwrap();
                fs::rename(saved, &session_path).unwrap();
                assert_eq!(
                    RunRecord::read(root.path()).unwrap().state,
                    RunRecordState::Completed
                );
                RunBundle::read(root.path()).unwrap();
                assert_eq!(
                    Session::read(&record).unwrap().unwrap().state(),
                    SessionState::Running
                );
            }
            let receipt_bytes = committed.then(|| fs::read(&receipt_path).unwrap());
            for _ in 0..2 {
                let status = SessionService::status(
                    &ServiceContext::default(),
                    StatusRequest {
                        session: SessionSelection {
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
                    Session::read(&record).unwrap().unwrap().state(),
                    if committed {
                        SessionState::Terminal
                    } else {
                        SessionState::Unknown
                    }
                );
            }
            if let Some(receipt_bytes) = receipt_bytes {
                let mut successor = Session::read(&record).unwrap().unwrap();
                let mut stored = serde_json::to_value(&successor).unwrap();
                stored["active_stage"] =
                    serde_json::json!(root.path().join("executions/successor"));
                stored["previous_stage"] = serde_json::json!(root.path());
                for state in [SessionState::Restoring, SessionState::Running] {
                    stored["state"] = serde_json::to_value(state).unwrap();
                    if state == SessionState::Running {
                        stored["active_execution"] = serde_json::json!("execution-successor");
                    }
                    successor = serde_json::from_value(stored.clone()).unwrap();
                    successor.write().unwrap();
                    let before = fs::read(&session_path).unwrap();
                    let _session = successor.lock().unwrap();
                    let _execution =
                        super::super::registry::RunLease::acquire(root.path()).unwrap();
                    for _ in 0..2 {
                        let mut original = record.clone();
                        reconcile(&mut original).unwrap();
                        assert_eq!(original.state, RunRecordState::Completed);
                        assert_eq!(fs::read(&session_path).unwrap(), before);
                        assert_eq!(fs::read(&receipt_path).unwrap(), receipt_bytes);
                        assert_eq!(fs::read(&bundle_path).unwrap(), bundle_bytes);
                    }
                }
            }
        }
    }
}
