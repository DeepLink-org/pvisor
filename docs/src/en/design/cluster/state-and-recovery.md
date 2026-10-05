# State authority, leases and restart reconciliation

The Controller's runtime state is a view derived from Worker reports. A low-frequency log preserves accepted intents and result identities so queued tasks, control requests and terminal receipts survive restart. Heartbeat deadlines, node pressure and Running confirmations update in memory.

## State classes and consistency {#authority}

| State | Authority | Persistence and restart behavior |
| --- | --- | --- |
| TaskSpec, DAG, environment templates, fork requests | Accepted immutable intents | Transaction replay; unassigned tasks remain queued |
| Assignment keys, generations, control/cancel intents, drain | Acknowledged Controller decisions | Low-frequency persistence; restart cannot change execution identity |
| Terminal state, native results, artifact receipts, evidence retirement | Accepted Worker evidence or explicit administrative decisions | Persistent; matching identities permit rereads/retries |
| Active inventory, deadlines, Worker seen/admission, Running ACK | Latest Worker reports | In-memory updates; restart waits for new reports |
| Ready, expiry, phase counts, tenant/Worker reservations, reference indexes | Derived task/reference indexes | Rebuilt by replay, then corrected through reconciliation |
| Actual processes, VMs, controls and node caches | Worker and native runtime | Controller records cannot replace direct observations |
| Pending terminal delivery and control observations | Worker-local durable outbox/evidence | Exact-identity recovery and retries after Worker restart |

Eventual consistency applies to the Controller's knowledge of actual execution. Task creation, assignments and receipts retain a local commit barrier to avoid losing acknowledged queued work or changing execution identities. There is no distributed consensus, synchronous cross-node heartbeat log or stateless queue.

The Controller converges through periodic Worker `poll` requests, rather than discovering arbitrary nodes and pulling their entire state. Convergence requires the owning Worker to be reachable, report a complete inventory and retain usable local storage. Permanent partitions have no automatic convergence deadline.

## Identity and leases {#lease}

All four `LeaseKey` fields must match:

| Field | Purpose |
| --- | --- |
| `task_id` | Immutable submission identity |
| `worker_id` | Logical node identity |
| `incarnation` | One Worker process instance; restart uses a new value |
| `generation` | Task assignment generation; fences old assignment messages |

The Controller uses Unix milliseconds for `expires_at_ms`. Workers set their watchdog from monotonic request-start time plus the returned lease duration; waiting for HTTP does not extend execution. A late renewal cannot revive an expired run. Stopping follows the executor's native cancellation path and may include a termination grace period.

When an online, confirmed lease expires, the reaper persists its ending. Unknown execution normally becomes `Lost`; failed delivery of a known native outcome preserves that outcome. Cancellation without confirmed termination is not success. `Lost` is terminal and does not establish that external side effects have stopped.

## Task state machine {#phases}

| Phase | Meaning and next step |
| --- | --- |
| `waiting_dependencies` | DAG identity exists; wait for all predecessors' aggregate success |
| `waiting_checkpoint` | Live-fork branch exists; wait for the matching sealed full checkpoint |
| `queued` | Wait for matching and admission |
| `leased` | Assignment committed; not yet confirmed in Worker inventory |
| `running` | Worker confirmed execution identity; native actions have separate observations |
| `paused` / `offloaded` | Native success accepted; CPU reservation is zero, RAM/slots remain reserved |
| `suspending` | Full snapshot sealed; await native termination and delivery |
| `retaining_artifacts` | Native execution ended; delivery lease and bounded upload reservation remain |
| `cancelling` | Cancel accepted; await completion or lease invalidation |
| `succeeded` / `failed` / `cancelled` / `lost` / `suspended` | Terminal; execution reservations released, results and retained references preserved |

The main path is `queued → leased → running → succeeded/failed`; controls, cancellation and delivery add branches. `reconciliation_pending` is orthogonal to phase. After restart, `running` or `paused` with this flag set represents historical state only.

## Controller restart algorithm {#reconcile}

1. Open the log exclusively, validate and replay each frame, and verify the task-retention bound. Complete invalid frames reject startup; partial trailing debris is removed only after successful full replay.
2. Validate the log/store authority binding and rebuild ready indexes, counts, reservations and retained-manifest pins.
3. Mark every nonterminal task with a lease `reconciliation_pending = true`, add it to the reconciliation set and remove its historical deadline from expiry. Preserve resources and references.
4. The owning Worker reports a complete `PollRequest.active` with the same incarnation. Matching keys clear pending and reconstruct in-memory deadlines; first inventory confirmation can advance `leased` to `running`.
5. An unconfirmed `leased` assignment may be redelivered to that same Worker incarnation with the same key. The Worker deduplicates active task identities; completed tasks awaiting receipts must still be reported.
6. Wait for unreachable Workers. Do not release their reservations using historical deadlines or assign their tasks elsewhere.

`active` is a complete bounded inventory, including terminal delivery until Controller acknowledgment and excluding unstarted rejections. Partial inventories can misidentify assignments as unreceived. Its protocol bound is registered slots plus 64 delivery identities. It is not a full terminal-history upload and cannot reconstruct lost TaskSpecs or DAGs.

| Request during pending reconciliation | Behavior |
| --- | --- |
| Exact-key poll / terminal recover | May reconfirm ownership and renew |
| Exact terminal evidence, existing issued-control ACK, unstarted decline | May resolve historical execution evidence; does not authorize arbitrary new actions |
| New native controls, telemetry, artifact uploads, inference-wait progress | Require a fresh Worker lease confirmation first |
| Task/Graph/Worker/count queries | Read the derived view without expiry commits; callers check pending |
| Destructive artifact GC | Refused while any shard lease awaits reconciliation |

## Worker restart and terminal recovery {#worker-restart}

A Worker exclusively owns its state directory. At startup it loads the pending outbox bound to its Controller URL and Worker ID, calls `recover` for known terminal entries from old incarnations, renews/uploads/retries them, then registers a fresh incarnation.

This restores terminal delivery only; it does not adopt unknown live execution from an old process. Pending reconciliation after Controller restart prevents a replacement incarnation using the same Worker ID from taking over until the original Worker confirms ownership or an administrator resolves it. If the Controller never restarted and the old lease is still timed online, normal expiry can terminate its identity.

Persisted pause observations and resume intents can also replay, but alone cannot authorize Gateway reply delivery after restart. Inference waits still require fresh owning-Worker reports.

## Explicit resolution of permanent loss {#resolve-lost}

An administrator copies the complete `lease.key` from the task and calls `POST /v1/tasks/{id}/resolve-lost` or `pvisor-cluster resolve-lost key.json` for an execution awaiting reconciliation.

- Unknown native outcomes become `Lost`.
- Known native outcomes are preserved; abandoned artifact delivery makes the aggregate `Failed`, or `Cancelled` if cancellation was already pending.
- Controller execution reservations and active references are released without automatic retries; retained evidence follows its separate policy.
- Terminal rereads with the same complete key are idempotent; stale generations/incarnations are rejected.

This closes a control-plane identity, without proving remotely that processes, VMs or external side effects have stopped. Operations and business owners should inspect the lost node and external effects before submitting replacement work. Retries use new Task/Run/Attempt identities and preserve the old result.

## Compatibility and implementation {#implementation}

Historical `Renew` frames remain readable and enter reconciliation after replay as hints. Current writes filter all `Renew` changes. `--durable-leases` and `Scheduler::open_durable` have been removed; there is no alternate durable-heartbeat writing mode.

The main code paths are `Scheduler::open`, `reported_key` / `valid_key`, `poll` / `recover`, `resolve_lost` and `maintain_expiry`. `reported_key` permits exact nonterminal identities pending reconciliation; `valid_key` additionally requires confirmed ownership. The Worker watchdog and outbox live in `pvisor-worker.rs` and `bin/worker/outbox.rs`.
