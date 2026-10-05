//! Derived indexes: neither terminal history nor cancelled queue entries belong
//! on the scheduling/read hot path. WAL replay rebuilds these from task state.
use crate::{TaskPhase, TaskRecord};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

impl super::Scheduler {
    /// Read-only records in task-ID order. Full enumeration is linear in
    /// retained history; counts() is the aggregate monitoring hot path.
    pub fn task_records(&self) -> impl ExactSizeIterator<Item = &TaskRecord> {
        self.tasks.values().map(Box::as_ref)
    }
}

#[derive(Default)]
pub(super) struct ReadyQueue {
    order: BTreeMap<u64, Arc<str>>,
    positions: HashMap<Arc<str>, u64>,
    next: u64,
}

impl ReadyQueue {
    pub(super) fn len(&self) -> usize {
        self.order.len()
    }

    pub(super) fn push_back(&mut self, id: impl Into<Arc<str>>) {
        let id = id.into();
        self.remove(&id);
        if self.order.is_empty() {
            self.next = 0;
        }
        let sequence = self.next;
        self.next = self
            .next
            .checked_add(1)
            .expect("ready queue sequence exhausted");
        self.positions.insert(id.clone(), sequence);
        self.order.insert(sequence, id);
    }

    pub(super) fn pop_front(&mut self) -> Option<Arc<str>> {
        let (_, id) = self.order.pop_first()?;
        self.positions
            .remove(id.as_ref())
            .expect("ready queue position");
        Some(id)
    }

    pub(super) fn remove(&mut self, id: &str) {
        if let Some(sequence) = self.positions.remove(id) {
            self.order.remove(&sequence).expect("ready queue order");
        }
    }
}

impl FromIterator<String> for ReadyQueue {
    fn from_iter<T: IntoIterator<Item = String>>(iter: T) -> Self {
        let mut queue = Self::default();
        for id in iter {
            queue.push_back(id);
        }
        queue
    }
}

#[derive(Default)]
pub(super) struct TaskIndexes {
    pub(super) ready: ReadyQueue,
    counts: BTreeMap<&'static str, usize>,
}

// Preserve the existing /v1/counts key spelling, including concatenated words.
fn name(phase: TaskPhase) -> &'static str {
    match phase {
        TaskPhase::WaitingDependencies => "waitingdependencies",
        TaskPhase::WaitingCheckpoint => "waitingcheckpoint",
        TaskPhase::Queued => "queued",
        TaskPhase::Leased => "leased",
        TaskPhase::Running => "running",
        TaskPhase::Paused => "paused",
        TaskPhase::Offloaded => "offloaded",
        TaskPhase::Suspending => "suspending",
        TaskPhase::RetainingArtifacts => "retainingartifacts",
        TaskPhase::Cancelling => "cancelling",
        TaskPhase::Succeeded => "succeeded",
        TaskPhase::Failed => "failed",
        TaskPhase::Cancelled => "cancelled",
        TaskPhase::Lost => "lost",
        TaskPhase::Suspended => "suspended",
    }
}

impl TaskIndexes {
    fn subtract(&mut self, phase: TaskPhase) {
        let count = self.counts.get_mut(name(phase)).expect("task phase count");
        *count = count.checked_sub(1).expect("task phase count underflow");
        if *count == 0 {
            self.counts.remove(name(phase));
        }
    }

    pub(super) fn insert(&mut self, task: &TaskRecord, previous: Option<TaskPhase>) {
        if let Some(previous) = previous {
            self.subtract(previous);
            self.ready.remove(&task.spec.id);
        }
        *self.counts.entry(name(task.phase)).or_default() += 1;
        if task.phase == TaskPhase::Queued {
            self.ready.push_back(task.spec.id.as_str());
        }
    }

    pub(super) fn set_phase(&mut self, task: &mut TaskRecord, phase: TaskPhase) {
        if task.phase == phase {
            return;
        }
        self.subtract(task.phase);
        *self.counts.entry(name(phase)).or_default() += 1;
        if task.phase == TaskPhase::Queued {
            self.ready.remove(&task.spec.id);
        }
        if phase == TaskPhase::Queued {
            self.ready.push_back(task.spec.id.as_str());
        }
        task.phase = phase;
    }

    pub(super) fn counts(&self) -> BTreeMap<String, usize> {
        self.counts
            .iter()
            .map(|(name, count)| ((*name).into(), *count))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CLUSTER_VERSION, ExecutionClass, Resources, TaskSpec,
        scheduler::{Scheduler, SchedulerConfig},
    };
    use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation, RunSpec};
    use std::collections::VecDeque;

    #[test]
    fn indexed_fifo_matches_ordered_reference_under_removal_rotation_and_reinsertion() {
        let mut queue = ReadyQueue::default();
        let mut reference = VecDeque::<String>::new();
        let mut seed = 17_u64;
        for _ in 0..20_000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let id = format!("task-{}", seed % 127);
            match (seed >> 32) % 3 {
                0 => {
                    reference.retain(|old| old != &id);
                    reference.push_back(id.clone());
                    queue.push_back(id);
                }
                1 => {
                    reference.retain(|old| old != &id);
                    queue.remove(&id);
                }
                _ => assert_eq!(
                    queue.pop_front().map(|id| id.to_string()),
                    reference.pop_front()
                ),
            }
            assert_eq!(queue.len(), reference.len());
            assert_eq!(queue.positions.len(), reference.len());
        }
        while let Some(expected) = reference.pop_front() {
            assert_eq!(queue.pop_front().unwrap().as_ref(), expected);
        }
        assert_eq!(queue.len(), 0);
        // Removed tasks leave no IDs or ordered entries behind.
        for id in 0..10_000 {
            let id = format!("history-{id}");
            queue.push_back(id.as_str());
            queue.remove(&id);
        }
        assert!(queue.order.is_empty() && queue.positions.is_empty());
    }

    #[test]
    fn rotated_ready_ids_share_one_allocation_and_keep_the_same_identity() {
        let id: Arc<str> = "ready-id".into();
        let mut queue = ReadyQueue::default();
        queue.push_back(id.clone());
        assert_eq!(Arc::strong_count(&id), 3);
        let popped = queue.pop_front().unwrap();
        assert!(Arc::ptr_eq(&id, &popped));
        assert_eq!(Arc::strong_count(&id), 2);
        queue.push_back(popped);
        assert_eq!(Arc::strong_count(&id), 3);
        queue.remove(&id);
        assert_eq!(Arc::strong_count(&id), 1);
    }

    #[test]
    fn all_phase_pairs_preserve_counts_keys_zero_omission_and_ready_membership() {
        let temp = tempfile::tempdir().unwrap();
        let mut scheduler =
            Scheduler::open(&temp.path().join("wal"), SchedulerConfig::default()).unwrap();
        let mut run = RunSpec::process("state-index", "test", "/bin/true");
        let RunInvocation::Process(process) = &mut run.invocation;
        process.inherit_env = false;
        let record = scheduler
            .submit(
                TaskSpec {
                    version: CLUSTER_VERSION,
                    id: "state-index".into(),
                    tenant: "test".into(),
                    run,
                    execution: ExecutionClass {
                        executor: ExecutorKind::Process,
                        isolation: IsolationKind::HostProcess,
                    },
                    resources: Resources {
                        slots: 1,
                        memory_bytes: 64 * 1024 * 1024,
                        cpu_millis: 250,
                    },
                    labels: Default::default(),
                    cache_keys: vec![],
                    retain_bundle: false,
                    retain_artifacts: None,
                    gateway: None,
                    cpu_qos: None,
                    restore: None,
                    environment: None,
                },
                0,
            )
            .unwrap();
        let phases = [
            TaskPhase::WaitingDependencies,
            TaskPhase::WaitingCheckpoint,
            TaskPhase::Queued,
            TaskPhase::Leased,
            TaskPhase::Running,
            TaskPhase::Paused,
            TaskPhase::Offloaded,
            TaskPhase::Suspending,
            TaskPhase::RetainingArtifacts,
            TaskPhase::Cancelling,
            TaskPhase::Succeeded,
            TaskPhase::Failed,
            TaskPhase::Cancelled,
            TaskPhase::Lost,
            TaskPhase::Suspended,
        ];
        for before in phases {
            for after in phases {
                let mut task = record.clone();
                task.phase = before;
                let mut indexes = TaskIndexes::default();
                indexes.insert(&task, None);
                indexes.set_phase(&mut task, after);
                indexes.set_phase(&mut task, after);
                // Independent reference preserves the previous public spelling.
                assert_eq!(
                    indexes.counts(),
                    BTreeMap::from([(format!("{after:?}").to_lowercase(), 1)])
                );
                assert_eq!(indexes.ready.len(), usize::from(after == TaskPhase::Queued));
                if after == TaskPhase::Queued {
                    assert_eq!(indexes.ready.pop_front().unwrap().as_ref(), task.spec.id);
                }
                indexes.insert(&task, Some(after));
                assert_eq!(indexes.counts().values().sum::<usize>(), 1);
            }
        }
    }
}
