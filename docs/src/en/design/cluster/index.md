# Cluster architecture

pVisor Cluster extends the same `RunSpec` / `RunResult` execution semantics to multiple Workers. The Controller accepts tasks, chooses nodes and maintains execution identities; Workers prepare environments, perform native execution and deliver evidence. The current deployment unit is one Controller shard with exclusive ownership of its state directory, plus independently running Workers.

The design baseline is the current source on 2026-10-05. Runtime state uses Worker reconciliation and eventual consistency; accepted intents and terminal receipts retain low-frequency persistence. Deployment, protocol and validation boundaries are defined in the following documents.

Use the [Cluster quickstart](../../guides/cluster/index.md) for setup and step-by-step verification, including bounded resources, recovery, VMs and an offline model Gateway.

| Topic | Document and main questions |
| --- | --- |
| State and recovery | [State authority, leases and restart reconciliation](state-and-recovery.md): what must survive? Can lost work be rerun? |
| Scheduling | [Scheduling, admission and DAGs](scheduling.md): matching, reservations, final admission and dependency progress |
| Execution lifecycle | [Native controls, inference waits and forks](lifecycle.md): when is CPU released? When can a reply reach the Agent? |
| Storage | [Metadata, artifacts and reclamation](storage.md): commit barriers, outbox, checkpoint publication and GC |
| Sharing and lazy loading | [Shared working sets and lazy loading](shared-working-set.md): existing reuse, target costs, cache budgets and questions to freeze before experiments |
| Interfaces and operations | [Protocol, deployment and failure handling](operations.md): APIs, configuration, recovery, validation and evolution |

## Layers and responsibilities {#architecture}

```text
Caller / training system / platform service
        │ TaskSpec, DAG, cancel, control, fork; query results
        ▼
Controller HTTP API (Admin / Worker roles)
        │ bounded Dispatcher → single-writer Scheduler
        ├── tasks / DAGs / control intents / assignments / terminal receipts
        ├── Worker-derived runtime view, reservations, indexes
        └── low-frequency transaction log + Controller-local artifact CAS
        ▲
        │ Workers initiate register / poll / recover / ACK / complete
        ▼
Worker (node profile, final admission, lease watchdog, terminal outbox)
        ├── pVisor executors: host / rootless / container / VM
        ├── Attempt Gateway: model authorization, routing, capture, waits
        ├── OverlayFS / OverlayNet: actual file and network boundaries
        └── environment cache, native checkpoints, optional FS/S3 repository
```

| Component | Decisions and facts it owns | Responsibilities outside its scope |
| --- | --- | --- |
| Caller | Workload, input versions, task IDs, tenants, retry and business side-effect policy | Inferring from a timeout that execution had no side effects |
| Controller | Accepted intents, execution identity, control revisions, logical accounting, aggregate results and evidence roots | Deciding directly whether a kernel has stopped, controlling vCPUs or holding model-provider credentials |
| Worker | Final node admission, actual execution, native observations, complete active-key inventory, delivery retries | Changing task definitions or adopting unknown execution from an old incarnation |
| Core | Shared versioned types and validation rules | Global scheduling or a node runtime |
| Executors and drivers | Platform isolation, termination, pause, snapshots and resource controls | Treating Controller reservations as installed limits |
| Artifact and checkpoint repositories | Immutable contents, integrity, references and retention | Proving cross-node recovery from a local path alone |

Cluster does not incorporate Kubernetes, GPU training scheduling or rollout/scaffold state into its native control plane. Those systems can submit tasks and lifecycle requests; coordinating checkpoints with GPU scheduling and training transactions remains integration work.

## A task's complete path {#task-flow}

1. A caller submits an immutable `TaskSpec`. The Controller validates version, identities, resources, execution requirements and references, then returns the record after persistence. Repeating the same ID and contents returns the existing record.
2. A Worker registers its capabilities and polls with its complete active `LeaseKey` inventory, available capacity and node-pressure report.
3. The Controller selects compatible tasks from a bounded ready window, checking Worker/tenant reservations and available capacity. It returns an assignment only after its identity and reservation are persisted.
4. The Worker rechecks local conditions. An unstarted task can be declined with its exact key and returned to the queue. An accepted assignment gets independent local state, an executor and an optional Gateway.
5. Native execution starts under a monotonic lease watchdog. Subsequent polls confirm ownership and reconstruct `Running` and deadlines; renewal-only polls do not write the log.
6. After native execution and local evidence sealing finish, the Worker persists a terminal outbox entry. Optional `native-done` releases execution slots while retaining a bounded upload reservation. Required artifacts are then uploaded and verified.
7. The Controller accepts the exact execution's final result, persists its receipt, releases reservations and advances the DAG. The Worker persists the matching receipt before removing the pending entry; acknowledged local evidence can still occupy disk.

Execution failure, artifact failure and business effects are interpreted separately. If a command succeeds but required artifact delivery fails, its native result is preserved and the aggregate task cannot release successful dependencies.

## Design invariants {#invariants}

- A task ID's definition is immutable; control and fork requests use caller-provided idempotency IDs.
- An execution is identified by `{task_id, worker_id, incarnation, generation}`. Only matching identities may update its record. Transport retries are allowed; unknown execution is never automatically rerun on another node.
- New assignments, control intents and terminal receipts are acknowledged after the persistence barrier. HTTP timeouts or disconnects do not cancel queued operations.
- After Controller restart, a historical lease proves neither liveness nor termination. Pending reconciliation retains resources and artifact roots.
- Worker final admission and native observations determine actual execution. Requests, reservations, installed controls and observations remain distinct.
- GC preserves objects referenced by active leases, retained evidence or download protection; destructive reclamation waits for reconciliation.

These invariants preserve control-plane ownership and recovery ordering. They do not provide exactly-once execution for external APIs, databases or messages; business integration needs idempotency keys, compensation and verification.

## Source boundaries and reading order {#source-map}

Paths below are relative to the repository root. Shared protocol definitions have one authoritative owner.

| Path | Responsibility |
| --- | --- |
| `crates/pvisor-core/src/cluster.rs` | Task/Worker/Lease/Graph/Control, environment, artifact and telemetry protocols |
| `crates/pvisor-cluster/src/scheduler.rs` | State machine, transaction application, leases, accounting, matching and recovery |
| `crates/pvisor-cluster/src/scheduler/{indexes,graph,inference}.rs` | Ready/count indexes, DAG progress, inference waits |
| `crates/pvisor-cluster/src/server.rs`, `server/dispatcher.rs` | Roles, routes, request bounds, single writer and group commit |
| `crates/pvisor-cluster/src/{journal,artifacts,environment,admission,physical_memory}.rs` | Metadata log, CAS, environment validation, admission and memory reporting |
| `crates/pvisor-cluster/src/artifacts/{gc,quota}.rs` | References, download protection, GC, storage quotas and publication |
| `crates/pvisor-cluster/src/{client,main}.rs` | Typed HTTP client and CLI |
| `crates/pvisor/src/bin/pvisor-worker.rs`, `bin/worker/` | Node loop, watchdog, execution, outbox, Gateway, environment and snapshot integration |

After changing a module, revisit authority and invariants. A native-done optimization must also account for upload lifetime, lease renewal, duplicate completions and GC roots. A pause change must also account for CPU reservations, manual-control ownership and the Gateway delivery barrier.

Only one shard with exclusive write ownership is currently supported. Cross-host compatibility, HA, online metadata compaction, tenant identities and long-running density evidence are covered under [operations and evolution](operations.md#evolution).
