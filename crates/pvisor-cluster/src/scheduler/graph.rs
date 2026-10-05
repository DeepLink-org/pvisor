//! Dependency indexes are rebuilt by deterministic WAL replay. Only terminal
//! events visit their outgoing edges; polling never scans blocked graph nodes.
use super::*;

#[derive(Default)]
pub(super) struct GraphState {
    graphs: BTreeMap<String, StoredGraph>,
    owners: BTreeMap<String, String>,
    dependents: BTreeMap<String, Vec<String>>,
    remaining: BTreeMap<String, usize>,
}

struct StoredGraph {
    spec: TaskGraphSpec,
    created_at_ms: u64,
    cancel_requested_at_ms: Option<u64>,
}

impl Scheduler {
    /// Validate the entire graph before one fsync-before-ack transaction creates
    /// any identities. Existing tasks cannot be silently adopted into a graph.
    pub fn submit_graph(
        &mut self,
        spec: TaskGraphSpec,
        now: u64,
    ) -> anyhow::Result<TaskGraphRecord> {
        ensure!(spec.version == CLUSTER_VERSION, "unsupported graph version");
        ensure!(
            identifier(&spec.id) && identifier(&spec.tenant),
            "invalid graph or tenant id"
        );
        ensure!(
            !spec.nodes.is_empty() && spec.nodes.len() <= 256,
            "graph needs 1..256 nodes"
        );
        ensure!(
            serde_json::to_vec(&spec)?.len() <= 2 * 1024 * 1024,
            "graph specification exceeds 2 MiB"
        );
        if let Some(existing) = self.graph_state.graphs.get(&spec.id) {
            ensure!(
                serde_json::to_value(&existing.spec)? == serde_json::to_value(&spec)?,
                "idempotency conflict: graph id already has a different specification"
            );
            return self.graph(&spec.id);
        }
        ensure!(
            self.tasks
                .len()
                .checked_add(spec.nodes.len())
                .is_some_and(|n| n <= self.config.max_tasks),
            "task retention limit reached"
        );
        let mut ids = BTreeSet::new();
        let mut runs = BTreeSet::new();
        for node in &spec.nodes {
            ensure!(ids.insert(node.task.id.clone()), "duplicate graph task id");
            ensure!(
                !self.tasks.contains_key(&node.task.id),
                "graph task id already exists"
            );
            ensure!(
                node.task.tenant == spec.tenant,
                "graph nodes must share its tenant"
            );
            ensure!(
                runs.insert(node.task.run.run_id.to_string())
                    && !self.run_ids.contains(node.task.run.run_id.as_str()),
                "graph Run id already exists"
            );
        }
        let mut outgoing: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut indegrees = BTreeMap::new();
        let mut edges = 0usize;
        for node in &spec.nodes {
            let mut unique = BTreeSet::new();
            for parent in &node.depends_on {
                ensure!(
                    parent != &node.task.id && ids.contains(parent),
                    "dependency must name another task in this graph"
                );
                ensure!(unique.insert(parent), "duplicate dependency");
                outgoing.entry(parent).or_default().push(&node.task.id);
            }
            edges += unique.len();
            ensure!(edges <= 4096, "graph exceeds 4096 edges");
            indegrees.insert(node.task.id.as_str(), unique.len());
        }
        let mut ready: VecDeque<_> = indegrees
            .iter()
            .filter_map(|(&id, &n)| (n == 0).then_some(id))
            .collect();
        let mut visited = 0;
        while let Some(id) = ready.pop_front() {
            visited += 1;
            for child in outgoing.get(id).into_iter().flatten() {
                let n = indegrees.get_mut(child).expect("graph node");
                *n -= 1;
                if *n == 0 {
                    ready.push_back(child);
                }
            }
        }
        ensure!(
            visited == spec.nodes.len(),
            "graph contains a dependency cycle"
        );
        let tasks = spec
            .nodes
            .iter()
            .map(|node| {
                let mut task = self.prepare_submission(node.task.clone(), now)?;
                if !node.depends_on.is_empty() {
                    task.phase = TaskPhase::WaitingDependencies;
                }
                Ok(task)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let id = spec.id.clone();
        self.commit(vec![Change::GraphSubmitted {
            spec,
            tasks,
            at: now,
        }])?;
        self.graph(&id)
    }

    pub fn graph(&self, id: &str) -> anyhow::Result<TaskGraphRecord> {
        let stored = self
            .graph_state
            .graphs
            .get(id)
            .context("unknown task graph")?;
        let tasks: Vec<_> = stored
            .spec
            .nodes
            .iter()
            .map(|n| &self.tasks[&n.task.id])
            .collect();
        let terminal = tasks.iter().all(|t| t.phase.terminal());
        let phase = if terminal {
            if tasks.iter().all(|t| t.phase == TaskPhase::Succeeded) {
                TaskGraphPhase::Succeeded
            } else if tasks.iter().any(|t| {
                matches!(
                    t.phase,
                    TaskPhase::Failed | TaskPhase::Lost | TaskPhase::Suspended
                )
            }) {
                TaskGraphPhase::Failed
            } else {
                TaskGraphPhase::Cancelled
            }
        } else if stored.cancel_requested_at_ms.is_some() {
            TaskGraphPhase::Cancelling
        } else if tasks.iter().any(|t| t.generation > 0 || t.phase.terminal()) {
            TaskGraphPhase::Running
        } else {
            TaskGraphPhase::Queued
        };
        Ok(TaskGraphRecord {
            spec: stored.spec.clone(),
            phase,
            nodes: tasks
                .iter()
                .map(|t| TaskGraphNodeState {
                    task_id: t.spec.id.clone(),
                    phase: t.phase,
                })
                .collect(),
            created_at_ms: stored.created_at_ms,
            updated_at_ms: tasks
                .iter()
                .map(|t| t.updated_at_ms)
                .chain(stored.cancel_requested_at_ms)
                .max()
                .unwrap_or(stored.created_at_ms),
            cancel_requested_at_ms: stored.cancel_requested_at_ms,
        })
    }

    /// One transaction records intent and cancels every unfinished node. Live
    /// leases remain charged until their ordinary completion/expiry path settles.
    pub fn cancel_graph(&mut self, id: &str, now: u64) -> anyhow::Result<TaskGraphRecord> {
        self.reap(now)?;
        let stored = self
            .graph_state
            .graphs
            .get(id)
            .context("unknown task graph")?;
        if stored.cancel_requested_at_ms.is_none() {
            let unfinished = stored
                .spec
                .nodes
                .iter()
                .any(|n| !self.tasks[&n.task.id].phase.terminal());
            let mut changes = vec![Change::GraphCancelled {
                graph_id: id.into(),
                at: now,
            }];
            changes.extend(stored.spec.nodes.iter().filter_map(|node| {
                let task = &self.tasks[&node.task.id];
                (!task.phase.terminal() && task.phase != TaskPhase::Cancelling).then(|| {
                    Change::Cancel {
                        task_id: node.task.id.clone(),
                        at: now,
                    }
                })
            }));
            // A completed graph remains unchanged when cancellation is retried.
            if unfinished {
                self.commit(changes)?;
            }
        }
        self.graph(id)
    }

    pub(super) fn apply_graph(&mut self, spec: TaskGraphSpec, tasks: Vec<TaskRecord>, at: u64) {
        for node in &spec.nodes {
            self.graph_state
                .owners
                .insert(node.task.id.clone(), spec.id.clone());
            if !node.depends_on.is_empty() {
                self.graph_state
                    .remaining
                    .insert(node.task.id.clone(), node.depends_on.len());
                for parent in &node.depends_on {
                    self.graph_state
                        .dependents
                        .entry(parent.clone())
                        .or_default()
                        .push(node.task.id.clone());
                }
            }
        }
        self.graph_state.graphs.insert(
            spec.id.clone(),
            StoredGraph {
                spec,
                created_at_ms: at,
                cancel_requested_at_ms: None,
            },
        );
        for task in tasks {
            self.apply(Change::Submit {
                task: Box::new(task),
            });
        }
    }

    pub(super) fn apply_graph_cancel(&mut self, id: &str, at: u64) {
        self.graph_state
            .graphs
            .get_mut(id)
            .expect("graph")
            .cancel_requested_at_ms = Some(at);
    }

    pub(super) fn settle_dependencies(&mut self, id: &str, at: u64) {
        if !self.tasks[id].phase.terminal() {
            return;
        }
        self.graph_state.remaining.remove(id);
        if !self.graph_state.dependents.contains_key(id) {
            return;
        }
        let mut pending = VecDeque::from([id.to_owned()]);
        while let Some(parent) = pending.pop_front() {
            self.graph_state.remaining.remove(&parent);
            let parent_phase = self.tasks[&parent].phase;
            for child in self
                .graph_state
                .dependents
                .remove(&parent)
                .unwrap_or_default()
            {
                if self.tasks[&child].phase != TaskPhase::WaitingDependencies {
                    continue;
                }
                if parent_phase == TaskPhase::Succeeded {
                    let count = self
                        .graph_state
                        .remaining
                        .get_mut(&child)
                        .expect("dependency count");
                    *count -= 1;
                    if *count == 0 {
                        self.graph_state.remaining.remove(&child);
                        let task = self.tasks.get_mut(&child).expect("child");
                        self.indexes.set_phase(task, TaskPhase::Queued);
                        task.updated_at_ms = at;
                    }
                } else {
                    let graph = &self.graph_state.graphs[&self.graph_state.owners[&child]];
                    let task = self.tasks.get_mut(&child).expect("child");
                    let phase = if graph.cancel_requested_at_ms.is_some() {
                        TaskPhase::Cancelled
                    } else {
                        TaskPhase::Failed
                    };
                    self.indexes.set_phase(task, phase);
                    task.error = Some(format!(
                        "dependency {parent} ended as {parent_phase:?}; task was never executed"
                    ));
                    task.updated_at_ms = at;
                    self.graph_state.remaining.remove(&child);
                    pending.push_back(child);
                }
            }
        }
    }
}
