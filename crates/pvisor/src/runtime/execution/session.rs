//! Persistent Session transitions across Executions; storage and lease acquisition stay outside.
use super::super::{
    RunRecord,
    session_execution::{Capture, ForkRequest, Request, ResumeRequest},
};
use anyhow::{Context, ensure};
use pvisor_core::operation::{ExecutionCheckpoint, ExecutionSuspension, SnapshotRamStorage};
use pvisor_core::{RunResult, RunSpec, RunState};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// Durable continuation phase of a Session, independent of an Execution's live pause state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// The selected Execution owns live execution.
    Running,
    /// Suspension was admitted; native termination is not yet confirmed.
    Suspending,
    /// A completion receipt grants continuation from the sealed head.
    Suspended,
    /// A successor was admitted but has not confirmed startup.
    Restoring,
    /// Execution ended or continuation rights were explicitly withdrawn.
    Terminal,
    /// An admitted outcome lacks sufficient completion evidence.
    Unknown,
}
impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Running => "running",
            Self::Suspending => "suspending",
            Self::Suspended => "suspended",
            Self::Restoring => "restoring",
            Self::Terminal => "terminal",
            Self::Unknown => "unknown",
        })
    }
}

/// Persistent execution aggregate. State changes require typed transitions; callers
/// retain the Session lease through applying a transition and writing the resulting record.
/// Deserialization preserves the existing schema; reading storage validates its binding.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    /// Durable record schema version.
    pub version: u16,
    /// Logical Session identity shared by its Executions.
    pub run_id: String,
    /// Canonical Session storage root.
    pub root: PathBuf,
    /// Stage selected for the current or admitted successor Execution.
    pub active_stage: PathBuf,
    /// Previous stage retained during successor admission.
    pub previous_stage: PathBuf,
    /// Execution identity selected by confirmed startup.
    pub active_execution: String,
    /// Captured execution configuration, not frontend override authority.
    pub config: crate::RunConfig,
    /// Captured invocation and policy inputs for continuation.
    pub spec: RunSpec,
    state: SessionState,
    /// Confirmed suspended checkpoint; usable only in Suspended phase.
    pub head: Option<String>,
    /// Published checkpoints and their retained file branches.
    pub checkpoints: BTreeMap<String, Capture>,
    /// Scoped capture admission and result receipts.
    pub requests: BTreeMap<String, Request>,
    /// Scoped successor admission receipts.
    pub resumes: BTreeMap<String, ResumeRequest>,
    /// Scoped execution-fork receipts.
    pub forks: BTreeMap<String, ForkRequest>,
    /// Snapshot stores pinned by this Session's records.
    pub stores: BTreeSet<PathBuf>,
}

pub(crate) enum SessionTransition<'a> {
    ExecutionStarted {
        record: &'a RunRecord,
    },
    CaptureAdmitted {
        id: &'a str,
        suspend: bool,
        ram_storage: SnapshotRamStorage,
    },
    CaptureAcknowledged {
        id: &'a str,
        checkpoint: &'a ExecutionCheckpoint,
    },
    CaptureRejected {
        id: &'a str,
        message: String,
    },
    RestoreAdmitted {
        id: &'a str,
        stage: &'a Path,
        eager_ram: bool,
    },
    RestoreAborted {
        admitted: &'a Session,
        previous: &'a Session,
        id: &'a str,
        no_successor: bool,
    },
    Completed(&'a RunResult),
    CompletionObserved {
        record: &'a RunRecord,
        result: Option<&'a RunResult>,
    },
    Terminated,
}
impl Session {
    /// Read the durable continuation phase. A live pause is reported by Execution.
    pub fn state(&self) -> SessionState {
        self.state
    }

    pub(crate) fn new(
        run_id: String,
        stage: PathBuf,
        execution: Option<String>,
        config: crate::RunConfig,
        spec: RunSpec,
    ) -> anyhow::Result<Self> {
        ensure!(
            !run_id.is_empty() && stage.is_absolute(),
            "invalid Session identity or stage"
        );
        ensure!(
            execution.as_ref().is_none_or(|id| !id.is_empty()),
            "missing Execution identity"
        );
        let state = if execution.is_some() {
            SessionState::Running
        } else {
            SessionState::Restoring
        };
        Ok(Self {
            version: super::super::session_execution::SESSION_SCHEMA_VERSION,
            run_id,
            root: stage.clone(),
            active_stage: stage.clone(),
            previous_stage: stage,
            active_execution: execution.unwrap_or_default(),
            config,
            spec,
            state,
            head: None,
            checkpoints: BTreeMap::new(),
            requests: BTreeMap::new(),
            resumes: BTreeMap::new(),
            forks: BTreeMap::new(),
            stores: BTreeSet::new(),
        })
    }

    pub(crate) fn owns_restore_transition(&self, admitted: &Self, id: &str) -> bool {
        self.state == SessionState::Restoring
            && matches!((serde_json::to_value(self), serde_json::to_value(admitted)), (Ok(a), Ok(b)) if a == b)
            && self
                .resumes
                .get(id)
                .is_some_and(|request| request.stage == admitted.active_stage)
    }

    // Each arm validates before mutation so rejected transitions leave no partial changes.
    pub(crate) fn apply(&mut self, input: SessionTransition<'_>) -> anyhow::Result<bool> {
        match input {
            SessionTransition::ExecutionStarted { record } => {
                ensure!(
                    self.run_id == record.run_id
                        && self.active_stage == record.stage_dir()
                        && self.state == SessionState::Restoring,
                    "invalid resumed Session ownership"
                );
                self.active_execution = record
                    .execution_id
                    .clone()
                    .context("missing Execution identity")?;
                self.state = SessionState::Running;
                self.head = None;
            }
            SessionTransition::CaptureAdmitted {
                id,
                suspend,
                ram_storage,
            } => {
                ensure!(
                    self.state == SessionState::Running,
                    "SESSION_BUSY: execution state is {}",
                    self.state
                );
                ensure!(
                    !id.trim().is_empty() && id.len() <= 256 && !self.requests.contains_key(id),
                    "duplicate or invalid capture admission"
                );
                self.requests.insert(
                    id.into(),
                    Request {
                        suspend,
                        ram_storage,
                        checkpoint: None,
                        error: None,
                    },
                );
                if suspend {
                    self.state = SessionState::Suspending;
                }
            }
            SessionTransition::CaptureAcknowledged { id, checkpoint } => {
                checkpoint.validate()?;
                ensure!(
                    checkpoint.source_run_id == self.run_id
                        && checkpoint.source_execution_id == self.active_execution,
                    "native capture owner mismatch"
                );
                let request = self.requests.get_mut(id).context("request disappeared")?;
                ensure!(
                    request.ram_storage == checkpoint.ram_storage,
                    "capture RAM storage mismatch"
                );
                request.checkpoint = Some(checkpoint.snapshot_id.clone());
                self.checkpoints
                    .entry(checkpoint.snapshot_id.clone())
                    .or_insert(Capture {
                        checkpoint: checkpoint.clone(),
                        branches: BTreeMap::new(),
                    });
            }
            SessionTransition::CaptureRejected { id, message } => {
                let request = self.requests.get_mut(id).context("request disappeared")?;
                request.error = Some(message);
                if request.suspend && self.state == SessionState::Suspending {
                    self.state = SessionState::Unknown;
                }
            }
            SessionTransition::RestoreAdmitted {
                id,
                stage,
                eager_ram,
            } => {
                ensure!(
                    self.state == SessionState::Suspended && self.head.is_some(),
                    "SESSION_BUSY: resume requires a confirmed suspended head (state={})",
                    self.state
                );
                ensure!(
                    !id.trim().is_empty() && id.len() <= 256 && !self.resumes.contains_key(id),
                    "duplicate or invalid restore admission"
                );
                ensure!(
                    stage.starts_with(self.root.join("executions")),
                    "invalid successor stage"
                );
                self.previous_stage = self.active_stage.clone();
                self.active_stage = stage.to_path_buf();
                self.resumes.insert(
                    id.into(),
                    ResumeRequest {
                        stage: stage.to_path_buf(),
                        eager_ram,
                    },
                );
                self.state = SessionState::Restoring;
            }
            SessionTransition::RestoreAborted {
                admitted,
                previous,
                id,
                no_successor,
            } => {
                if !no_successor || !self.owns_restore_transition(admitted, id) {
                    return Ok(false);
                }
                ensure!(
                    previous.run_id == self.run_id
                        && previous.root == self.root
                        && previous.active_stage == self.previous_stage
                        && previous.active_execution == admitted.active_execution
                        && previous.head == admitted.head
                        && previous.state == SessionState::Suspended,
                    "invalid restore rollback"
                );
                *self = previous.clone();
            }
            SessionTransition::Completed(result) => self.complete(result)?,
            SessionTransition::CompletionObserved { record, result } => {
                if self.active_stage != record.stage_dir() {
                    return Ok(false);
                }
                self.validate_record_target(record)?;
                match result {
                    Some(result)
                        if matches!(
                            self.state,
                            SessionState::Running
                                | SessionState::Suspending
                                | SessionState::Unknown
                        ) =>
                    {
                        self.complete(result)?
                    }
                    Some(_) => return Ok(false),
                    None if self.state == SessionState::Unknown => return Ok(false),
                    None => self.state = SessionState::Unknown,
                }
            }
            SessionTransition::Terminated => {
                ensure!(
                    self.state == SessionState::Suspended,
                    "Session has no suspended continuation"
                );
                self.head = None;
                self.state = SessionState::Terminal;
            }
        }
        Ok(true)
    }
    fn complete(&mut self, result: &RunResult) -> anyhow::Result<()> {
        ensure!(
            result.state.is_terminal(),
            "cannot finish a Session with a nonterminal result"
        );
        ensure!(
            self.run_id == result.run_id.as_str()
                && self.active_execution == result.execution_id.as_str(),
            "terminal Execution identity mismatch"
        );
        if result.state == RunState::Hibernated {
            let receipt = ExecutionSuspension::from_result(result)?;
            let id = receipt.checkpoint.snapshot_id.clone();
            self.requests
                .get_mut(&receipt.request_id)
                .context("suspend receipt has no Session request")?
                .checkpoint = Some(id.clone());
            self.checkpoints.entry(id.clone()).or_insert(Capture {
                checkpoint: receipt.checkpoint,
                branches: BTreeMap::new(),
            });
            self.head = Some(id);
            self.state = SessionState::Suspended;
        } else {
            self.state = SessionState::Terminal;
            self.head = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (
        tempfile::TempDir,
        Session,
        RunRecord,
        ExecutionCheckpoint,
        RunResult,
    ) {
        let root = tempfile::tempdir().unwrap();
        let session = Session::new(
            "session".into(),
            root.path().into(),
            Some("execution".into()),
            crate::RunConfig::default(),
            RunSpec::process("session", "test", "true"),
        )
        .unwrap();
        let record = serde_json::from_value(serde_json::json!({
            "schema_version":1,"run_id":"session","execution_id":"execution","session_id":"session","agent":"test","pid":0,"command":["true"],"state":"running","storage":root.path(),"started_at_unix_ms":1,"network":{},"gateway_listen":null,"overlay":null
        })).unwrap();
        let checkpoint = ExecutionCheckpoint {
            snapshot_id: "a".repeat(64),
            store: root.path().join("snapshots"),
            source_run_id: "session".into(),
            source_execution_id: "execution".into(),
            created_at_unix_ms: 2,
            ram_storage: SnapshotRamStorage::Raw,
        };
        let result = serde_json::from_value(serde_json::json!({
            "run_id":"session","execution_id":"execution","state":"hibernated","started_at_unix_ms":1,"finished_at_unix_ms":3,"value":{"request_id":"suspend","checkpoint":checkpoint}
        })).unwrap();
        (root, session, record, checkpoint, result)
    }
    fn suspend(session: &mut Session, checkpoint: &ExecutionCheckpoint, result: &RunResult) {
        session
            .apply(SessionTransition::CaptureAdmitted {
                id: "suspend",
                suspend: true,
                ram_storage: SnapshotRamStorage::Raw,
            })
            .unwrap();
        assert_eq!(session.state(), SessionState::Suspending);
        session
            .apply(SessionTransition::CaptureAcknowledged {
                id: "suspend",
                checkpoint,
            })
            .unwrap();
        assert_eq!(session.state(), SessionState::Suspending);
        assert!(session.head.is_none());
        session.apply(SessionTransition::Completed(result)).unwrap();
        assert_eq!(session.state(), SessionState::Suspended);
    }
    #[test]
    fn termination_withdraws_continuation_without_erasing_history_or_reviving_old_receipts() {
        let (_root, mut session, record, checkpoint, result) = fixture();
        suspend(&mut session, &checkpoint, &result);
        session.apply(SessionTransition::Terminated).unwrap();
        let before = serde_json::to_value(&session).unwrap();
        assert!(
            !session
                .apply(SessionTransition::CompletionObserved {
                    record: &record,
                    result: Some(&result)
                })
                .unwrap()
        );
        assert_eq!(before, serde_json::to_value(&session).unwrap());
        assert_eq!(session.state(), SessionState::Terminal);
        assert!(session.head.is_none());
        assert_eq!(session.checkpoints.len(), 1);
        assert!(session.requests["suspend"].checkpoint.is_some());
    }
    #[test]
    fn restore_rolls_back_only_the_unchanged_admission_without_a_successor() {
        let (root, mut session, record, checkpoint, result) = fixture();
        suspend(&mut session, &checkpoint, &result);
        let previous = session.clone();
        let stage = root.path().join("executions/next");
        session
            .apply(SessionTransition::RestoreAdmitted {
                id: "resume",
                stage: &stage,
                eager_ram: false,
            })
            .unwrap();
        let admitted = session.clone();
        assert!(
            !session
                .apply(SessionTransition::CompletionObserved {
                    record: &record,
                    result: Some(&result)
                })
                .unwrap()
        );
        assert!(
            !session
                .apply(SessionTransition::RestoreAborted {
                    admitted: &admitted,
                    previous: &previous,
                    id: "resume",
                    no_successor: false
                })
                .unwrap()
        );
        assert!(
            session
                .apply(SessionTransition::RestoreAborted {
                    admitted: &admitted,
                    previous: &previous,
                    id: "resume",
                    no_successor: true
                })
                .unwrap()
        );
        assert_eq!(
            serde_json::to_value(&session).unwrap(),
            serde_json::to_value(&previous).unwrap()
        );
        session
            .apply(SessionTransition::RestoreAdmitted {
                id: "resume",
                stage: &stage,
                eager_ram: false,
            })
            .unwrap();
        let mut successor = record;
        successor.storage = stage;
        successor.execution_id = Some("successor".into());
        session
            .apply(SessionTransition::ExecutionStarted { record: &successor })
            .unwrap();
        assert!(
            !session
                .apply(SessionTransition::RestoreAborted {
                    admitted: &admitted,
                    previous: &previous,
                    id: "resume",
                    no_successor: true
                })
                .unwrap()
        );
        assert_eq!(session.state(), SessionState::Running);
        assert_eq!(session.active_execution, "successor");
    }
    #[test]
    fn missing_receipt_recovery_is_idempotent_and_can_later_confirm_completion() {
        let (_root, mut session, record, _checkpoint, mut result) = fixture();
        assert!(
            session
                .apply(SessionTransition::CompletionObserved {
                    record: &record,
                    result: None
                })
                .unwrap()
        );
        assert_eq!(session.state(), SessionState::Unknown);
        assert!(
            !session
                .apply(SessionTransition::CompletionObserved {
                    record: &record,
                    result: None
                })
                .unwrap()
        );
        result.state = RunState::Completed;
        result.value = None;
        result.exit_code = Some(0);
        assert!(
            session
                .apply(SessionTransition::CompletionObserved {
                    record: &record,
                    result: Some(&result)
                })
                .unwrap()
        );
        assert_eq!(session.state(), SessionState::Terminal);
        assert!(
            !session
                .apply(SessionTransition::CompletionObserved {
                    record: &record,
                    result: Some(&result)
                })
                .unwrap()
        );
    }

    #[test]
    fn invalid_receipts_and_duplicate_admissions_leave_the_aggregate_unchanged() {
        let (_root, mut session, _record, checkpoint, mut result) = fixture();
        let before = serde_json::to_value(&session).unwrap();
        assert!(
            session
                .apply(SessionTransition::Completed(&result))
                .is_err()
        ); // Missing scoped request.
        assert_eq!(before, serde_json::to_value(&session).unwrap());
        session
            .apply(SessionTransition::CaptureAdmitted {
                id: "suspend",
                suspend: true,
                ram_storage: SnapshotRamStorage::Raw,
            })
            .unwrap();
        let before = serde_json::to_value(&session).unwrap();
        assert!(
            session
                .apply(SessionTransition::CaptureAdmitted {
                    id: "suspend",
                    suspend: true,
                    ram_storage: SnapshotRamStorage::Raw
                })
                .is_err()
        );
        let mut invalid_owner = checkpoint.clone();
        invalid_owner.source_execution_id = "foreign".into();
        let mut invalid_storage = checkpoint.clone();
        invalid_storage.ram_storage = SnapshotRamStorage::Compressed;
        for invalid in [invalid_owner, invalid_storage] {
            assert!(
                session
                    .apply(SessionTransition::CaptureAcknowledged {
                        id: "suspend",
                        checkpoint: &invalid,
                    })
                    .is_err()
            );
            assert_eq!(before, serde_json::to_value(&session).unwrap());
        }
        result.execution_id = "foreign".into();
        assert!(
            session
                .apply(SessionTransition::Completed(&result))
                .is_err()
        );
        assert!(
            session
                .apply(SessionTransition::CaptureAcknowledged {
                    id: "missing",
                    checkpoint: &checkpoint
                })
                .is_err()
        );
        assert_eq!(before, serde_json::to_value(&session).unwrap());
    }
}
