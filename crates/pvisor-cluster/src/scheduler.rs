//! Single-writer durable shard. Queue lookahead and expiry indexes bound work
//! by ready/expired tasks, rather than by historical task count.
use crate::journal::Journal;
use crate::*;
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

mod graph;
mod indexes;

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub artifact_storage_limits: Option<ArtifactStorageLimits>,
    pub lease_duration_ms: u64,
    pub queue_lookahead: usize,
    pub max_batch: u32,
    pub max_tasks: usize,
    /// Retained WAL bytes; rejects new commits before exhausting host storage.
    pub max_journal_bytes: u64,
    pub max_artifact_bytes: u64,
    /// Limits concurrent reservations per tenant. Unlisted tenants have no quota.
    pub tenant_quotas: BTreeMap<String, Resources>,
}
impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            artifact_storage_limits: None,
            lease_duration_ms: 30_000,
            queue_lookahead: 256,
            max_batch: 64,
            max_tasks: 1_000_000,
            max_journal_bytes: crate::journal::DEFAULT_MAX_JOURNAL_BYTES,
            max_artifact_bytes: crate::artifacts::DEFAULT_MAX_ARTIFACT_BYTES,
            tenant_quotas: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Change {
    ArtifactAuthority {
        id: String,
    },
    GraphSubmitted {
        spec: TaskGraphSpec,
        tasks: Vec<TaskRecord>,
        at: u64,
    },
    GraphCancelled {
        graph_id: String,
        at: u64,
    },
    Environment {
        record: EnvironmentRecord,
    },
    Submit {
        task: Box<TaskRecord>,
    },
    Fork {
        record: Box<ExecutionForkRecord>,
        branches: Vec<TaskRecord>,
    },
    LiveForkRequested {
        record: Box<LiveForkRecord>,
        control: ControlRecord,
        branches: Vec<TaskRecord>,
    },
    Register {
        registration: WorkerRegistration,
        at: u64,
    },
    Drain {
        worker_id: String,
        draining: bool,
    },
    Assign {
        #[serde(default)]
        artifact_pin_protocol: Option<u32>,
        lease: Lease,
        at: u64,
    },
    Renew {
        worker_id: String,
        at: u64,
        expires: u64,
        keys: Vec<LeaseKey>,
        acknowledged: BTreeSet<String>,
        #[serde(default)]
        admission: Option<Box<AdmissionReport>>,
    },
    Cancel {
        task_id: String,
        at: u64,
    },
    LeaseExpired {
        task_id: String,
        at: u64,
    },
    NativeDone {
        task_id: String,
        result: Box<pvisor_core::RunResult>,
        reserved: Resources,
        at: u64,
    },
    ArtifactsRetired {
        entries: Vec<ArtifactRetirement>,
        at: u64,
    },
    Finish {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint_publication: Option<CheckpointPublication>,
        task_id: String,
        phase: TaskPhase,
        result: Option<Box<pvisor_core::RunResult>>,
        error: Option<String>,
        #[serde(default)]
        artifacts: Option<BlobRef>,
        #[serde(default)]
        artifact_error: Option<String>,
        at: u64,
    },
    ControlRequested {
        task_id: String,
        record: ControlRecord,
    },
    ControlIssued {
        task_id: String,
        revision: u64,
        reserved: Resources,
        at: u64,
    },
    ControlAcknowledged {
        acknowledgement: ControlAcknowledgement,
        reserved: Resources,
        at: u64,
    },
    Decline {
        rejection: AdmissionRejection,
        at: u64,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct Transaction {
    version: u32,
    changes: Vec<Change>,
}

pub struct Scheduler {
    graph_state: graph::GraphState,
    config: SchedulerConfig,
    journal: Journal,
    tasks: BTreeMap<String, TaskRecord>,
    run_ids: BTreeSet<String>,
    forks: BTreeMap<(String, String), ExecutionForkRecord>,
    live_forks: BTreeMap<(String, String), LiveForkRecord>,
    /// One capture per source, indexed independently of historical workflows.
    pending_live_forks: BTreeMap<String, String>,
    workers: BTreeMap<String, WorkerRecord>,
    indexes: indexes::TaskIndexes,
    active: BTreeMap<String, BTreeSet<String>>,
    expiry: BTreeSet<(u64, String)>,
    tenant_reserved: BTreeMap<String, Resources>,
    artifacts: crate::artifacts::ArtifactStore,
    retained: BTreeMap<String, BlobRef>,
    retention_age: BTreeSet<(u64, String)>,
    retained_pins: BTreeMap<String, crate::artifacts::gc::Pins>,
    environments: BTreeMap<String, EnvironmentRecord>,
    memory_bindings: BTreeMap<String, MemoryBinding>,
    cpu_bindings: BTreeMap<String, CpuBinding>,
    node_memory_bindings: BTreeMap<String, NodeMemoryBinding>,
    // Release the controller claim after its durable state and pins are dropped.
    _artifact_owner: crate::artifacts::ControllerOwner,
}

#[derive(Default)]
struct NodeMemoryBinding {
    process: Option<(u32, u64)>,
    host_boot: Option<String>,
}

struct CpuBinding {
    key: LeaseKey,
    attempt: pvisor_core::AttemptId,
    usage: Option<pvisor_core::cpu::ProcessCpuUsage>,
}

struct MemoryBinding {
    key: LeaseKey,
    attempt: pvisor_core::AttemptId,
    process: Option<(u32, u64)>,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

impl Scheduler {
    pub fn open(path: &Path, config: SchedulerConfig) -> anyhow::Result<Self> {
        ensure!(
            config.lease_duration_ms >= 100 && config.lease_duration_ms <= 300_000,
            "lease duration must be 100..300000 ms"
        );
        ensure!(
            config.queue_lookahead > 0 && config.max_batch > 0 && config.max_tasks > 0,
            "scheduler limits must be positive"
        );
        if let Some(limits) = &config.artifact_storage_limits {
            limits.validate()?;
        }
        let (journal, mut transactions) =
            Journal::open_stream::<Transaction>(path, config.max_journal_bytes)?;
        let artifacts = crate::artifacts::ArtifactStore::open_with_quota(
            &path.with_extension("artifacts"),
            config.max_artifact_bytes,
        )?;
        let artifact_owner = artifacts.claim_controller()?;
        let mut authority = None;
        let mut scheduler = Self {
            _artifact_owner: artifact_owner,
            graph_state: graph::GraphState::default(),
            config,
            journal,
            tasks: BTreeMap::new(),
            run_ids: BTreeSet::new(),
            forks: BTreeMap::new(),
            live_forks: BTreeMap::new(),
            pending_live_forks: BTreeMap::new(),
            workers: BTreeMap::new(),
            indexes: indexes::TaskIndexes::default(),
            active: BTreeMap::new(),
            expiry: BTreeSet::new(),
            tenant_reserved: BTreeMap::new(),
            artifacts,
            retained: BTreeMap::new(),
            retention_age: BTreeSet::new(),
            retained_pins: BTreeMap::new(),
            environments: BTreeMap::new(),
            memory_bindings: BTreeMap::new(),
            cpu_bindings: BTreeMap::new(),
            node_memory_bindings: BTreeMap::new(),
        };
        for transaction in transactions.by_ref() {
            let transaction = transaction?;
            ensure!(
                transaction.version == CLUSTER_VERSION,
                "unsupported journal version"
            );
            for change in transaction.changes {
                if let Change::ArtifactAuthority { id } = &change {
                    ensure!(authority.is_none(), "duplicate artifact WAL authority");
                    authority = Some(id.clone());
                }
                scheduler.apply(change);
            }
        }
        let valid_end = transactions.valid_end;
        drop(transactions);
        ensure!(
            scheduler.tasks.len() <= scheduler.config.max_tasks,
            "replayed task history exceeds configured max_tasks"
        );
        scheduler.journal.finish_replay(valid_end)?;
        scheduler
            .artifacts
            .bind_authority(authority.as_deref(), |id| {
                scheduler.journal.append(&Transaction {
                    version: CLUSTER_VERSION,
                    changes: vec![Change::ArtifactAuthority { id: id.into() }],
                })
            })?;
        if let Some(limits) = &scheduler.config.artifact_storage_limits {
            scheduler.artifacts.set_storage_limits(limits.clone())?;
        }
        // Replayed submit/decline events can refer to the same queued task.
        // Rebuild from durable state so one batch cannot lease it twice.
        let mut ready: Vec<_> = scheduler
            .tasks
            .values()
            .filter(|task| task.phase == TaskPhase::Queued)
            .map(|task| (task.updated_at_ms, task.spec.id.clone()))
            .collect();
        ready.sort();
        scheduler.indexes.ready = ready.into_iter().map(|(_, id)| id).collect();
        for (id, reference) in &scheduler.retained {
            let (_, pins) = scheduler.artifacts.pin_manifest(reference)?;
            scheduler.retained_pins.insert(id.clone(), pins);
        }
        Ok(scheduler)
    }

    fn commit(&mut self, changes: Vec<Change>) -> anyhow::Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let transaction = Transaction {
            version: CLUSTER_VERSION,
            changes,
        };
        self.journal.append(&transaction)?;
        for change in transaction.changes {
            self.apply(change);
        }
        Ok(())
    }

    pub(crate) fn ensure_available(&self) -> anyhow::Result<()> {
        self.journal.ensure_available()
    }

    pub(crate) fn begin_group(&mut self) -> anyhow::Result<()> {
        self.journal.begin_group()
    }

    pub(crate) fn end_group(&mut self) -> anyhow::Result<()> {
        self.journal.end_group()
    }

    pub(crate) fn pending_journal_bytes(&self) -> usize {
        self.journal.pending_bytes()
    }

    #[cfg(test)]
    pub(crate) fn journal_syncs(&self) -> usize {
        self.journal.syncs()
    }

    #[cfg(test)]
    pub(crate) fn fail_journal_sync(&mut self) {
        self.journal.fail_sync();
    }

    fn release(&mut self, id: &str) {
        let task = &self.tasks[id];
        if let Some(lease) = &task.lease {
            self.expiry.remove(&(lease.expires_at_ms, id.to_owned()));
            self.active
                .get_mut(&lease.key.worker_id)
                .expect("active worker")
                .remove(id);
            let worker = self
                .workers
                .get_mut(&lease.key.worker_id)
                .expect("lease worker");
            worker.reserved = worker
                .reserved
                .checked_sub(task.current_reservation())
                .expect("worker accounting");
            let tenant = self
                .tenant_reserved
                .get_mut(&task.spec.tenant)
                .expect("tenant accounting");
            *tenant = tenant
                .checked_sub(task.current_reservation())
                .expect("tenant accounting");
        }
    }

    fn reallocate(&mut self, id: &str, reserved: Resources) {
        let task = &self.tasks[id];
        let previous = task.current_reservation();
        let worker = self
            .workers
            .get_mut(&task.lease.as_ref().expect("lease").key.worker_id)
            .expect("worker");
        worker.reserved = worker
            .reserved
            .checked_sub(previous)
            .and_then(|r| r.checked_add(reserved))
            .expect("worker accounting");
        let tenant = self
            .tenant_reserved
            .get_mut(&task.spec.tenant)
            .expect("tenant");
        *tenant = tenant
            .checked_sub(previous)
            .and_then(|r| r.checked_add(reserved))
            .expect("tenant accounting");
        self.tasks.get_mut(id).expect("task").reserved = Some(reserved);
    }

    fn apply(&mut self, change: Change) {
        match change {
            Change::ArtifactAuthority { .. } => {}
            Change::GraphSubmitted { spec, tasks, at } => self.apply_graph(spec, tasks, at),
            Change::GraphCancelled { graph_id, at } => self.apply_graph_cancel(&graph_id, at),
            Change::Environment { record } => {
                self.environments.insert(record.digest.clone(), record);
            }
            Change::Submit { task } => {
                self.run_ids.insert(task.spec.run.run_id.to_string());
                let previous = self.tasks.get(&task.spec.id).map(|task| task.phase);
                self.indexes.insert(&task, previous);
                self.tasks.insert(task.spec.id.clone(), *task);
            }
            Change::LiveForkRequested {
                record,
                control,
                branches,
            } => {
                self.apply(Change::ControlRequested {
                    task_id: record.source_task_id.clone(),
                    record: control,
                });
                for task in branches {
                    self.apply(Change::Submit {
                        task: Box::new(task),
                    });
                }
                self.pending_live_forks.insert(
                    record.source_task_id.clone(),
                    record.request.request_id.clone(),
                );
                self.live_forks.insert(
                    (
                        record.source_task_id.clone(),
                        record.request.request_id.clone(),
                    ),
                    *record,
                );
            }
            Change::Fork { record, branches } => {
                for task in branches {
                    self.apply(Change::Submit {
                        task: Box::new(task),
                    });
                }
                self.forks.insert(
                    (
                        record.source_task_id.clone(),
                        record.request.request_id.clone(),
                    ),
                    *record,
                );
            }
            Change::Register { registration, at } => {
                let id = registration.id.clone();
                let same_incarnation = self.workers.get(&id).is_some_and(|worker| {
                    worker.registration.incarnation == registration.incarnation
                });
                let memory_sample = same_incarnation
                    .then(|| self.workers[&id].memory_sample.clone())
                    .flatten();
                if !same_incarnation {
                    self.node_memory_bindings.remove(&id);
                }
                let reserved = self
                    .workers
                    .get(&id)
                    .map_or(Resources::default(), |w| w.reserved);
                let draining = self.workers.get(&id).is_some_and(|w| w.draining);
                self.workers.insert(
                    id.clone(),
                    WorkerRecord {
                        registration,
                        seen_at_ms: at,
                        draining,
                        reserved,
                        admission: None,
                        admission_received_at_ms: None,
                        memory_sample,
                    },
                );
                self.active.entry(id).or_default();
            }
            Change::Drain {
                worker_id,
                draining,
            } => self.workers.get_mut(&worker_id).expect("worker").draining = draining,
            Change::Assign {
                lease,
                at,
                artifact_pin_protocol,
            } => {
                let task = self.tasks.get_mut(&lease.key.task_id).expect("task");
                self.indexes.set_phase(task, TaskPhase::Leased);
                task.artifact_pin_protocol = artifact_pin_protocol;
                task.generation = lease.key.generation;
                task.updated_at_ms = at;
                let worker = self.workers.get_mut(&lease.key.worker_id).expect("worker");
                worker.reserved = worker
                    .reserved
                    .checked_add(task.spec.resources)
                    .expect("worker accounting");
                let reserved = self
                    .tenant_reserved
                    .entry(task.spec.tenant.clone())
                    .or_default();
                *reserved = reserved
                    .checked_add(task.spec.resources)
                    .expect("tenant accounting");
                self.active
                    .get_mut(&lease.key.worker_id)
                    .expect("worker")
                    .insert(task.spec.id.clone());
                self.expiry
                    .insert((lease.expires_at_ms, task.spec.id.clone()));
                task.lease = Some(lease);
                task.reserved = Some(task.spec.resources);
            }
            Change::Renew {
                worker_id,
                at,
                expires,
                keys,
                acknowledged,
                admission,
            } => {
                let worker = self.workers.get_mut(&worker_id).expect("worker");
                worker.seen_at_ms = at;
                worker.admission_received_at_ms = admission.as_ref().map(|_| at);
                worker.admission = admission.map(|r| *r);
                for key in keys {
                    let task = self.tasks.get_mut(&key.task_id).expect("task");
                    let lease = task.lease.as_mut().expect("lease");
                    self.expiry
                        .remove(&(lease.expires_at_ms, key.task_id.clone()));
                    lease.expires_at_ms = expires;
                    self.expiry.insert((expires, key.task_id.clone()));
                    if task.phase == TaskPhase::Leased && acknowledged.contains(&key.task_id) {
                        self.indexes.set_phase(task, TaskPhase::Running);
                    }
                    task.updated_at_ms = at;
                }
            }
            Change::Cancel { task_id, at } => {
                // A preceding graph cancellation may already have settled this
                // blocked descendant in the same transaction.
                if self.tasks[&task_id].phase.terminal() {
                    return;
                }
                let task = self.tasks.get_mut(&task_id).expect("task");
                let phase = if matches!(
                    task.phase,
                    TaskPhase::Queued
                        | TaskPhase::WaitingCheckpoint
                        | TaskPhase::WaitingDependencies
                ) {
                    TaskPhase::Cancelled
                } else {
                    TaskPhase::Cancelling
                };
                self.indexes.set_phase(task, phase);
                task.updated_at_ms = at;
                for control in &mut task.controls {
                    if !control.phase.terminal() {
                        control.phase = ControlPhase::Aborted;
                        control.completed_at_ms = Some(at);
                    }
                }
                self.settle_live_fork(
                    &task_id,
                    None,
                    Some("source cancelled before capture acknowledgement".into()),
                    at,
                );
                self.settle_dependencies(&task_id, at);
            }
            Change::LeaseExpired { task_id, at } => {
                // Native evidence is already in the WAL. Move it into the
                // final record without serializing it again in an expiry batch.
                let task = self.tasks.get_mut(&task_id).expect("task");
                let known = task.result.is_some();
                let phase = if known {
                    if task.phase == TaskPhase::Cancelling {
                        TaskPhase::Cancelled
                    } else {
                        TaskPhase::Failed
                    }
                } else {
                    TaskPhase::Lost
                };
                let result = task.result.take().map(Box::new);
                self.apply(Change::Finish {
                    checkpoint_publication: None,
                    task_id, phase, result,
                    error: (!known).then(|| "lease expired; execution outcome unknown, no automatic retry".into()),
                    artifacts: None,
                    artifact_error: known.then(|| "artifact delivery lease expired; native outcome preserved, no automatic retry".into()),
                    at,
                });
            }
            Change::NativeDone {
                task_id,
                result,
                reserved,
                at,
            } => {
                self.reallocate(&task_id, reserved);
                let task = self.tasks.get_mut(&task_id).expect("task");
                if task.phase != TaskPhase::Cancelling {
                    self.indexes.set_phase(task, TaskPhase::RetainingArtifacts);
                }
                task.result = Some(*result);
                task.updated_at_ms = at;
                task.memory_sample = None;
                task.cpu_sample = None;
                self.cpu_bindings.remove(&task_id);
                self.memory_bindings.remove(&task_id);
                for control in &mut task.controls {
                    if !control.phase.terminal() {
                        control.phase = ControlPhase::Aborted;
                        control.completed_at_ms = Some(at);
                    }
                }
                self.settle_live_fork(
                    &task_id,
                    None,
                    Some("source ended before capture acknowledgement".into()),
                    at,
                );
                // No dependency settlement until verified delivery or explicit failure.
            }
            Change::ArtifactsRetired { entries, at } => {
                for entry in entries {
                    let task = self.tasks.get_mut(&entry.task_id).expect("retired task");
                    task.artifact_retired_at_ms.get_or_insert(at);
                    self.retained.remove(&entry.task_id);
                    self.retention_age
                        .remove(&(entry.finished_at_ms, entry.task_id.clone()));
                    self.retained_pins.remove(&entry.task_id);
                }
            }
            Change::Finish {
                checkpoint_publication,
                task_id,
                phase,
                result,
                error,
                artifacts,
                artifact_error,
                at,
            } => {
                self.release(&task_id);
                let task = self.tasks.get_mut(&task_id).expect("task");
                self.indexes.set_phase(task, phase);
                task.result = result.map(|r| *r);
                task.error = error;
                task.artifacts = artifacts;
                task.checkpoint_publication = checkpoint_publication;
                if let Some(reference) = &task.artifacts {
                    self.retained.insert(task_id.clone(), reference.clone());
                    self.retention_age.insert((at, task_id.clone()));
                }
                task.artifact_error = artifact_error;
                task.updated_at_ms = at;
                task.reserved = None;
                task.memory_sample = None;
                task.cpu_sample = None;
                self.cpu_bindings.remove(&task_id);
                self.memory_bindings.remove(&task_id);
                for control in &mut task.controls {
                    if !control.phase.terminal() {
                        control.phase = ControlPhase::Aborted;
                        control.completed_at_ms = Some(at);
                    }
                }
                // Preserve the lease key as evidence for idempotent completion.
                self.settle_live_fork(
                    &task_id,
                    None,
                    Some("source ended before capture acknowledgement".into()),
                    at,
                );
                self.settle_dependencies(&task_id, at);
            }
            Change::ControlRequested { task_id, record } => {
                self.tasks
                    .get_mut(&task_id)
                    .expect("task")
                    .controls
                    .push(record);
            }
            Change::Decline { rejection, at } => {
                let id = rejection.key.task_id.clone();
                self.release(&id);
                let task = self.tasks.get_mut(&id).expect("task");
                self.indexes.set_phase(task, TaskPhase::Queued);
                task.lease = None;
                task.reserved = None;
                task.updated_at_ms = at;
                task.admission_rejections += 1;
                task.last_admission_rejection = Some(rejection);
                task.memory_sample = None;
                task.cpu_sample = None;
                self.cpu_bindings.remove(&id);
                self.memory_bindings.remove(&id);
                for control in &mut task.controls {
                    if !control.phase.terminal() {
                        control.phase = ControlPhase::Aborted;
                        control.completed_at_ms = Some(at);
                    }
                }
            }
            Change::ControlIssued {
                task_id,
                revision,
                reserved,
                at,
            } => {
                let admission = reserved
                    .checked_sub(self.tasks[&task_id].current_reservation())
                    .expect("control admission");
                self.reallocate(&task_id, reserved);
                let task = self.tasks.get_mut(&task_id).expect("task");
                let record = task
                    .controls
                    .iter_mut()
                    .find(|c| c.command.revision == revision)
                    .expect("control");
                record.phase = ControlPhase::Issued;
                record.issued_at_ms = Some(at);
                record.admission = admission;
                task.updated_at_ms = at;
            }
            Change::ControlAcknowledged {
                acknowledgement,
                reserved,
                at,
            } => {
                let id = &acknowledgement.command.key.task_id;
                let settles_capture = self.pending_live_forks.get(id).is_some_and(|request_id| {
                    let plan = &self.live_forks[&(id.clone(), request_id.clone())];
                    plan.source_key == acknowledgement.command.key
                        && plan.request.checkpoint_request_id
                            == acknowledgement.command.request.request_id
                });
                let capture_outcome = settles_capture.then(|| acknowledgement.outcome.clone());
                self.reallocate(id, reserved);
                let task = self.tasks.get_mut(id).expect("task");
                let phase = match &acknowledgement.outcome {
                    ControlOutcome::Checkpointed { .. }
                        if acknowledgement.command.request.action == ControlAction::Suspend =>
                    {
                        Some(TaskPhase::Suspending)
                    }
                    ControlOutcome::Succeeded { state, .. } => Some(match state {
                        pvisor_core::VmState::Running => TaskPhase::Running,
                        pvisor_core::VmState::Paused => TaskPhase::Paused,
                        pvisor_core::VmState::Offloaded => TaskPhase::Offloaded,
                    }),
                    _ => None,
                };
                if let Some(phase) = phase {
                    self.indexes.set_phase(task, phase);
                }
                let record = task
                    .controls
                    .iter_mut()
                    .find(|c| c.command == acknowledgement.command)
                    .expect("control");
                record.phase = match acknowledgement.outcome {
                    ControlOutcome::Checkpointed { .. } | ControlOutcome::Succeeded { .. } => {
                        ControlPhase::Succeeded
                    }
                    ControlOutcome::Failed { .. } => ControlPhase::Failed,
                };
                record.outcome = Some(acknowledgement.outcome);
                record.completed_at_ms = Some(at);
                task.updated_at_ms = at;
                match capture_outcome {
                    Some(ControlOutcome::Checkpointed { checkpoint }) => {
                        self.settle_live_fork(id, Some(checkpoint), None, at)
                    }
                    Some(ControlOutcome::Failed { error }) => self.settle_live_fork(
                        id,
                        None,
                        Some(format!("live fork capture failed: {error}")),
                        at,
                    ),
                    _ => {}
                }
            }
        }
    }

    pub fn publish_environment(
        &mut self,
        template: EnvironmentTemplate,
    ) -> anyhow::Result<EnvironmentRecord> {
        let record = crate::environment::record(template)?;
        if let Some(existing) = self.environments.get(&record.digest) {
            return Ok(existing.clone());
        }
        ensure!(
            self.environments.len() < 10_000,
            "environment retention limit reached"
        );
        self.commit(vec![Change::Environment {
            record: record.clone(),
        }])?;
        Ok(record)
    }
    pub fn environment(&self, digest: &str) -> anyhow::Result<EnvironmentRecord> {
        self.environments
            .get(digest)
            .cloned()
            .context("unknown environment digest")
    }
    fn task_environment(&self, spec: &TaskSpec) -> Option<EnvironmentRecord> {
        spec.environment
            .as_ref()
            .map(|digest| self.environments[digest].clone())
    }

    fn restore_observation(
        &self,
        spec: &TaskSpec,
    ) -> anyhow::Result<Option<(&LeaseKey, &pvisor_core::operation::ExecutionCheckpoint)>> {
        let Some(restore) = &spec.restore else {
            return Ok(None);
        };
        ensure!(
            identifier(&restore.task_id) && identifier(&restore.request_id),
            "invalid execution restore reference"
        );
        let source = self
            .tasks
            .get(&restore.task_id)
            .context("execution restore source task is unknown")?;
        ensure!(
            source.spec.tenant == spec.tenant,
            "execution restore cannot cross tenant boundaries"
        );
        ensure!(
            source.spec.cpu_qos == spec.cpu_qos,
            "execution restore must preserve the captured CPU QoS class"
        );
        let observed = source
            .controls
            .iter()
            .find(|record| record.command.request.request_id == restore.request_id)
            .context("execution checkpoint control is unknown")?;
        ensure!(
            observed.phase == ControlPhase::Succeeded
                && matches!(
                    observed.command.request.action,
                    ControlAction::Checkpoint | ControlAction::Suspend
                ),
            "execution restore requires a durably acknowledged checkpoint"
        );
        let Some(ControlOutcome::Checkpointed { checkpoint }) = &observed.outcome else {
            anyhow::bail!("execution checkpoint control has no sealed checkpoint");
        };
        checkpoint.validate()?;
        if observed.command.request.action == ControlAction::Suspend {
            let receipt = pvisor_core::operation::ExecutionSuspension::from_result(
                source
                    .result
                    .as_ref()
                    .context("suspend awaits native terminal completion")?,
            )?;
            ensure!(
                source.phase.terminal()
                    && receipt.request_id == restore.request_id
                    && receipt.checkpoint == *checkpoint,
                "suspend continuation requires matching native termination evidence"
            );
        }
        ensure!(
            checkpoint.source_run_id == source.spec.run.run_id.as_str(),
            "execution checkpoint source Run mismatch"
        );
        Ok(Some((&observed.command.key, checkpoint)))
    }

    fn task_checkpoint(
        &self,
        spec: &TaskSpec,
    ) -> anyhow::Result<Option<pvisor_core::operation::ExecutionCheckpoint>> {
        Ok(self
            .restore_observation(spec)?
            .map(|(_, checkpoint)| checkpoint.clone()))
    }

    fn task_checkpoint_publication(&self, spec: &TaskSpec) -> Option<CheckpointPublication> {
        let restore = spec.restore.as_ref()?;
        let source = self.tasks.get(&restore.task_id)?;
        let publication = source.checkpoint_publication.as_ref()?;
        let observation = source.controls.iter().find(|record| {
            record.command.request.request_id == restore.request_id
                && record.phase == ControlPhase::Succeeded
        })?;
        let ControlOutcome::Checkpointed { checkpoint } = observation.outcome.as_ref()? else {
            return None;
        };
        (publication.checkpoint == *checkpoint).then(|| publication.clone())
    }

    fn restore_fits(
        &self,
        spec: &TaskSpec,
        registration: &WorkerRegistration,
    ) -> anyhow::Result<bool> {
        let Some((source, checkpoint)) = self.restore_observation(spec)? else {
            return Ok(true);
        };
        if registration.execution_restore_protocol != Some(CLUSTER_VERSION) {
            return Ok(false);
        }
        if source.worker_id == registration.id {
            return Ok(true);
        }
        Ok(self
            .task_checkpoint_publication(spec)
            .is_some_and(|publication| {
                publication.checkpoint == *checkpoint
                    && registration
                        .checkpoint_storage
                        .as_ref()
                        .is_some_and(|support| {
                            support.repository == publication.repository
                                && support.compatibility == publication.compatibility
                        })
            }))
    }

    pub fn submit(&mut self, spec: TaskSpec, now: u64) -> anyhow::Result<TaskRecord> {
        let task = self.prepare_submission(spec, now)?;
        if !self.tasks.contains_key(&task.spec.id) {
            self.commit(vec![Change::Submit {
                task: Box::new(task.clone()),
            }])?;
        }
        Ok(task)
    }

    fn prepare_submission(&self, spec: TaskSpec, now: u64) -> anyhow::Result<TaskRecord> {
        self.prepare_submission_for_capture(spec, now, None)
    }

    fn prepare_submission_for_capture(
        &self,
        spec: TaskSpec,
        now: u64,
        capture_key: Option<&LeaseKey>,
    ) -> anyhow::Result<TaskRecord> {
        ensure!(
            spec.version == CLUSTER_VERSION,
            "unsupported cluster version"
        );
        ensure!(
            identifier(&spec.id) && identifier(&spec.tenant),
            "invalid task or tenant id"
        );
        ensure!(
            !spec.run.run_id.is_empty()
                && spec.run.schema_version == pvisor_core::RUNTIME_SCHEMA_VERSION,
            "invalid RunSpec identity/version"
        );
        spec.validate_cpu_qos()?;
        spec.validate_gateway()?;
        spec.validate_artifacts()?;
        ensure!(
            !spec
                .run
                .metadata
                .contains_key("pvisor.orchestration.checkpoint_publication"),
            "task overrides checkpoint publication provenance"
        );
        ensure!(
            !spec
                .run
                .metadata
                .contains_key("pvisor.orchestration.artifact_retention"),
            "task cannot override Worker artifact retention provenance"
        );
        let pvisor_core::RunInvocation::Process(process) = &spec.run.invocation;
        ensure!(!process.program.trim().is_empty(), "empty program");
        ensure!(
            !process.inherit_env,
            "cluster tasks must explicitly project environment variables"
        );
        ensure!(
            !spec
                .run
                .metadata
                .contains_key("pvisor.orchestration.execution_restore"),
            "task cannot override Worker execution restore provenance"
        );
        ensure!(
            !spec
                .run
                .metadata
                .contains_key("pvisor.orchestration.gateway"),
            "task cannot override Worker Gateway provenance"
        );
        if let Some(digest) = &spec.environment {
            self.environment(digest)?;
            ensure!(
                spec.execution
                    == ExecutionClass {
                        executor: pvisor_core::ExecutorKind::VirtualMachine,
                        isolation: pvisor_core::IsolationKind::VirtualMachine
                    },
                "immutable environments require VM execution"
            );
            ensure!(
                !spec.run.metadata.keys().any(|k| k.starts_with("pvisor.vm.")
                    || k.starts_with("pvisor.orchestration.environment")),
                "environment tasks cannot override host VM/environment preparation metadata"
            );
            ensure!(
                process
                    .cwd
                    .as_ref()
                    .is_none_or(|p| Path::new(p).is_absolute()),
                "environment cwd must be an absolute guest path"
            );
        }
        ensure!(
            spec.resources.slots == 1
                && spec.resources.memory_bytes > 0
                && spec.resources.cpu_millis > 0,
            "task needs one slot and positive memory/CPU admission budgets"
        );
        ensure!(
            spec.run.runtime.max_output_bytes <= 1024 * 1024,
            "cluster output limit is 1 MiB per stream"
        );
        if let Some(existing) = self.tasks.get(&spec.id) {
            ensure!(
                serde_json::to_value(&existing.spec)? == serde_json::to_value(&spec)?,
                "idempotency conflict: task id already has a different specification"
            );
            return Ok(existing.clone());
        }
        ensure!(
            !self.run_ids.contains(spec.run.run_id.as_str()),
            "Run id already belongs to another task"
        );
        if let Some(restore) = &spec.restore {
            if let Some(key) = capture_key {
                let source = &self.tasks[&restore.task_id];
                ensure!(
                    source.phase == TaskPhase::Running
                        && source.lease.as_ref().is_some_and(|lease| lease.key == *key)
                        && self.valid_key(key, now),
                    "live fork source lease changed"
                );
            } else {
                self.restore_observation(&spec)?;
            }
            let source = &self.tasks[&restore.task_id].spec;
            ensure!(
                spec.run.run_id != source.run.run_id
                    && spec.run.parent_run_id.as_ref() == Some(&source.run.run_id),
                "execution restore requires a new Run identity and explicit parent"
            );
            ensure!(
                spec.execution == source.execution
                    && spec.resources == source.resources
                    && spec.gateway == source.gateway
                    && spec.environment == source.environment,
                "execution restore must preserve source execution, resources and environment"
            );
            let mut expected = source.run.clone();
            expected.run_id = spec.run.run_id.clone();
            expected.parent_run_id = Some(source.run.run_id.clone());
            expected.task_id = spec.run.task_id.clone();
            ensure!(
                serde_json::to_value(expected)? == serde_json::to_value(&spec.run)?,
                "execution restore must preserve source command, input, environment and policies"
            );
        }
        ensure!(
            self.tasks.len() < self.config.max_tasks,
            "task retention limit reached"
        );
        let task = TaskRecord {
            checkpoint_publication: None,
            artifact_pin_protocol: None,
            artifact_retired_at_ms: None,
            spec,
            phase: TaskPhase::Queued,
            generation: 0,
            lease: None,
            result: None,
            error: None,
            created_at_ms: now,
            updated_at_ms: now,
            controls: Vec::new(),
            reserved: None,
            admission_rejections: 0,
            last_admission_rejection: None,
            artifacts: None,
            artifact_error: None,
            memory_sample: None,
            cpu_sample: None,
        };
        Ok(task)
    }

    pub fn execution_fork(
        &self,
        source: &str,
        request_id: &str,
    ) -> anyhow::Result<ExecutionForkRecord> {
        self.forks
            .get(&(source.to_owned(), request_id.to_owned()))
            .cloned()
            .context("unknown execution fork")
    }

    /// All branches and their receipt share one fsync-before-ack transaction.
    /// No caller-supplied command, host path or authorization override is accepted.
    pub fn fork_execution(
        &mut self,
        source: &str,
        request: ExecutionForkRequest,
        now: u64,
    ) -> anyhow::Result<ExecutionForkRecord> {
        self.reap(now)?;
        ensure!(
            request.version == CLUSTER_VERSION,
            "unsupported fork version"
        );
        ensure!(
            identifier(source)
                && identifier(&request.request_id)
                && identifier(&request.checkpoint_request_id),
            "invalid execution fork identity"
        );
        if let Some(record) = self
            .forks
            .get(&(source.to_owned(), request.request_id.clone()))
        {
            ensure!(
                record.request == request,
                "execution fork idempotency conflict"
            );
            return Ok(record.clone());
        }
        ensure!(
            !self
                .live_forks
                .contains_key(&(source.to_owned(), request.request_id.clone())),
            "fork request id already belongs to live capture"
        );
        let branches = self.prepare_fork_branches(source, &request, now, None)?;
        let (source_key, checkpoint) = self
            .restore_observation(&branches[0].spec)?
            .context("fork requires a sealed execution checkpoint")?;
        let record = ExecutionForkRecord {
            version: CLUSTER_VERSION,
            source_task_id: source.to_owned(),
            source_key: source_key.clone(),
            request,
            checkpoint: checkpoint.clone(),
            created_at_ms: now,
        };
        let change = Change::Fork {
            record: Box::new(record.clone()),
            branches,
        };
        let envelope_bytes = serde_json::to_vec(&Transaction {
            version: CLUSTER_VERSION,
            changes: Vec::new(),
        })?
        .len();
        ensure!(
            serde_json::to_vec(&change)?
                .len()
                .checked_add(envelope_bytes)
                .is_some_and(|bytes| bytes <= MAX_EXECUTION_FORK_BYTES),
            "execution fork transaction exceeds 4 MiB"
        );
        self.commit(vec![change])?;
        Ok(record)
    }

    fn prepare_fork_branches(
        &self,
        source: &str,
        request: &ExecutionForkRequest,
        now: u64,
        capture_key: Option<&LeaseKey>,
    ) -> anyhow::Result<Vec<TaskRecord>> {
        ensure!(
            (1..=MAX_EXECUTION_FORK_BRANCHES).contains(&request.branches.len()),
            "execution fork needs 1..64 branches"
        );
        ensure!(
            self.tasks
                .len()
                .checked_add(request.branches.len())
                .is_some_and(|n| n <= self.config.max_tasks),
            "task retention limit reached"
        );
        let parent = self.tasks.get(source).context("unknown fork source task")?;
        ensure!(
            serde_json::to_vec(&parent.spec)?
                .len()
                .checked_mul(request.branches.len())
                .is_some_and(|n| n <= MAX_EXECUTION_FORK_BYTES),
            "execution fork specification batch exceeds 4 MiB"
        );
        let mut task_ids = BTreeSet::new();
        let mut run_ids = BTreeSet::new();
        let mut branches = Vec::with_capacity(request.branches.len());
        for branch in &request.branches {
            ensure!(
                identifier(&branch.task_id)
                    && !branch.run_id.is_empty()
                    && branch.run_id.as_str().len() <= 128,
                "invalid fork branch identity"
            );
            ensure!(
                task_ids.insert(branch.task_id.clone())
                    && run_ids.insert(branch.run_id.to_string()),
                "duplicate fork branch identity"
            );
            ensure!(
                !self.tasks.contains_key(&branch.task_id)
                    && !self.run_ids.contains(branch.run_id.as_str()),
                "fork branch identity already exists"
            );
            let mut spec = parent.spec.clone();
            spec.id = branch.task_id.clone();
            spec.run.run_id = branch.run_id.clone();
            spec.run.parent_run_id = Some(parent.spec.run.run_id.clone());
            spec.restore = Some(ExecutionRestore {
                task_id: source.to_owned(),
                request_id: request.checkpoint_request_id.clone(),
            });
            let mut task = self.prepare_submission_for_capture(spec, now, capture_key)?;
            if capture_key.is_some() {
                task.phase = TaskPhase::WaitingCheckpoint;
            }
            branches.push(task);
        }
        Ok(branches)
    }

    pub fn live_fork(&self, source: &str, request_id: &str) -> anyhow::Result<LiveForkRecord> {
        self.live_forks
            .get(&(source.to_owned(), request_id.to_owned()))
            .cloned()
            .context("unknown live execution fork")
    }

    pub fn request_live_fork(
        &mut self,
        source: &str,
        request: ExecutionForkRequest,
        now: u64,
    ) -> anyhow::Result<LiveForkRecord> {
        self.reap(now)?;
        ensure!(
            request.version == CLUSTER_VERSION,
            "unsupported fork version"
        );
        ensure!(
            identifier(source)
                && identifier(&request.request_id)
                && identifier(&request.checkpoint_request_id),
            "invalid live fork identity"
        );
        if let Some(record) = self
            .live_forks
            .get(&(source.to_owned(), request.request_id.clone()))
        {
            ensure!(record.request == request, "live fork idempotency conflict");
            return Ok(record.clone());
        }
        ensure!(
            !self
                .forks
                .contains_key(&(source.to_owned(), request.request_id.clone())),
            "fork request id already belongs to sealed fork creation"
        );
        let parent = self.tasks.get(source).context("unknown live fork source")?;
        ensure!(
            !parent
                .controls
                .iter()
                .any(|c| c.command.request.request_id == request.checkpoint_request_id),
            "live fork requires a fresh checkpoint request id"
        );
        let control = self.prepare_control(
            source,
            ControlRequest {
                request_id: request.checkpoint_request_id.clone(),
                action: ControlAction::Checkpoint,
            },
            now,
        )?;
        ensure!(
            self.workers[&control.command.key.worker_id]
                .registration
                .execution_restore_protocol
                == Some(CLUSTER_VERSION),
            "live fork requires native restore support"
        );
        let branches =
            self.prepare_fork_branches(source, &request, now, Some(&control.command.key))?;
        let record = LiveForkRecord {
            version: CLUSTER_VERSION,
            source_task_id: source.to_owned(),
            source_key: control.command.key.clone(),
            request,
            phase: LiveForkPhase::Capturing,
            fork: None,
            error: None,
            created_at_ms: now,
            completed_at_ms: None,
        };
        let transaction = Transaction {
            version: CLUSTER_VERSION,
            changes: vec![Change::LiveForkRequested {
                record: Box::new(record.clone()),
                control,
                branches,
            }],
        };
        ensure!(
            serde_json::to_vec(&transaction)?.len() <= MAX_EXECUTION_FORK_BYTES,
            "live fork transaction exceeds 4 MiB"
        );
        self.commit(transaction.changes)?;
        Ok(record)
    }

    /// Derived state is part of the source acknowledgement/cancellation/finish
    /// frame, so replay can never expose a sealed observation without releasing
    /// the waiting branches, or admit branches before the matching observation.
    fn settle_live_fork(
        &mut self,
        source: &str,
        checkpoint: Option<pvisor_core::operation::ExecutionCheckpoint>,
        error: Option<String>,
        at: u64,
    ) {
        let Some(request_id) = self.pending_live_forks.remove(source) else {
            return;
        };
        let key = (source.to_owned(), request_id);
        let plan = self.live_forks.get_mut(&key).expect("pending live fork");
        plan.completed_at_ms = Some(at);
        let success = checkpoint.is_some();
        plan.phase = if success {
            LiveForkPhase::Ready
        } else {
            LiveForkPhase::Failed
        };
        plan.error = error;
        if let Some(checkpoint) = checkpoint {
            let record = ExecutionForkRecord {
                version: CLUSTER_VERSION,
                source_task_id: source.to_owned(),
                source_key: plan.source_key.clone(),
                request: plan.request.clone(),
                checkpoint,
                created_at_ms: at,
            };
            self.forks.insert(key, record.clone());
            plan.fork = Some(record);
        }
        for branch in &plan.request.branches {
            let task = self
                .tasks
                .get_mut(&branch.task_id)
                .expect("waiting fork branch");
            if task.phase != TaskPhase::WaitingCheckpoint {
                continue;
            }
            task.updated_at_ms = at;
            if success {
                self.indexes.set_phase(task, TaskPhase::Queued);
            } else {
                self.indexes.set_phase(task, TaskPhase::Failed);
                task.error = plan.error.clone();
            }
        }
    }

    pub fn register(
        &mut self,
        registration: WorkerRegistration,
        now: u64,
    ) -> anyhow::Result<WorkerRecord> {
        ensure!(
            registration
                .parked_execution_suspend_protocol
                .is_none_or(|v| v == CLUSTER_VERSION)
                && (registration.parked_execution_suspend_protocol.is_none()
                    || (registration.vm_control_protocol == Some(CLUSTER_VERSION)
                        && registration.execution.contains(&ExecutionClass {
                            executor: pvisor_core::ExecutorKind::VirtualMachine,
                            isolation: pvisor_core::IsolationKind::VirtualMachine,
                        })
                        && registration
                            .vm_control_actions
                            .contains(&ControlAction::Suspend))),
            "invalid parked execution suspension support"
        );
        ensure!(
            registration.cpu_observation_protocol.is_none_or(|v| (1
                ..=pvisor_core::cpu::CPU_OBSERVATION_PROTOCOL_VERSION)
                .contains(&v))
                && (registration.cpu_observation_protocol.is_none()
                    || registration.execution.contains(&ExecutionClass {
                        executor: pvisor_core::ExecutorKind::VirtualMachine,
                        isolation: pvisor_core::IsolationKind::VirtualMachine,
                    })),
            "invalid CPU observation capability"
        );
        ensure!(
            registration.cpu_qos_classes.len() <= 2
                && registration
                    .cpu_qos_classes
                    .iter()
                    .enumerate()
                    .all(|(i, class)| !registration.cpu_qos_classes[..i].contains(class))
                && (registration.cpu_qos_classes.is_empty()
                    || registration.execution.contains(&ExecutionClass {
                        executor: pvisor_core::ExecutorKind::VirtualMachine,
                        isolation: pvisor_core::IsolationKind::VirtualMachine,
                    })),
            "invalid CPU QoS capability"
        );
        self.reap(now)?;
        ensure!(
            registration.version == CLUSTER_VERSION,
            "unsupported cluster version"
        );
        ensure!(
            identifier(&registration.id) && identifier(&registration.incarnation),
            "invalid worker identity"
        );
        ensure!(
            registration.capacity.slots > 0
                && registration.capacity.memory_bytes > 0
                && registration.capacity.cpu_millis > 0,
            "worker capacity must be positive"
        );
        ensure!(
            !registration.execution.is_empty(),
            "worker advertises no execution classes"
        );
        if let Some(support) = &registration.environment_support {
            ensure!(
                support.version == CLUSTER_VERSION
                    && matches!(support.architecture.as_str(), "amd64" | "arm64"),
                "invalid environment capability"
            );
        }
        if let Some(support) = &registration.gateway {
            support.validate()?;
        }
        if let Some(support) = &registration.checkpoint_storage {
            support.validate()?;
            ensure!(
                registration.execution_restore_protocol == Some(CLUSTER_VERSION),
                "checkpoint repository requires native restore support"
            );
        }
        if let Some(support) = &registration.artifact_export {
            support.validate()?;
            ensure!(
                !support.execution_checkpoint
                    || registration
                        .checkpoint_storage
                        .as_ref()
                        .is_some_and(|storage| storage.publish)
                        && registration
                            .vm_control_actions
                            .contains(&ControlAction::Suspend),
                "checkpoint export requires a writable repository and native suspend support"
            );
            ensure!(
                registration.artifact_protocol == Some(CLUSTER_VERSION),
                "extended artifact export requires native Bundle protocol"
            );
        }
        ensure!(
            registration
                .execution_restore_protocol
                .is_none_or(|v| v == CLUSTER_VERSION)
                && (registration.execution_restore_protocol.is_none()
                    || registration
                        .vm_control_actions
                        .contains(&ControlAction::Checkpoint)),
            "invalid execution restore capability"
        );
        if let Some(worker) = self.workers.get(&registration.id) {
            ensure!(
                worker.registration.incarnation == registration.incarnation
                    || self.active[&registration.id].is_empty(),
                "worker incarnation still owns leases; wait for expiry"
            );
            ensure!(
                worker.reserved.fits(registration.capacity),
                "new capacity is below existing reservations"
            );
        }
        let id = registration.id.clone();
        self.commit(vec![Change::Register {
            registration,
            at: now,
        }])?;
        Ok(self.workers[&id].clone())
    }

    pub fn drain(&mut self, worker_id: &str, draining: bool) -> anyhow::Result<()> {
        ensure!(self.workers.contains_key(worker_id), "unknown worker");
        self.commit(vec![Change::Drain {
            worker_id: worker_id.to_owned(),
            draining,
        }])
    }

    pub fn decline(
        &mut self,
        rejection: AdmissionRejection,
        now: u64,
    ) -> anyhow::Result<TaskRecord> {
        self.reap(now)?;
        ensure!(
            !rejection.reason.is_empty() && rejection.reason.len() <= 1024,
            "invalid admission rejection"
        );
        let task = self
            .tasks
            .get(&rejection.key.task_id)
            .context("unknown task")?;
        if let Some(previous) = &task.last_admission_rejection
            && previous.key == rejection.key
        {
            ensure!(previous == &rejection, "conflicting admission rejection");
            return Ok(task.clone());
        }
        ensure!(
            task.phase == TaskPhase::Leased && self.valid_key(&rejection.key, now),
            "only an unstarted live assignment can be declined"
        );
        ensure!(
            task.admission_rejections < u64::MAX,
            "admission rejection count overflow"
        );
        let id = rejection.key.task_id.clone();
        self.commit(vec![Change::Decline { rejection, at: now }])?;
        Ok(self.tasks[&id].clone())
    }

    fn valid_key(&self, key: &LeaseKey, now: u64) -> bool {
        self.tasks.get(&key.task_id).is_some_and(|t| {
            !t.phase.terminal()
                && t.lease
                    .as_ref()
                    .is_some_and(|l| l.key == *key && l.expires_at_ms > now)
        })
    }

    pub fn recover(
        &mut self,
        request: RecoveryRequest,
        now: u64,
    ) -> anyhow::Result<RecoveryResponse> {
        self.reap(now)?;
        let worker = self
            .workers
            .get(&request.worker_id)
            .context("unknown worker")?;
        ensure!(
            worker.registration.incarnation == request.incarnation,
            "stale worker incarnation"
        );
        // Terminal entries can outnumber the live capacity after lost replies.
        // Only live exact keys are renewed; all other reservations stay unchanged.
        ensure!(
            request.completed.len() <= 4096,
            "recovery batch limit exceeded"
        );
        let mut seen = BTreeSet::new();
        let mut unique = BTreeSet::new();
        let mut renewed = Vec::new();
        let mut stop = Vec::new();
        for key in &request.completed {
            ensure!(
                key.worker_id == request.worker_id
                    && key.incarnation == request.incarnation
                    && identifier(&key.task_id)
                    && key.generation > 0
                    && unique.insert((key.task_id.clone(), key.generation)),
                "invalid or duplicate recovery key"
            );
            seen.insert(key.task_id.clone());
            if self.valid_key(key, now) {
                renewed.push(key.clone());
                if self.tasks[&key.task_id].phase == TaskPhase::Cancelling {
                    stop.push(key.clone());
                }
            } else {
                stop.push(key.clone());
            }
        }
        let expires = now
            .checked_add(self.config.lease_duration_ms)
            .context("time overflow")?;
        self.commit(vec![Change::Renew {
            worker_id: request.worker_id,
            at: now,
            expires,
            keys: renewed.clone(),
            acknowledged: seen,
            admission: None,
        }])?;
        // No queue scan, assignment redelivery, unseen-key renewal or control issue.
        Ok(RecoveryResponse {
            version: CLUSTER_VERSION,
            lease_duration_ms: self.config.lease_duration_ms,
            renewed,
            stop,
        })
    }

    pub fn poll(&mut self, mut request: PollRequest, now: u64) -> anyhow::Result<PollResponse> {
        self.reap(now)?;
        ensure!(
            request.max_assignments <= self.config.max_batch,
            "batch limit exceeded"
        );
        let worker = self
            .workers
            .get(&request.worker_id)
            .context("unknown worker")?;
        ensure!(
            worker.registration.incarnation == request.incarnation,
            "stale worker incarnation"
        );
        ensure!(
            request.active.len()
                <= worker.registration.capacity.slots as usize + MAX_ARTIFACT_DELIVERIES
                && request
                    .active
                    .iter()
                    .filter(|key| {
                        self.tasks.get(&key.task_id).is_none_or(|task| {
                            task.result.is_none()
                                || task.lease.as_ref().is_none_or(|lease| lease.key != **key)
                        })
                    })
                    .count()
                    <= worker.registration.capacity.slots as usize,
            "too many active leases"
        );
        if let Some(report) = &mut request.admission {
            report.validate()?;
            ensure!(
                report.available == request.available,
                "inconsistent admission report"
            );
            ensure!(
                report.available.fits(worker.registration.capacity),
                "reported availability exceeds worker capacity"
            );
            // A stale sample never authorizes new work, but renewal/teardown
            // must continue even if the node probe is no longer responsive.
            if report.mode == AdmissionMode::LinuxPressure
                && report.sample_age_ms >= self.config.lease_duration_ms
            {
                request.available = Resources::default();
                report.available = Resources::default();
                if !report.blocked.contains(&AdmissionBlock::StaleSample) {
                    report.blocked.push(AdmissionBlock::StaleSample);
                }
            }
        }
        let mut seen = BTreeSet::new();
        let mut renewed = Vec::new();
        let mut stop = Vec::new();
        for key in &request.active {
            ensure!(
                key.worker_id == request.worker_id
                    && key.incarnation == request.incarnation
                    && seen.insert(key.task_id.clone()),
                "invalid or duplicate active lease"
            );
            if self.valid_key(key, now) {
                renewed.push(key.clone());
                if self.tasks[&key.task_id].phase == TaskPhase::Cancelling {
                    stop.push(key.clone());
                }
            } else {
                stop.push(key.clone());
            }
        }
        let expires = now
            .checked_add(self.config.lease_duration_ms)
            .context("time overflow")?;
        let mut renew_keys = renewed.clone();
        for id in &self.active[&request.worker_id] {
            let task = &self.tasks[id];
            if task.phase == TaskPhase::Leased && !seen.contains(id) {
                renew_keys.push(task.lease.as_ref().unwrap().key.clone());
            }
        }
        self.commit(vec![Change::Renew {
            worker_id: request.worker_id.clone(),
            at: now,
            expires,
            keys: renew_keys,
            acknowledged: seen.clone(),
            admission: request.admission.clone().map(Box::new),
        }])?;

        // Lost assignment responses are redelivered with the same fencing key.
        // No extra reservation and no second execution for a known key.
        let mut assignments = Vec::new();
        for id in &self.active[&request.worker_id] {
            let task = &self.tasks[id];
            if task.phase == TaskPhase::Leased && !seen.contains(id) {
                assignments.push(Assignment {
                    checkpoint_publication: self.task_checkpoint_publication(&task.spec),
                    spec: task.spec.clone(),
                    lease: task.lease.clone().unwrap(),
                    environment: self.task_environment(&task.spec),
                    checkpoint: self.task_checkpoint(&task.spec)?,
                });
            }
        }
        let (controls, available) = self.issue_controls(&request, &renewed, now)?;
        let worker = &self.workers[&request.worker_id];
        if worker.draining {
            return Ok(PollResponse {
                version: CLUSTER_VERSION,
                lease_duration_ms: self.config.lease_duration_ms,
                assignments,
                renewed,
                stop,
                controls,
            });
        }
        let mut budget = worker
            .registration
            .capacity
            .checked_sub(worker.reserved)
            .expect("worker capacity");
        budget.slots = budget.slots.min(available.slots);
        budget.memory_bytes = budget.memory_bytes.min(available.memory_bytes);
        budget.cpu_millis = budget.cpu_millis.min(available.cpu_millis);
        let registration = worker.registration.clone();
        let cached: BTreeSet<_> = registration.cache_keys.iter().collect();
        // Rotate the window even if nothing fits: large or incompatible tasks
        // cannot indefinitely hide later work. Equal scores retain queue order.
        let mut window = Vec::new();
        for _ in 0..self.indexes.ready.len().min(self.config.queue_lookahead) {
            let id = self.indexes.ready.pop_front().unwrap();
            if self.tasks[id.as_ref()].phase == TaskPhase::Queued {
                window.push(id);
            }
        }
        window.sort_by_key(|id| {
            std::cmp::Reverse(
                self.tasks[id.as_ref()]
                    .spec
                    .cache_keys
                    .iter()
                    .filter(|k| cached.contains(k))
                    .count()
                    + self.tasks[id.as_ref()]
                        .spec
                        .environment
                        .as_ref()
                        .map_or(0, |digest| {
                            self.environments[digest]
                                .template
                                .layers()
                                .filter(|layer| cached.contains(&layer.handle))
                                .count()
                        }),
            )
        });
        let artifact_headroom = self.artifacts.storage_has_headroom();
        let mut changes = Vec::new();
        let mut tenant_delta: BTreeMap<String, Resources> = BTreeMap::new();
        for id in &window {
            let task = &self.tasks[id.as_ref()];
            if assignments.len() >= request.max_assignments as usize {
                break;
            }
            let spec = &task.spec;
            let reserved = self
                .tenant_reserved
                .get(&spec.tenant)
                .copied()
                .unwrap_or_default();
            let delta = tenant_delta.get(&spec.tenant).copied().unwrap_or_default();
            let tenant_next = reserved
                .checked_add(delta)
                .and_then(|r| r.checked_add(spec.resources));
            let quota_fits = tenant_next.is_some_and(|r| {
                self.config
                    .tenant_quotas
                    .get(&spec.tenant)
                    .is_none_or(|q| r.fits(*q))
            });
            if !spec.resources.fits(budget)
                || (spec.requires_artifacts() && !artifact_headroom)
                || spec.retain_artifacts.as_ref().is_some_and(|retention| {
                    registration
                        .artifact_export
                        .as_ref()
                        .is_none_or(|support| !support.satisfies(retention))
                })
                || spec.gateway.as_ref().is_some_and(|requirement| {
                    registration
                        .gateway
                        .as_ref()
                        .is_none_or(|support| !support.satisfies(requirement))
                })
                || spec
                    .cpu_qos
                    .is_some_and(|class| !registration.cpu_qos_classes.contains(&class))
                || !self.restore_fits(spec, &registration)?
                || spec
                    .retain_artifacts
                    .as_ref()
                    .and_then(|retention| retention.execution_checkpoint.as_ref())
                    .is_some_and(|requirement| {
                        registration
                            .checkpoint_storage
                            .as_ref()
                            .is_none_or(|support| {
                                !support.publish || support.repository != requirement.repository
                            })
                    })
                || !quota_fits
                || (spec.requires_artifacts()
                    && registration.artifact_protocol != Some(CLUSTER_VERSION))
                || spec.environment.as_ref().is_some_and(|digest| {
                    registration
                        .environment_support
                        .as_ref()
                        .is_none_or(|support| {
                            support.version != CLUSTER_VERSION
                                || support.architecture
                                    != self.environments[digest].template.architecture
                        })
                })
                || !registration.execution.contains(&spec.execution)
                || !spec
                    .labels
                    .iter()
                    .all(|(k, v)| registration.labels.get(k) == Some(v))
            {
                continue;
            }
            let lease = Lease {
                key: LeaseKey {
                    task_id: id.to_string(),
                    worker_id: registration.id.clone(),
                    incarnation: registration.incarnation.clone(),
                    generation: task
                        .generation
                        .checked_add(1)
                        .context("generation overflow")?,
                },
                expires_at_ms: expires,
            };
            budget = budget.checked_sub(spec.resources).unwrap();
            tenant_delta.insert(
                spec.tenant.clone(),
                delta.checked_add(spec.resources).unwrap(),
            );
            assignments.push(Assignment {
                checkpoint_publication: self.task_checkpoint_publication(spec),
                spec: spec.clone(),
                lease: lease.clone(),
                environment: self.task_environment(spec),
                checkpoint: self.task_checkpoint(spec)?,
            });
            changes.push(Change::Assign {
                lease,
                at: now,
                artifact_pin_protocol: Some(CLUSTER_VERSION),
            });
        }
        // Restore queue on storage failure. Queue order is advisory; leases are durable.
        let committed = self.commit(changes);
        for id in window {
            if self.tasks[id.as_ref()].phase == TaskPhase::Queued {
                self.indexes.ready.push_back(id);
            }
        }
        committed?;
        Ok(PollResponse {
            version: CLUSTER_VERSION,
            lease_duration_ms: self.config.lease_duration_ms,
            assignments,
            renewed,
            stop,
            controls,
        })
    }

    pub fn request_control(
        &mut self,
        id: &str,
        request: ControlRequest,
        now: u64,
    ) -> anyhow::Result<ControlRecord> {
        self.reap(now)?;
        let record = self.prepare_control(id, request, now)?;
        if !self.tasks[id]
            .controls
            .iter()
            .any(|c| c.command.request.request_id == record.command.request.request_id)
        {
            self.commit(vec![Change::ControlRequested {
                task_id: id.into(),
                record: record.clone(),
            }])?;
        }
        Ok(record)
    }

    fn prepare_control(
        &self,
        id: &str,
        request: ControlRequest,
        now: u64,
    ) -> anyhow::Result<ControlRecord> {
        ensure!(
            identifier(&request.request_id),
            "invalid control request id"
        );
        let task = self.tasks.get(id).context("unknown task")?;
        if let Some(record) = task
            .controls
            .iter()
            .find(|r| r.command.request.request_id == request.request_id)
        {
            ensure!(
                record.command.request == request,
                "control idempotency conflict"
            );
            return Ok(record.clone());
        }
        let lease = task.lease.as_ref().context("task has no active lease")?;
        ensure!(
            !task.phase.terminal()
                && !matches!(
                    task.phase,
                    TaskPhase::Cancelling | TaskPhase::Suspending | TaskPhase::RetainingArtifacts
                )
                && self.valid_key(&lease.key, now),
            "task is no longer controllable"
        );
        ensure!(
            task.spec.execution.executor == pvisor_core::ExecutorKind::VirtualMachine
                && task.spec.execution.isolation == pvisor_core::IsolationKind::VirtualMachine
                && self.workers[&lease.key.worker_id]
                    .registration
                    .vm_control_protocol
                    == Some(CLUSTER_VERSION),
            "worker does not support VM controls"
        );
        ensure!(
            self.workers[&lease.key.worker_id]
                .registration
                .vm_control_actions
                .contains(&request.action),
            "worker does not support this VM control action"
        );
        ensure!(
            task.controls.last().is_none_or(|r| r.phase.terminal()),
            "another control is still pending"
        );
        ensure!(
            request.action != ControlAction::Checkpoint || task.phase == TaskPhase::Running,
            "execution checkpoint requires a running task"
        );
        ensure!(
            request.action != ControlAction::Suspend
                || matches!(
                    task.phase,
                    TaskPhase::Running | TaskPhase::Paused | TaskPhase::Offloaded
                ),
            "execution suspension requires a running, paused or offloaded task"
        );
        ensure!(
            request.action != ControlAction::Suspend
                || task.phase == TaskPhase::Running
                || self.workers[&lease.key.worker_id]
                    .registration
                    .parked_execution_suspend_protocol
                    == Some(CLUSTER_VERSION),
            "worker does not support suspending paused/offloaded execution"
        );
        ensure!(
            request.action != ControlAction::Offload || task.spec.restore.is_none(),
            "restored private COW RAM does not support writable-backing offload"
        );
        ensure!(
            task.controls.len() < 4096,
            "control history retention limit reached"
        );
        let record = ControlRecord {
            command: ControlCommand {
                key: lease.key.clone(),
                revision: task.controls.len() as u64 + 1,
                request,
            },
            phase: ControlPhase::Pending,
            outcome: None,
            requested_at_ms: now,
            issued_at_ms: None,
            completed_at_ms: None,
            admission: Resources::default(),
        };
        Ok(record)
    }

    fn issue_controls(
        &mut self,
        request: &PollRequest,
        renewed: &[LeaseKey],
        now: u64,
    ) -> anyhow::Result<(Vec<ControlCommand>, Resources)> {
        let worker = &self.workers[&request.worker_id];
        let mut available = request.available;
        // Issued commands may have had their response lost, so this heartbeat
        // need not include their new local charges yet. Account for all of them
        // before admitting pending resumes or new assignments in the same poll.
        for key in renewed {
            let task = &self.tasks[&key.task_id];
            if task.phase != TaskPhase::Cancelling
                && let Some(record) = task
                    .controls
                    .last()
                    .filter(|c| c.phase == ControlPhase::Issued)
            {
                available = available.saturating_sub(record.admission);
            }
        }
        let mut budget = worker
            .registration
            .capacity
            .checked_sub(worker.reserved)
            .expect("worker capacity");
        budget.slots = budget.slots.min(available.slots);
        budget.memory_bytes = budget.memory_bytes.min(available.memory_bytes);
        budget.cpu_millis = budget.cpu_millis.min(available.cpu_millis);
        let mut controls = Vec::new();
        let mut changes = Vec::new();
        let mut tenant_delta = BTreeMap::<String, Resources>::new();
        for key in renewed {
            let task = &self.tasks[&key.task_id];
            if task.phase == TaskPhase::Cancelling {
                continue;
            }
            let Some(record) = task.controls.last().filter(|c| !c.phase.terminal()) else {
                continue;
            };
            if record.phase == ControlPhase::Issued {
                controls.push(record.command.clone());
                continue;
            }
            let mut reserved = task.current_reservation();
            if record.command.request.action == ControlAction::Resume {
                let additional = task
                    .spec
                    .resources
                    .checked_sub(reserved)
                    .context("invalid resume budget")?;
                let previous_delta = tenant_delta
                    .get(&task.spec.tenant)
                    .copied()
                    .unwrap_or_default();
                let tenant_next = self.tenant_reserved[&task.spec.tenant]
                    .checked_add(previous_delta)
                    .and_then(|r| r.checked_add(additional));
                if !additional.fits(budget)
                    || !tenant_next.is_some_and(|r| {
                        self.config
                            .tenant_quotas
                            .get(&task.spec.tenant)
                            .is_none_or(|q| r.fits(*q))
                    })
                {
                    continue;
                }
                budget = budget.checked_sub(additional).unwrap();
                available = available.checked_sub(additional).unwrap();
                tenant_delta.insert(
                    task.spec.tenant.clone(),
                    previous_delta.checked_add(additional).unwrap(),
                );
                reserved = task.spec.resources;
            }
            controls.push(record.command.clone());
            changes.push(Change::ControlIssued {
                task_id: key.task_id.clone(),
                revision: record.command.revision,
                reserved,
                at: now,
            });
        }
        self.commit(changes)?;
        Ok((controls, available))
    }

    pub fn acknowledge_control(
        &mut self,
        acknowledgement: ControlAcknowledgement,
        now: u64,
    ) -> anyhow::Result<ControlRecord> {
        self.reap(now)?;
        acknowledgement
            .outcome
            .validate(acknowledgement.command.request.action)?;
        let key = &acknowledgement.command.key;
        let task = self.tasks.get(&key.task_id).context("unknown task")?;
        if let ControlOutcome::Checkpointed { checkpoint } = &acknowledgement.outcome {
            ensure!(
                checkpoint.source_run_id == task.spec.run.run_id.as_str(),
                "checkpoint belongs to another native Run"
            );
        }
        let record = task
            .controls
            .iter()
            .find(|r| r.command == acknowledgement.command)
            .context("unknown or stale control")?;
        if record.phase == ControlPhase::Succeeded || record.phase == ControlPhase::Failed {
            ensure!(
                record.outcome.as_ref() == Some(&acknowledgement.outcome),
                "conflicting control acknowledgement"
            );
            return Ok(record.clone());
        }
        ensure!(
            record.phase == ControlPhase::Issued
                && task.phase != TaskPhase::Cancelling
                && self.valid_key(key, now),
            "stale or expired control acknowledgement"
        );
        let reserved = acknowledgement
            .outcome
            .reservation(task.spec.resources, task.current_reservation());
        let revision = acknowledgement.command.revision;
        let id = key.task_id.clone();
        self.commit(vec![Change::ControlAcknowledged {
            acknowledgement,
            reserved,
            at: now,
        }])?;
        Ok(self.tasks[&id]
            .controls
            .iter()
            .find(|r| r.command.revision == revision)
            .unwrap()
            .clone())
    }

    pub fn cancel(&mut self, id: &str, now: u64) -> anyhow::Result<TaskRecord> {
        self.reap(now)?;
        let task = self.tasks.get(id).context("unknown task")?;
        if !task.phase.terminal() && task.phase != TaskPhase::Cancelling {
            self.commit(vec![Change::Cancel {
                task_id: id.into(),
                at: now,
            }])?;
        }
        Ok(self.tasks[id].clone())
    }

    pub fn complete(&mut self, completion: Completion, now: u64) -> anyhow::Result<TaskRecord> {
        if let Some(receipt) = self.completion_receipt(&completion)? {
            return Ok(receipt);
        }
        let verified = completion
            .artifacts
            .as_ref()
            .map(|reference| self.artifacts.verify(reference, &completion.key))
            .transpose()?;
        self.complete_verified(completion, verified, now)
    }

    /// Exact terminal receipts do not require evidence bodies to still exist.
    pub(crate) fn completion_receipt(
        &self,
        completion: &Completion,
    ) -> anyhow::Result<Option<TaskRecord>> {
        let task = self
            .tasks
            .get(&completion.key.task_id)
            .context("unknown task")?;
        if !task.phase.terminal() {
            return Ok(None);
        }
        ensure!(
            task.phase != TaskPhase::Lost
                && task.lease.as_ref().is_some_and(|l| l.key == completion.key)
                && serde_json::to_value(&task.result)? == serde_json::to_value(&completion.result)?
                && task.error == completion.error
                && task.artifacts == completion.artifacts
                && task.artifact_error == completion.artifact_error,
            "stale or conflicting completion"
        );
        Ok(Some(task.clone()))
    }

    pub(crate) fn artifact_gc_snapshot(
        &mut self,
        request: &ArtifactGcRequest,
        now: u64,
    ) -> anyhow::Result<crate::artifacts::gc::Snapshot> {
        request.validate()?;
        self.reap(now)?;
        let retire: Vec<_> = self
            .retention_age
            .iter()
            .take_while(|(at, _)| request.retire_before_ms.is_some_and(|cutoff| *at < cutoff))
            .take(256)
            .map(|(at, id)| ArtifactRetirement {
                task_id: id.clone(),
                generation: self.tasks[id].generation,
                reference: self.retained[id].clone(),
                finished_at_ms: *at,
            })
            .collect();
        let retired: BTreeSet<_> = retire.iter().map(|r| r.task_id.clone()).collect();
        let mut live = BTreeSet::new();
        let mut legacy = false;
        for ids in self.active.values() {
            for id in ids {
                let task = &self.tasks[id];
                if let Some(lease) = &task.lease {
                    live.insert(crate::artifacts::gc::lease_id(&lease.key));
                }
                legacy |= task.artifact_pin_protocol != Some(CLUSTER_VERSION);
            }
        }
        Ok(crate::artifacts::gc::Snapshot {
            live,
            retire,
            legacy,
            retained: self
                .retained
                .iter()
                .filter(|(id, _)| !retired.contains(id.as_str()))
                .map(|(_, r)| r.clone())
                .collect(),
        })
    }
    pub(crate) fn retire_artifacts(
        &mut self,
        entries: &[ArtifactRetirement],
        now: u64,
    ) -> anyhow::Result<BTreeSet<String>> {
        self.reap(now)?;
        for entry in entries {
            let task = self
                .tasks
                .get(&entry.task_id)
                .context("unknown retirement task")?;
            ensure!(
                task.phase.terminal()
                    && task.generation == entry.generation
                    && task.artifacts.as_ref() == Some(&entry.reference)
                    && task.updated_at_ms == entry.finished_at_ms,
                "artifact retirement plan is stale"
            );
        }
        let pending: Vec<_> = entries
            .iter()
            .filter(|e| self.tasks[&e.task_id].artifact_retired_at_ms.is_none())
            .cloned()
            .collect();
        if !pending.is_empty() {
            self.commit(vec![Change::ArtifactsRetired {
                entries: pending,
                at: now,
            }])?;
        }
        Ok(self
            .active
            .values()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.tasks[id].lease.as_ref())
            .map(|lease| crate::artifacts::gc::lease_id(&lease.key))
            .collect())
    }

    pub fn artifact_store(&self) -> crate::artifacts::ArtifactStore {
        self.artifacts.clone()
    }

    pub fn authorize_artifact_upload(&mut self, key: &LeaseKey, now: u64) -> anyhow::Result<()> {
        self.reap(now)?;
        ensure!(
            self.valid_key(key, now),
            "artifact upload requires a live lease"
        );
        Ok(())
    }

    pub fn native_done(
        &mut self,
        request: NativeDone,
        now: u64,
    ) -> anyhow::Result<NativeDoneReceipt> {
        ensure!(
            request.version == ARTIFACT_DELIVERY_VERSION,
            "unsupported artifact delivery protocol"
        );
        let key = request.key.clone();
        self.complete_inner(
            Completion {
                key: request.key,
                result: Some(request.result),
                error: None,
                artifacts: None,
                artifact_error: None,
            },
            None,
            now,
            true,
        )?;
        Ok(NativeDoneReceipt {
            version: ARTIFACT_DELIVERY_VERSION,
            reserved: self.tasks[&key.task_id].current_reservation(),
            key,
        })
    }

    pub(crate) fn complete_verified(
        &mut self,
        completion: Completion,
        verified: Option<crate::artifacts::VerifiedArtifacts>,
        now: u64,
    ) -> anyhow::Result<TaskRecord> {
        if let Some(receipt) = self.completion_receipt(&completion)? {
            return Ok(receipt);
        }
        self.complete_inner(completion, verified, now, false)
    }

    fn complete_inner(
        &mut self,
        completion: Completion,
        verified: Option<crate::artifacts::VerifiedArtifacts>,
        now: u64,
        native_only: bool,
    ) -> anyhow::Result<TaskRecord> {
        ensure!(
            match (&completion.artifacts, &verified) {
                (None, None) => true,
                (Some(reference), Some(verified)) => {
                    verified.matches(reference, &completion.key, completion.result.as_ref())
                }
                _ => false,
            },
            "unverified artifact manifest"
        );
        self.reap(now)?;
        let task = self
            .tasks
            .get(&completion.key.task_id)
            .context("unknown task")?;
        if let Some(retention) = &task.spec.retain_artifacts {
            ensure!(
                verified.as_ref().is_none_or(|v| v.satisfies(retention)),
                "retained artifacts omit requested trace or writable layer"
            );
            ensure!(
                retention.execution_checkpoint.is_none()
                    || verified.as_ref().is_none_or(|v| v.checkpoint.is_some()),
                "retained artifacts omit a valid checkpoint publication"
            );
        }
        if let Some(publication) = verified.as_ref().and_then(|v| v.checkpoint.as_ref()) {
            let requirement = task
                .spec
                .retain_artifacts
                .as_ref()
                .and_then(|r| r.execution_checkpoint.as_ref())
                .context("unrequested checkpoint publication")?;
            let receipt = pvisor_core::operation::ExecutionSuspension::from_result(
                completion
                    .result
                    .as_ref()
                    .context("checkpoint publication has no native result")?,
            )?;
            ensure!(
                publication.repository == requirement.repository
                    && publication.checkpoint == receipt.checkpoint,
                "checkpoint publication contradicts native suspension or repository requirement"
            );
        }
        if native_only {
            ensure!(
                task.spec.requires_artifacts() && !task.phase.terminal(),
                "native handoff requires pending artifact delivery"
            );
        }
        if !task.phase.terminal()
            && let Some(native) = &task.result
        {
            ensure!(
                serde_json::to_value(native)? == serde_json::to_value(&completion.result)?,
                "completion contradicts durable native terminal result"
            );
        }
        if task.phase.terminal() {
            ensure!(
                task.phase != TaskPhase::Lost
                    && task.lease.as_ref().is_some_and(|l| l.key == completion.key)
                    && serde_json::to_value(&task.result)?
                        == serde_json::to_value(&completion.result)?
                    && task.error == completion.error
                    && task.artifacts == completion.artifacts
                    && task.artifact_error == completion.artifact_error,
                "stale or conflicting completion"
            );
            return Ok(task.clone());
        }
        ensure!(
            self.valid_key(&completion.key, now),
            "stale or expired lease"
        );
        ensure!(
            completion.result.is_some() != completion.error.is_some(),
            "completion needs exactly one result or error"
        );
        if let Some(error) = &completion.artifact_error {
            ensure!(
                !error.is_empty() && error.len() <= 8192 && completion.artifacts.is_none(),
                "invalid artifact export error"
            );
        }
        ensure!(
            native_only
                || !task.spec.requires_artifacts()
                || completion.result.is_none()
                || completion.artifacts.is_some()
                || completion.artifact_error.is_some(),
            "required Run Bundle needs retention or an explicit export failure"
        );
        if let Some(result) = &completion.result {
            ensure!(
                result.run_id == task.spec.run.run_id,
                "result belongs to a different Run"
            );
            if let Some(cpu) = &result.executor_observations.cpu_usage {
                cpu.validate()?;
                ensure!(
                    task.spec.execution
                        == ExecutionClass {
                            executor: pvisor_core::ExecutorKind::VirtualMachine,
                            isolation: pvisor_core::IsolationKind::VirtualMachine,
                        },
                    "final CPU observation requires a native VM"
                );
                if let Some(binding) = self.cpu_bindings.get(&completion.key.task_id) {
                    ensure!(
                        binding.key == completion.key && binding.attempt == result.attempt_id,
                        "final CPU observation Attempt differs from live CPU binding"
                    );
                    if let (
                        Some(previous),
                        pvisor_core::cpu::TerminalCpuUsage::Measured { usage },
                    ) = (&binding.usage, cpu)
                    {
                        usage.interval_since(previous)?;
                    }
                }
                if let Some(binding) = self.memory_bindings.get(&completion.key.task_id) {
                    ensure!(
                        binding.key == completion.key && binding.attempt == result.attempt_id,
                        "final CPU observation Attempt differs from live memory binding"
                    );
                    if let (
                        Some((pid, start)),
                        pvisor_core::cpu::TerminalCpuUsage::Measured { usage },
                    ) = (binding.process, cpu)
                    {
                        ensure!(
                            pid == usage.pid && start == usage.start_time_ticks,
                            "final CPU observation process differs from live memory binding"
                        );
                    }
                }
            }
            ensure!(
                matches!(
                    result.state,
                    pvisor_core::RunState::Completed
                        | pvisor_core::RunState::Failed
                        | pvisor_core::RunState::Cancelled
                        | pvisor_core::RunState::Hibernated
                ),
                "result must be terminal"
            );
            ensure!(
                result.output.stdout.as_ref().map_or(0, |s| s.len())
                    <= task.spec.run.runtime.max_output_bytes
                    && result.output.stderr.as_ref().map_or(0, |s| s.len())
                        <= task.spec.run.runtime.max_output_bytes,
                "result exceeds output limit"
            );
        }
        if native_only && task.result.is_some() {
            return Ok(task.clone());
        }
        if native_only {
            ensure!(
                task.controls.iter().all(|record| record.phase.terminal()
                    || (record.phase == ControlPhase::Issued
                        && record.command.request.action == ControlAction::Suspend
                        && completion.result.as_ref().is_some_and(
                            |result| result.state == pvisor_core::RunState::Hibernated
                        ))),
                "native handoff requires settled control observations"
            );
        }
        let mut changes = Vec::new();
        if let Some(result) = completion
            .result
            .as_ref()
            .filter(|r| r.state == pvisor_core::RunState::Hibernated)
        {
            let receipt = pvisor_core::operation::ExecutionSuspension::from_result(result)?;
            let record = task
                .controls
                .iter()
                .find(|record| record.command.request.request_id == receipt.request_id)
                .context("hibernation has no issued suspend command")?;
            ensure!(
                record.command.key == completion.key
                    && record.command.request.action == ControlAction::Suspend,
                "hibernation belongs to another suspend command or lease"
            );
            let outcome = ControlOutcome::Checkpointed {
                checkpoint: receipt.checkpoint,
            };
            outcome.validate(ControlAction::Suspend)?;
            match record.phase {
                ControlPhase::Succeeded => ensure!(
                    record.outcome.as_ref() == Some(&outcome),
                    "hibernation contradicts acknowledged checkpoint"
                ),
                ControlPhase::Issued if task.phase != TaskPhase::Cancelling => {
                    changes.push(Change::ControlAcknowledged {
                        acknowledgement: ControlAcknowledgement {
                            command: record.command.clone(),
                            outcome,
                        },
                        reserved: task.current_reservation(),
                        at: now,
                    })
                }
                ControlPhase::Aborted if task.phase == TaskPhase::Cancelling => {}
                _ => anyhow::bail!("hibernation requires an issued or acknowledged suspend"),
            }
        }
        if native_only {
            let reserved = artifact_delivery_reservation(task.current_reservation())
                .context("execution reservation too small for bounded artifact delivery")?;
            ensure!(
                self.active[&completion.key.worker_id]
                    .iter()
                    .filter(|id| self.tasks[*id].result.is_some())
                    .count()
                    < MAX_ARTIFACT_DELIVERIES,
                "artifact delivery concurrency limit reached"
            );
            let id = completion.key.task_id;
            changes.push(Change::NativeDone {
                task_id: id.clone(),
                result: Box::new(completion.result.context("missing native result")?),
                reserved,
                at: now,
            });
            self.commit(changes)?;
            return Ok(self.tasks[&id].clone());
        }
        let phase =
            if task.phase == TaskPhase::Cancelling {
                TaskPhase::Cancelled
            } else if completion
                .result
                .as_ref()
                .is_some_and(|r| r.state == pvisor_core::RunState::Hibernated)
                && (!task.spec.requires_artifacts() || completion.artifacts.is_some())
            {
                TaskPhase::Suspended
            } else if completion.result.as_ref().is_some_and(|r| {
                r.state == pvisor_core::RunState::Completed && r.exit_code == Some(0)
            }) && (!task.spec.requires_artifacts() || completion.artifacts.is_some())
            {
                TaskPhase::Succeeded
            } else {
                TaskPhase::Failed
            };
        let id = completion.key.task_id;
        changes.push(Change::Finish {
            checkpoint_publication: verified.as_ref().and_then(|v| v.checkpoint.clone()),
            task_id: id.clone(),
            phase,
            result: completion.result.map(Box::new),
            error: completion.error,
            artifacts: completion.artifacts,
            artifact_error: completion.artifact_error,
            at: now,
        });
        self.commit(changes)?;
        if let Some(verified) = verified {
            self.retained_pins.insert(id.clone(), verified.pins);
        }
        Ok(self.tasks[&id].clone())
    }

    pub fn reap(&mut self, now: u64) -> anyhow::Result<usize> {
        let changes: Vec<_> = self
            .expiry
            .range(..=(now, String::from("\u{10ffff}")))
            .map(|(_, id)| Change::LeaseExpired {
                task_id: id.clone(),
                at: now,
            })
            .collect();
        let count = changes.len();
        self.commit(changes)?;
        Ok(count)
    }

    pub fn task(&self, id: &str) -> anyhow::Result<TaskRecord> {
        self.tasks.get(id).cloned().context("unknown task")
    }

    /// Bounded ephemeral observations: do not renew leases, write WAL records,
    /// alter reservations or admit work based on RSS/PSS alone.
    pub fn report_memory(
        &mut self,
        request: MemoryReportRequest,
        now: u64,
    ) -> anyhow::Result<MemoryReportReceipt> {
        ensure!(
            !request.samples.is_empty() && request.samples.len() <= 64,
            "memory report needs 1..64 samples"
        );
        let worker = self
            .workers
            .get(&request.worker_id)
            .context("unknown worker")?;
        ensure!(
            worker.registration.incarnation == request.incarnation,
            "stale worker incarnation"
        );
        let mut seen = BTreeSet::new();
        // Validate the entire batch before publishing any observation.
        for report in &request.samples {
            ensure!(
                report.key.worker_id == request.worker_id
                    && report.key.incarnation == request.incarnation
                    && seen.insert(&report.key.task_id),
                "invalid or duplicate memory report lease"
            );
            report.sample.validate()?;
            ensure!(report.sequence > 0, "invalid memory sample sequence");
            if self.valid_key(&report.key, now) && self.tasks[&report.key.task_id].result.is_none()
            {
                let task = &self.tasks[&report.key.task_id];
                ensure!(
                    task.spec.execution.executor == pvisor_core::ExecutorKind::VirtualMachine
                        && task.spec.run.run_id == report.sample.run_id,
                    "memory report does not match assigned VM Run"
                );
                if let Some(binding) = self.cpu_bindings.get(&report.key.task_id) {
                    ensure!(
                        binding.key == report.key && binding.attempt == report.sample.attempt_id,
                        "memory and CPU report Attempt identities differ"
                    );
                    if let (Some(cpu), Some(memory)) = (&binding.usage, &report.sample.usage) {
                        ensure!(
                            cpu.pid == memory.pid
                                && cpu.start_time_ticks == memory.start_time_ticks,
                            "memory and CPU report process identities differ"
                        );
                    }
                }
                if let Some(binding) = self.memory_bindings.get(&report.key.task_id) {
                    ensure!(
                        binding.key == report.key && binding.attempt == report.sample.attempt_id,
                        "memory report Attempt changed within live lease"
                    );
                    if let (Some((pid, start)), Some(new)) = (binding.process, &report.sample.usage)
                    {
                        ensure!(
                            pid == new.pid && start == new.start_time_ticks,
                            "memory report native process changed within live Attempt"
                        );
                    }
                }
                if let Some(previous) = &task.memory_sample {
                    ensure!(
                        report.sequence != previous.report.sequence || report == &previous.report,
                        "conflicting memory sample sequence"
                    );
                }
            }
        }
        let mut receipt = MemoryReportReceipt {
            accepted: vec![],
            ignored: vec![],
        };
        for report in request.samples {
            if self.valid_key(&report.key, now) && self.tasks[&report.key.task_id].result.is_none()
            {
                if let Some(previous) = &self.tasks[&report.key.task_id].memory_sample {
                    if report.sequence < previous.report.sequence {
                        receipt.ignored.push(report.key);
                        continue;
                    }
                    if report.sequence == previous.report.sequence {
                        receipt.accepted.push(report.key);
                        continue;
                    }
                }
                receipt.accepted.push(report.key.clone());
                let binding = self
                    .memory_bindings
                    .entry(report.key.task_id.clone())
                    .or_insert_with(|| MemoryBinding {
                        key: report.key.clone(),
                        attempt: report.sample.attempt_id.clone(),
                        process: None,
                    });
                if let Some(usage) = &report.sample.usage {
                    binding.process = Some((usage.pid, usage.start_time_ticks));
                }
                let task = self.tasks.get_mut(&report.key.task_id).unwrap();
                task.memory_sample = Some(ReceivedMemorySample {
                    report,
                    received_at_ms: now,
                });
            } else {
                receipt.ignored.push(report.key);
            }
        }
        Ok(receipt)
    }
    /// Cheap cumulative CPU counters and intervals, independent of admission,
    /// heartbeat and the journal. Validate a complete batch before publishing.
    pub fn report_cpu(
        &mut self,
        request: CpuReportRequest,
        now: u64,
    ) -> anyhow::Result<CpuReportReceipt> {
        ensure!(
            !request.samples.is_empty() && request.samples.len() <= 64,
            "CPU report needs 1..64 samples"
        );
        let worker = self
            .workers
            .get(&request.worker_id)
            .context("unknown worker")?;
        ensure!(
            worker.registration.incarnation == request.incarnation,
            "stale worker incarnation"
        );
        ensure!(
            worker
                .registration
                .cpu_observation_protocol
                .is_some_and(
                    |v| (1..=pvisor_core::cpu::CPU_OBSERVATION_PROTOCOL_VERSION).contains(&v)
                ),
            "Worker did not advertise CPU observation support"
        );
        let mut seen = BTreeSet::new();
        let mut intervals = BTreeMap::new();
        for report in &request.samples {
            ensure!(
                report.key.worker_id == request.worker_id
                    && report.key.incarnation == request.incarnation
                    && seen.insert(&report.key.task_id),
                "invalid or duplicate CPU report lease"
            );
            report.sample.validate()?;
            ensure!(report.sequence > 0, "invalid CPU sample sequence");
            if !self.valid_key(&report.key, now) || self.tasks[&report.key.task_id].result.is_some()
            {
                continue;
            }
            let task = &self.tasks[&report.key.task_id];
            ensure!(
                task.spec.execution
                    == ExecutionClass {
                        executor: pvisor_core::ExecutorKind::VirtualMachine,
                        isolation: pvisor_core::IsolationKind::VirtualMachine
                    }
                    && task.spec.run.run_id == report.sample.run_id,
                "CPU report does not match assigned VM Run"
            );
            if let Some(binding) = self.memory_bindings.get(&report.key.task_id) {
                ensure!(
                    binding.key == report.key && binding.attempt == report.sample.attempt_id,
                    "CPU and memory report Attempt identities differ"
                );
                if let (Some((pid, start)), Some(cpu)) = (binding.process, &report.sample.usage) {
                    ensure!(
                        pid == cpu.pid && start == cpu.start_time_ticks,
                        "CPU and memory report process identities differ"
                    );
                }
            }
            let previous = task.cpu_sample.as_ref();
            if let Some(previous) = previous {
                ensure!(
                    report.sequence != previous.report.sequence || report == &previous.report,
                    "conflicting CPU sample sequence"
                );
            }
            if let Some(binding) = self.cpu_bindings.get(&report.key.task_id) {
                ensure!(
                    binding.key == report.key && binding.attempt == report.sample.attempt_id,
                    "CPU report Attempt changed within live lease"
                );
                if let (Some(old), Some(new)) = (&binding.usage, &report.sample.usage) {
                    ensure!(
                        old.pid == new.pid
                            && old.start_time_ticks == new.start_time_ticks
                            && old.clock_ticks_per_second == new.clock_ticks_per_second,
                        "CPU report process/clock changed within Attempt"
                    );
                    if previous.is_none_or(|p| report.sequence > p.report.sequence) {
                        intervals.insert(report.key.task_id.clone(), new.interval_since(old)?);
                    }
                }
            }
        }
        let mut receipt = CpuReportReceipt {
            accepted: vec![],
            ignored: vec![],
        };
        for report in request.samples {
            if !self.valid_key(&report.key, now) || self.tasks[&report.key.task_id].result.is_some()
            {
                receipt.ignored.push(report.key);
                continue;
            }
            if let Some(previous) = &self.tasks[&report.key.task_id].cpu_sample {
                if report.sequence < previous.report.sequence {
                    receipt.ignored.push(report.key);
                    continue;
                }
                if report.sequence == previous.report.sequence {
                    receipt.accepted.push(report.key);
                    continue;
                }
            }
            receipt.accepted.push(report.key.clone());
            let binding = self
                .cpu_bindings
                .entry(report.key.task_id.clone())
                .or_insert_with(|| CpuBinding {
                    key: report.key.clone(),
                    attempt: report.sample.attempt_id.clone(),
                    usage: None,
                });
            if let Some(usage) = &report.sample.usage {
                binding.usage = Some(usage.clone());
            }
            let interval = intervals.remove(&report.key.task_id);
            let task_id = report.key.task_id.clone();
            self.tasks.get_mut(&task_id).unwrap().cpu_sample = Some(ReceivedCpuSample {
                report,
                received_at_ms: now,
                interval,
            });
        }
        Ok(receipt)
    }
    /// Node telemetry is historical data, independent of heartbeats, leases,
    /// reservations and the scheduling journal. No reap or admission occurs.
    pub fn report_node_memory(
        &mut self,
        request: NodeMemoryReportRequest,
        now: u64,
    ) -> anyhow::Result<NodeMemoryReportReceipt> {
        request.sample.validate()?;
        ensure!(request.sequence > 0, "invalid node memory sequence");
        let worker = self
            .workers
            .get(&request.worker_id)
            .context("unknown worker")?;
        ensure!(
            worker.registration.incarnation == request.incarnation,
            "stale worker incarnation"
        );
        let process = match &request.sample.supervisor {
            pvisor_core::memory::MemoryObservation::Measured { usage } => {
                Some((usage.pid, usage.start_time_ticks))
            }
            _ => None,
        };
        let boot = match &request.sample.system {
            pvisor_core::memory::MemoryObservation::Measured { usage } => Some(&usage.host_boot_id),
            _ => None,
        };
        if let Some(binding) = self.node_memory_bindings.get(&request.worker_id) {
            if let (Some(old), Some(new)) = (binding.process, process) {
                ensure!(
                    old == new,
                    "supervisor process changed within worker incarnation"
                );
            }
            if let (Some(old), Some(new)) = (&binding.host_boot, boot) {
                ensure!(old == new, "host boot changed within worker incarnation");
            }
        }
        let mut receipt = NodeMemoryReportReceipt {
            worker_id: request.worker_id.clone(),
            incarnation: request.incarnation.clone(),
            sequence: request.sequence,
            accepted: true,
        };
        if let Some(previous) = &worker.memory_sample {
            ensure!(
                request.sequence != previous.report.sequence || request == previous.report,
                "conflicting node memory sequence"
            );
            if request.sequence <= previous.report.sequence {
                receipt.accepted = request.sequence == previous.report.sequence;
                return Ok(receipt);
            }
        }
        let binding = self
            .node_memory_bindings
            .entry(request.worker_id.clone())
            .or_default();
        if process.is_some() {
            binding.process = process;
        }
        if let Some(boot) = boot {
            binding.host_boot = Some(boot.clone());
        }
        let worker = self.workers.get_mut(&request.worker_id).unwrap();
        worker.memory_sample = Some(ReceivedNodeMemorySample {
            report: request,
            received_at_ms: now,
        });
        Ok(receipt)
    }

    pub fn workers(&self) -> Vec<WorkerRecord> {
        self.workers.values().cloned().collect()
    }
    pub fn counts(&self) -> BTreeMap<String, usize> {
        self.indexes.counts()
    }
}
