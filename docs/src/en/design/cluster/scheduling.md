# Scheduling, admission and DAGs

Worker polling triggers Controller matching within a bounded ready window. Controller reservations, Worker-reported availability and final Worker admission must all permit execution. None replaces native resource enforcement.

## Task and node models {#model}

`TaskSpec` includes version, immutable ID, tenant, native `RunSpec`, execution class, `Resources`, label constraints, cache-affinity keys and optional environment, restore, retention, Gateway and CPU QoS requirements. Native policy and run identities remain defined by shared Core types.

`WorkerRegistration` includes node ID/incarnation, total capacity, execution classes, labels and cache keys, plus advertised Gateway, environment, VM-control, checkpoint, artifact-export and CPU QoS/observation protocols. Missing capabilities do not silently downgrade task requirements.

| Resource | Accounting meaning |
| --- | --- |
| `slots` | Concurrent native-execution slots |
| `memory_bytes` | Admission RAM budget, rather than RSS/PSS |
| `cpu_millis` | CPU reservation; 1000 is one logical core's budget |

Worker reservations sum its tasks' current reservations; tenant reservations sum that tenant's tasks across nodes. Explicit tenant quotas limit concurrent resources; unlisted tenants have no such quota. Resource arithmetic checks overflow instead of allowing wraparound.

## Scheduling within a poll {#poll}

1. Validate Worker ID/incarnation, complete active inventory, duplicate keys, resource values and batch bounds. Maintain expiry of confirmed leases.
2. Renew exact keys, send stop instructions for cancellation and confirm received assignments. Maintain existing execution before assigning new work.
3. Redeliver unconfirmed assignments and issued controls with their existing identities. Additional Resume resources are charged before issue and conservatively withheld on redelivery, avoiding reuse of unacknowledged CPU.
4. If the Worker is draining, block new assignments while maintaining execution and delivery. For new work, combine local availability, remaining Controller reservations and valid admission reports.
5. Inspect a bounded ready window, sort by cache affinity and check execution class, labels, environment/restore compatibility, Gateway/QoS capabilities, tenant quotas and artifact headroom.
6. Establish leases, increment generations and reserve Worker/tenant capacity within the scheduling operation. Return assignments after persistence. A definite publication quota refusal restores queue positions and returns no uncommitted new work.

Defaults are `queue_lookahead = 256` and `max_batch = 64`. Unmatched candidates rotate so each poll need not scan terminal history. Affinity keys are optimization hints; bounded windows and rotation do not establish strict fairness, priority, preemption or tail-latency guarantees.

## Indexes and complexity boundaries {#indexes}

Ready uses ordered sequence entries and a task-to-position index, removing terminal/cancelled tasks. Phase counts update incrementally. Expiry is ordered by deadline/task; active tasks are indexed by Worker. DAGs index successors and outstanding dependency counts. Pending reconciliation leases are excluded from expiry.

Poll candidate work is bounded by window/batch limits, dependency progress visits affected nodes, and counts avoid whole-history scans. Registration, replay, large administrative responses, GC root collection and retained history have separate costs. Locally bounded algorithms do not make every system operation constant-time.

Each TaskRecord has one boxed authoritative allocation; DAGs retain topology and references instead of copying RunSpecs. The default retention bound is 1,000,000 task records. Terminal artifact GC does not delete task identities, and metadata history compaction is not implemented.

## Final admission and node pressure {#admission}

Workers recheck local conditions before starting. An unstarted assignment may be declined with its exact key. The Controller persists the decline, releases reservations and requeues it; a later assignment has a new generation. Decline is not a retry mechanism for already executed work.

Optional Linux admission samples CPU/memory PSI, CPU affinity and visible cgroup v2 limits. Stale pressure reports do not authorize looser admission; reports older than the lease window restrict new work. Final Worker availability can be below registered capacity.

Optional CPU reservation overcommit requires an explicit policy and finite node CPU quota. Kernel quotas, BE `SCHED_IDLE`, LS core scheduling and logical reservations act at different layers. Pause releases logical CPU only. There is no default RAM overcommit, and a changed residency sample does not directly authorize reusable RAM capacity.

Physical memory, CPU-rate and node-memory interfaces report lease-bound observations without persisting every sample by default. They are not yet billing, tenant cost aggregation or cross-node capacity-prediction systems.

## DAG submission and progress {#dag}

A graph accepts 1–256 same-tenant nodes, at most 4096 edges and a 2 MiB specification. Before submission it checks unique Task/Run IDs, internal references, self-loops, duplicate edges, acyclicity and total task count. Existing tasks cannot be adopted. All nodes and topology are created in one transaction frame.

Root nodes enter `queued`; others enter `waiting_dependencies`. Only aggregate predecessor `Succeeded` reduces outstanding dependencies. Native success alone does not release dependencies when required artifacts remain undelivered. Failure, cancellation, Lost and other nonsuccess terminal states block affected successors; independent branches can continue. Propagation uses a work queue instead of deep recursion.

Graph cancellation records intent and updates nodes in one transaction. Unstarted nodes can finish; running nodes enter cancellation while retaining reservations until observations arrive. Queries reconstruct graphs from original topology and task records; graph nodes have no second set of execution identities.

A graph does not automatically mount predecessor artifacts into successor workspaces. Callers establish data dependencies through pinned inputs, environments or explicit delivery conventions. Graph idempotency requires the same ID, node order, dependencies and full specification.

## Reservation handoffs {#reservations}

| Situation | CPU | RAM / slots |
| --- | --- | --- |
| Leased / Running | Full reservation | Full reservation |
| Pause/offload request not yet acknowledged | Previous charge retained | Full reservation |
| Native pause/offload success accepted | 0 | Full RAM and slots retained |
| Resume issued, ACK outstanding | Full reservation | Full reservation |
| Resume failed | Increased charge retained until later settlement | No optimistic release |
| Cancelling / pending reconciliation | Current charge retained | Current charge retained |
| NativeDone handoff accepted | 100 millis | 16 MiB, 0 slots; only if the original budget accommodates it |
| Native execution ended without handoff | Previous charge until final acceptance | Same |
| Suspended or final terminal accepted | 0 | 0; evidence references are retained independently |

The delivery protocol allows up to 64 additional delivery identities; the current Worker separately bounds local delivery concurrency at 16. Where older protocols cannot use the handoff, retaining the original budget is the safe fallback.

Implementation primarily resides in `scheduler.rs`, `scheduler/indexes.rs`, `scheduler/graph.rs` and `admission.rs`. Native policies and local pressure rechecks reside in the Worker.
