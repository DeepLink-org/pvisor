# Cluster execution and centralized evidence

pVisor implements a cluster execution path with one Controller shard and independent Workers, including capability matching, leases, DAGs, native controls, checkpoints/forks and centralized evidence. [Cluster architecture](../cluster/index.md) defines current mechanisms and failure contracts; research workloads and extension boundaries remain here.

## A task's path through a Worker {#worker-flow}

A caller submits a native RunSpec and pinned input requirements. The Controller accepts the task and assigns an exact lease during Worker polling. After final admission, the Worker prepares an independent environment, executes, retains evidence and uploads/delivers it through a durable outbox. See the [complete task path](../cluster/index.md#task-flow).

Worker reports reconstruct the Controller runtime view, with fresh ownership confirmation required after restart. Unknown execution is never automatically rerun elsewhere. Initial task creation, low-frequency control intents and terminal receipts remain persistent. See [state and recovery](../cluster/state-and-recovery.md) for loss and replacement boundaries.

## Implementation and integration boundaries {#boundary}

| Layer | Current mechanisms | Integration/validation remaining |
| --- | --- | --- |
| pVisor Worker | Native execution, environment layers, Gateway, observations, terminal outbox | Malicious-node trust boundary, local evidence GC, long-running operations |
| Cluster Controller | Queue, matching, reservations, DAGs, reconciliation, fencing | Multiple shards/HA, tenant identities, online history compaction |
| Evidence/checkpoint repositories | Local CAS, verified manifests, reference protection, optional FS/S3 snapshot publication/import | Replication, tenant storage quotas, cross-host runtime compatibility/recovery |
| Kubernetes, Ray or training frameworks | Can call task/lifecycle APIs | GPU/rollout/scaffold coordination and end-to-end recovery |
| Review/release service | Can read retained evidence | Baseline verification, change selection, business reconciliation, merge and deployment |

Local Run Bundle paths are Worker references; JSON upload does not transfer every file. Downloadable checkpoints do not establish arbitrary-node restore. Environments, snapshots and input versions need explicit compatibility contracts.

## Next experiments {#validation}

Preserve the scope of existing protocol, process and hardware gates, then deploy fixed workloads on independent hosts. Measure useful execution, startup/restore tails, memory density and storage costs. Extend the [concurrent-density methodology](../../benchmarks/density.md); do not extrapolate from single-host empty tasks.

Inject permanent node loss, abrupt Controller termination, long partitions, full disks, interrupted uploads, credential revocation and parallel model waits. Each outcome must identify inputs, Task/Run/Attempt, native observations and artifacts. Check unknown side effects and explicit loss resolution; control-plane fencing is not external-effect rollback.

## Research questions and release conditions {#research}

Further reducing Controller state requires defining reconstruction authority for upstream desired state, bounded Worker terminal inventories and retained manifests. Solve unassigned intents and acknowledged history before treating the Controller as entirely rebuildable. Multiple shards and disaster recovery require a new ownership protocol, not local file locks.

Production release also needs node/tenant identities, long-running representative failure/performance experiments, compatibility matrices and operational recovery. See [Cluster operations and validation](../cluster/operations.md) for mechanisms and evolution constraints, [Parallel agents](../../guides/parallel-agents.md) for local workflows, and [RL execution substrate](rl-execution-substrate.md) for training integration.
