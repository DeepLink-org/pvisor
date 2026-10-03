//! Single-writer durable shard. Queue lookahead and expiry indexes bound work
//! by ready/expired tasks, rather than by historical task count.
use crate::journal::Journal;
use crate::*;
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub lease_duration_ms: u64,
    pub queue_lookahead: usize,
    pub max_batch: u32,
    pub max_tasks: usize,
    /// Limits concurrent reservations per tenant. Unlisted tenants have no quota.
    pub tenant_quotas: BTreeMap<String, Resources>,
}
impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            lease_duration_ms: 30_000,
            queue_lookahead: 256,
            max_batch: 64,
            max_tasks: 1_000_000,
            tenant_quotas: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Change {
    Submit {
        task: Box<TaskRecord>,
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
        lease: Lease,
        at: u64,
    },
    Renew {
        worker_id: String,
        at: u64,
        expires: u64,
        keys: Vec<LeaseKey>,
        acknowledged: BTreeSet<String>,
    },
    Cancel {
        task_id: String,
        at: u64,
    },
    Finish {
        task_id: String,
        phase: TaskPhase,
        result: Option<Box<pvisor_core::RunResult>>,
        error: Option<String>,
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
}

#[derive(Debug, Serialize, Deserialize)]
struct Transaction {
    version: u32,
    changes: Vec<Change>,
}

pub struct Scheduler {
    config: SchedulerConfig,
    journal: Journal,
    tasks: BTreeMap<String, TaskRecord>,
    workers: BTreeMap<String, WorkerRecord>,
    queue: VecDeque<String>,
    active: BTreeMap<String, BTreeSet<String>>,
    expiry: BTreeSet<(u64, String)>,
    tenant_reserved: BTreeMap<String, Resources>,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
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
        let (journal, transactions) = Journal::open::<Transaction>(path)?;
        let mut scheduler = Self {
            config,
            journal,
            tasks: BTreeMap::new(),
            workers: BTreeMap::new(),
            queue: VecDeque::new(),
            active: BTreeMap::new(),
            expiry: BTreeSet::new(),
            tenant_reserved: BTreeMap::new(),
        };
        for transaction in transactions {
            ensure!(
                transaction.version == CLUSTER_VERSION,
                "unsupported journal version"
            );
            for change in transaction.changes {
                scheduler.apply(change);
            }
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
            Change::Submit { task } => {
                self.queue.push_back(task.spec.id.clone());
                self.tasks.insert(task.spec.id.clone(), *task);
            }
            Change::Register { registration, at } => {
                let id = registration.id.clone();
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
                    },
                );
                self.active.entry(id).or_default();
            }
            Change::Drain {
                worker_id,
                draining,
            } => self.workers.get_mut(&worker_id).expect("worker").draining = draining,
            Change::Assign { lease, at } => {
                let task = self.tasks.get_mut(&lease.key.task_id).expect("task");
                task.phase = TaskPhase::Leased;
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
            } => {
                self.workers.get_mut(&worker_id).expect("worker").seen_at_ms = at;
                for key in keys {
                    let task = self.tasks.get_mut(&key.task_id).expect("task");
                    let lease = task.lease.as_mut().expect("lease");
                    self.expiry
                        .remove(&(lease.expires_at_ms, key.task_id.clone()));
                    lease.expires_at_ms = expires;
                    self.expiry.insert((expires, key.task_id.clone()));
                    if task.phase == TaskPhase::Leased && acknowledged.contains(&key.task_id) {
                        task.phase = TaskPhase::Running;
                    }
                    task.updated_at_ms = at;
                }
            }
            Change::Cancel { task_id, at } => {
                let task = self.tasks.get_mut(&task_id).expect("task");
                task.phase = if task.phase == TaskPhase::Queued {
                    TaskPhase::Cancelled
                } else {
                    TaskPhase::Cancelling
                };
                task.updated_at_ms = at;
                for control in &mut task.controls {
                    if !control.phase.terminal() {
                        control.phase = ControlPhase::Aborted;
                        control.completed_at_ms = Some(at);
                    }
                }
            }
            Change::Finish {
                task_id,
                phase,
                result,
                error,
                at,
            } => {
                self.release(&task_id);
                let task = self.tasks.get_mut(&task_id).expect("task");
                task.phase = phase;
                task.result = result.map(|r| *r);
                task.error = error;
                task.updated_at_ms = at;
                task.reserved = None;
                for control in &mut task.controls {
                    if !control.phase.terminal() {
                        control.phase = ControlPhase::Aborted;
                        control.completed_at_ms = Some(at);
                    }
                }
                // Preserve the lease key as evidence for idempotent completion.
            }
            Change::ControlRequested { task_id, record } => {
                self.tasks
                    .get_mut(&task_id)
                    .expect("task")
                    .controls
                    .push(record);
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
                self.reallocate(id, reserved);
                let task = self.tasks.get_mut(id).expect("task");
                let record = task
                    .controls
                    .iter_mut()
                    .find(|c| c.command == acknowledgement.command)
                    .expect("control");
                record.phase = match acknowledgement.outcome {
                    ControlOutcome::Succeeded { state, .. } => {
                        task.phase = match state {
                            pvisor_core::VmState::Running => TaskPhase::Running,
                            pvisor_core::VmState::Paused => TaskPhase::Paused,
                            pvisor_core::VmState::Offloaded => TaskPhase::Offloaded,
                        };
                        ControlPhase::Succeeded
                    }
                    ControlOutcome::Failed { .. } => ControlPhase::Failed,
                };
                record.outcome = Some(acknowledgement.outcome);
                record.completed_at_ms = Some(at);
                task.updated_at_ms = at;
            }
        }
    }

    pub fn submit(&mut self, spec: TaskSpec, now: u64) -> anyhow::Result<TaskRecord> {
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
        let pvisor_core::RunInvocation::Process(process) = &spec.run.invocation;
        ensure!(!process.program.trim().is_empty(), "empty program");
        ensure!(
            !process.inherit_env,
            "cluster tasks must explicitly project environment variables"
        );
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
            self.tasks.len() < self.config.max_tasks,
            "task retention limit reached"
        );
        let task = TaskRecord {
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
        };
        self.commit(vec![Change::Submit {
            task: Box::new(task.clone()),
        }])?;
        Ok(task)
    }

    pub fn register(
        &mut self,
        registration: WorkerRegistration,
        now: u64,
    ) -> anyhow::Result<WorkerRecord> {
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

    fn valid_key(&self, key: &LeaseKey, now: u64) -> bool {
        self.tasks.get(&key.task_id).is_some_and(|t| {
            !t.phase.terminal()
                && t.lease
                    .as_ref()
                    .is_some_and(|l| l.key == *key && l.expires_at_ms > now)
        })
    }

    pub fn poll(&mut self, request: PollRequest, now: u64) -> anyhow::Result<PollResponse> {
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
            request.active.len() <= worker.registration.capacity.slots as usize,
            "too many active leases"
        );
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
        }])?;

        // Lost assignment responses are redelivered with the same fencing key.
        // No extra reservation and no second execution for a known key.
        let mut assignments = Vec::new();
        for id in &self.active[&request.worker_id] {
            let task = &self.tasks[id];
            if task.phase == TaskPhase::Leased && !seen.contains(id) {
                assignments.push(Assignment {
                    spec: task.spec.clone(),
                    lease: task.lease.clone().unwrap(),
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
        for _ in 0..self.queue.len().min(self.config.queue_lookahead) {
            let id = self.queue.pop_front().unwrap();
            if self.tasks[&id].phase == TaskPhase::Queued {
                window.push(id);
            }
        }
        window.sort_by_key(|id| {
            std::cmp::Reverse(
                self.tasks[id]
                    .spec
                    .cache_keys
                    .iter()
                    .filter(|k| cached.contains(k))
                    .count(),
            )
        });
        let mut changes = Vec::new();
        let mut tenant_delta: BTreeMap<String, Resources> = BTreeMap::new();
        for id in &window {
            let task = &self.tasks[id];
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
                || !quota_fits
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
                    task_id: id.clone(),
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
                spec: spec.clone(),
                lease: lease.clone(),
            });
            changes.push(Change::Assign { lease, at: now });
        }
        // Restore queue on storage failure. Queue order is advisory; leases are durable.
        self.queue.extend(window);
        self.commit(changes)?;
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
                && task.phase != TaskPhase::Cancelling
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
        self.commit(vec![Change::ControlRequested {
            task_id: id.into(),
            record: record.clone(),
        }])?;
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
        self.reap(now)?;
        let task = self
            .tasks
            .get(&completion.key.task_id)
            .context("unknown task")?;
        if task.phase.terminal() {
            ensure!(
                task.phase != TaskPhase::Lost
                    && task.lease.as_ref().is_some_and(|l| l.key == completion.key)
                    && serde_json::to_value(&task.result)?
                        == serde_json::to_value(&completion.result)?
                    && task.error == completion.error,
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
        if let Some(result) = &completion.result {
            ensure!(
                result.run_id == task.spec.run.run_id,
                "result belongs to a different Run"
            );
            ensure!(
                matches!(
                    result.state,
                    pvisor_core::RunState::Completed
                        | pvisor_core::RunState::Failed
                        | pvisor_core::RunState::Cancelled
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
        let phase =
            if task.phase == TaskPhase::Cancelling {
                TaskPhase::Cancelled
            } else if completion.result.as_ref().is_some_and(|r| {
                r.state == pvisor_core::RunState::Completed && r.exit_code == Some(0)
            }) {
                TaskPhase::Succeeded
            } else {
                TaskPhase::Failed
            };
        let id = completion.key.task_id;
        self.commit(vec![Change::Finish {
            task_id: id.clone(),
            phase,
            result: completion.result.map(Box::new),
            error: completion.error,
            at: now,
        }])?;
        Ok(self.tasks[&id].clone())
    }

    pub fn reap(&mut self, now: u64) -> anyhow::Result<usize> {
        let changes: Vec<_> = self
            .expiry
            .range(..=(now, String::from("\u{10ffff}")))
            .map(|(_, id)| Change::Finish {
                task_id: id.clone(),
                phase: TaskPhase::Lost,
                result: None,
                error: Some("lease expired; execution outcome unknown, no automatic retry".into()),
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
    pub fn workers(&self) -> Vec<WorkerRecord> {
        self.workers.values().cloned().collect()
    }
    pub fn counts(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for task in self.tasks.values() {
            *counts
                .entry(format!("{:?}", task.phase).to_lowercase())
                .or_default() += 1;
        }
        counts
    }
}
