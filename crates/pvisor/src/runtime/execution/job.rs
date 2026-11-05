//! Persistent Job transitions across Attempts; storage and lease acquisition stay outside.
use super::super::{
    RunRecord,
    job_execution::{Capture, ForkRequest, Request, ResumeRequest},
};
use anyhow::{Context, ensure};
use pvisor_core::operation::{ExecutionCheckpoint, ExecutionSuspension, SnapshotRamStorage};
use pvisor_core::{RunResult, RunSpec, RunState};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// Durable continuation phase of a Job, independent of an Attempt's live pause state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// The selected Attempt owns live execution.
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
impl std::fmt::Display for JobState {
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
/// retain the Job lease through applying a transition and writing the resulting record.
/// Deserialization preserves the existing schema; reading storage validates its binding.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobExecution {
    /// Durable record schema version.
    pub version: u16,
    /// Logical Job identity shared by its Attempts.
    pub run_id: String,
    /// Canonical Job storage root.
    pub root: PathBuf,
    /// Stage selected for the current or admitted successor Attempt.
    pub active_stage: PathBuf,
    /// Previous stage retained during successor admission.
    pub previous_stage: PathBuf,
    /// Attempt identity selected by confirmed startup.
    pub active_attempt: String,
    /// Captured execution configuration, not frontend override authority.
    pub config: crate::RunConfig,
    /// Captured invocation and policy inputs for continuation.
    pub spec: RunSpec,
    state: JobState,
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
    /// Snapshot stores pinned by this Job's records.
    pub stores: BTreeSet<PathBuf>,
}

pub(crate) enum JobTransition<'a> {
    AttemptStarted {
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
        admitted: &'a JobExecution,
        previous: &'a JobExecution,
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
impl JobExecution {
    /// Read the durable continuation phase. A live pause is reported by Execution.
    pub fn state(&self) -> JobState {
        self.state
    }

    pub(crate) fn new(
        run_id: String,
        stage: PathBuf,
        attempt: Option<String>,
        config: crate::RunConfig,
        spec: RunSpec,
    ) -> anyhow::Result<Self> {
        ensure!(
            !run_id.is_empty() && stage.is_absolute(),
            "invalid Job identity or stage"
        );
        ensure!(
            attempt.as_ref().is_none_or(|id| !id.is_empty()),
            "missing Attempt identity"
        );
        let state = if attempt.is_some() {
            JobState::Running
        } else {
            JobState::Restoring
        };
        Ok(Self {
            version: super::super::job_execution::JOB_SCHEMA_VERSION,
            run_id,
            root: stage.clone(),
            active_stage: stage.clone(),
            previous_stage: stage,
            active_attempt: attempt.unwrap_or_default(),
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
        self.state == JobState::Restoring
            && matches!((serde_json::to_value(self), serde_json::to_value(admitted)), (Ok(a), Ok(b)) if a == b)
            && self
                .resumes
                .get(id)
                .is_some_and(|request| request.stage == admitted.active_stage)
    }

    // Each arm validates before mutation so rejected transitions leave no partial changes.
    pub(crate) fn apply(&mut self, input: JobTransition<'_>) -> anyhow::Result<bool> {
        match input {
            JobTransition::AttemptStarted { record } => {
                ensure!(
                    self.run_id == record.run_id
                        && self.active_stage == record.stage_dir()
                        && self.state == JobState::Restoring,
                    "invalid resumed Job ownership"
                );
                self.active_attempt = record
                    .attempt_id
                    .clone()
                    .context("missing Attempt identity")?;
                self.state = JobState::Running;
                self.head = None;
            }
            JobTransition::CaptureAdmitted {
                id,
                suspend,
                ram_storage,
            } => {
                ensure!(
                    self.state == JobState::Running,
                    "JOB_BUSY: execution state is {}",
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
                    self.state = JobState::Suspending;
                }
            }
            JobTransition::CaptureAcknowledged { id, checkpoint } => {
                checkpoint.validate()?;
                ensure!(
                    checkpoint.source_run_id == self.run_id
                        && checkpoint.source_attempt_id == self.active_attempt,
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
            JobTransition::CaptureRejected { id, message } => {
                let request = self.requests.get_mut(id).context("request disappeared")?;
                request.error = Some(message);
                if request.suspend && self.state == JobState::Suspending {
                    self.state = JobState::Unknown;
                }
            }
            JobTransition::RestoreAdmitted {
                id,
                stage,
                eager_ram,
            } => {
                ensure!(
                    self.state == JobState::Suspended && self.head.is_some(),
                    "JOB_BUSY: resume requires a confirmed suspended head (state={})",
                    self.state
                );
                ensure!(
                    !id.trim().is_empty() && id.len() <= 256 && !self.resumes.contains_key(id),
                    "duplicate or invalid restore admission"
                );
                ensure!(
                    stage.starts_with(self.root.join("attempts")),
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
                self.state = JobState::Restoring;
            }
            JobTransition::RestoreAborted {
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
                        && previous.active_attempt == admitted.active_attempt
                        && previous.head == admitted.head
                        && previous.state == JobState::Suspended,
                    "invalid restore rollback"
                );
                *self = previous.clone();
            }
            JobTransition::Completed(result) => self.complete(result)?,
            JobTransition::CompletionObserved { record, result } => {
                if self.active_stage != record.stage_dir() {
                    return Ok(false);
                }
                self.validate_record_target(record)?;
                match result {
                    Some(result)
                        if matches!(
                            self.state,
                            JobState::Running | JobState::Suspending | JobState::Unknown
                        ) =>
                    {
                        self.complete(result)?
                    }
                    Some(_) => return Ok(false),
                    None if self.state == JobState::Unknown => return Ok(false),
                    None => self.state = JobState::Unknown,
                }
            }
            JobTransition::Terminated => {
                ensure!(
                    self.state == JobState::Suspended,
                    "Job has no suspended continuation"
                );
                self.head = None;
                self.state = JobState::Terminal;
            }
        }
        Ok(true)
    }
    fn complete(&mut self, result: &RunResult) -> anyhow::Result<()> {
        ensure!(
            result.state.is_terminal(),
            "cannot finish a Job with a nonterminal result"
        );
        ensure!(
            self.run_id == result.run_id.as_str()
                && self.active_attempt == result.attempt_id.as_str(),
            "terminal Attempt identity mismatch"
        );
        if result.state == RunState::Hibernated {
            let receipt = ExecutionSuspension::from_result(result)?;
            let id = receipt.checkpoint.snapshot_id.clone();
            self.requests
                .get_mut(&receipt.request_id)
                .context("suspend receipt has no Job request")?
                .checkpoint = Some(id.clone());
            self.checkpoints.entry(id.clone()).or_insert(Capture {
                checkpoint: receipt.checkpoint,
                branches: BTreeMap::new(),
            });
            self.head = Some(id);
            self.state = JobState::Suspended;
        } else {
            self.state = JobState::Terminal;
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
        JobExecution,
        RunRecord,
        ExecutionCheckpoint,
        RunResult,
    ) {
        let root = tempfile::tempdir().unwrap();
        let job = JobExecution::new(
            "job".into(),
            root.path().into(),
            Some("attempt".into()),
            crate::RunConfig::default(),
            RunSpec::process("job", "test", "true"),
        )
        .unwrap();
        let record = serde_json::from_value(serde_json::json!({
            "schema_version":1,"run_id":"job","attempt_id":"attempt","session_id":"session","agent":"test","pid":0,"command":["true"],"state":"running","storage":root.path(),"started_at_unix_ms":1,"network":{},"gateway_listen":null,"overlay":null
        })).unwrap();
        let checkpoint = ExecutionCheckpoint {
            snapshot_id: "a".repeat(64),
            store: root.path().join("snapshots"),
            source_run_id: "job".into(),
            source_attempt_id: "attempt".into(),
            created_at_unix_ms: 2,
            ram_storage: SnapshotRamStorage::Raw,
        };
        let result = serde_json::from_value(serde_json::json!({
            "run_id":"job","attempt_id":"attempt","state":"hibernated","started_at_unix_ms":1,"finished_at_unix_ms":3,"value":{"request_id":"suspend","checkpoint":checkpoint}
        })).unwrap();
        (root, job, record, checkpoint, result)
    }
    fn suspend(job: &mut JobExecution, checkpoint: &ExecutionCheckpoint, result: &RunResult) {
        job.apply(JobTransition::CaptureAdmitted {
            id: "suspend",
            suspend: true,
            ram_storage: SnapshotRamStorage::Raw,
        })
        .unwrap();
        assert_eq!(job.state(), JobState::Suspending);
        job.apply(JobTransition::CaptureAcknowledged {
            id: "suspend",
            checkpoint,
        })
        .unwrap();
        assert_eq!(job.state(), JobState::Suspending);
        assert!(job.head.is_none());
        job.apply(JobTransition::Completed(result)).unwrap();
        assert_eq!(job.state(), JobState::Suspended);
    }
    #[test]
    fn termination_withdraws_continuation_without_erasing_history_or_reviving_old_receipts() {
        let (_root, mut job, record, checkpoint, result) = fixture();
        suspend(&mut job, &checkpoint, &result);
        job.apply(JobTransition::Terminated).unwrap();
        let before = serde_json::to_value(&job).unwrap();
        assert!(
            !job.apply(JobTransition::CompletionObserved {
                record: &record,
                result: Some(&result)
            })
            .unwrap()
        );
        assert_eq!(before, serde_json::to_value(&job).unwrap());
        assert_eq!(job.state(), JobState::Terminal);
        assert!(job.head.is_none());
        assert_eq!(job.checkpoints.len(), 1);
        assert!(job.requests["suspend"].checkpoint.is_some());
    }
    #[test]
    fn restore_rolls_back_only_the_unchanged_admission_without_a_successor() {
        let (root, mut job, record, checkpoint, result) = fixture();
        suspend(&mut job, &checkpoint, &result);
        let previous = job.clone();
        let stage = root.path().join("attempts/next");
        job.apply(JobTransition::RestoreAdmitted {
            id: "resume",
            stage: &stage,
            eager_ram: false,
        })
        .unwrap();
        let admitted = job.clone();
        assert!(
            !job.apply(JobTransition::CompletionObserved {
                record: &record,
                result: Some(&result)
            })
            .unwrap()
        );
        assert!(
            !job.apply(JobTransition::RestoreAborted {
                admitted: &admitted,
                previous: &previous,
                id: "resume",
                no_successor: false
            })
            .unwrap()
        );
        assert!(
            job.apply(JobTransition::RestoreAborted {
                admitted: &admitted,
                previous: &previous,
                id: "resume",
                no_successor: true
            })
            .unwrap()
        );
        assert_eq!(
            serde_json::to_value(&job).unwrap(),
            serde_json::to_value(&previous).unwrap()
        );
        job.apply(JobTransition::RestoreAdmitted {
            id: "resume",
            stage: &stage,
            eager_ram: false,
        })
        .unwrap();
        let mut successor = record;
        successor.storage = stage;
        successor.attempt_id = Some("successor".into());
        job.apply(JobTransition::AttemptStarted { record: &successor })
            .unwrap();
        assert!(
            !job.apply(JobTransition::RestoreAborted {
                admitted: &admitted,
                previous: &previous,
                id: "resume",
                no_successor: true
            })
            .unwrap()
        );
        assert_eq!(job.state(), JobState::Running);
        assert_eq!(job.active_attempt, "successor");
    }
    #[test]
    fn missing_receipt_recovery_is_idempotent_and_can_later_confirm_completion() {
        let (_root, mut job, record, _checkpoint, mut result) = fixture();
        assert!(
            job.apply(JobTransition::CompletionObserved {
                record: &record,
                result: None
            })
            .unwrap()
        );
        assert_eq!(job.state(), JobState::Unknown);
        assert!(
            !job.apply(JobTransition::CompletionObserved {
                record: &record,
                result: None
            })
            .unwrap()
        );
        result.state = RunState::Completed;
        result.value = None;
        result.exit_code = Some(0);
        assert!(
            job.apply(JobTransition::CompletionObserved {
                record: &record,
                result: Some(&result)
            })
            .unwrap()
        );
        assert_eq!(job.state(), JobState::Terminal);
        assert!(
            !job.apply(JobTransition::CompletionObserved {
                record: &record,
                result: Some(&result)
            })
            .unwrap()
        );
    }

    #[test]
    fn invalid_receipts_and_duplicate_admissions_leave_the_aggregate_unchanged() {
        let (_root, mut job, _record, checkpoint, mut result) = fixture();
        let before = serde_json::to_value(&job).unwrap();
        assert!(job.apply(JobTransition::Completed(&result)).is_err()); // Missing scoped request.
        assert_eq!(before, serde_json::to_value(&job).unwrap());
        job.apply(JobTransition::CaptureAdmitted {
            id: "suspend",
            suspend: true,
            ram_storage: SnapshotRamStorage::Raw,
        })
        .unwrap();
        let before = serde_json::to_value(&job).unwrap();
        assert!(
            job.apply(JobTransition::CaptureAdmitted {
                id: "suspend",
                suspend: true,
                ram_storage: SnapshotRamStorage::Raw
            })
            .is_err()
        );
        let mut invalid_owner = checkpoint.clone();
        invalid_owner.source_attempt_id = "foreign".into();
        let mut invalid_storage = checkpoint.clone();
        invalid_storage.ram_storage = SnapshotRamStorage::Compressed;
        for invalid in [invalid_owner, invalid_storage] {
            assert!(
                job.apply(JobTransition::CaptureAcknowledged {
                    id: "suspend",
                    checkpoint: &invalid,
                })
                .is_err()
            );
            assert_eq!(before, serde_json::to_value(&job).unwrap());
        }
        result.attempt_id = "foreign".into();
        assert!(job.apply(JobTransition::Completed(&result)).is_err());
        assert!(
            job.apply(JobTransition::CaptureAcknowledged {
                id: "missing",
                checkpoint: &checkpoint
            })
            .is_err()
        );
        assert_eq!(before, serde_json::to_value(&job).unwrap());
    }
}
