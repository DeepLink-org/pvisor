# pVisor distributed control plane

This crate owns durable task submission, Worker placement/admission and the
cluster lease protocol. `pvisor-worker` embeds the pVisor execution kernel and
uses the same `RunSpec` and `RunResult` as local execution. Reservations are
admission estimates; enforcement evidence comes from native observations and
the Run Bundle.

The Controller is one durable shard with multiple Workers. The current design,
protocol and operational boundaries are maintained in the project documentation:

- [Cluster quickstart](../../docs/src/en/guides/cluster/index.md)
  ([中文](../../docs/src/zh/guides/cluster/index.md)).
- [Architecture](../../docs/src/en/design/cluster/index.md)
  ([中文](../../docs/src/zh/design/cluster/index.md)).
- [Scheduling](../../docs/src/en/design/cluster/scheduling.md): reservations,
  quotas, Worker capability matching and final admission.
- [State and recovery](../../docs/src/en/design/cluster/state-and-recovery.md):
  Worker reconciliation, lease fencing and explicit lost resolution.
- [Storage](../../docs/src/en/design/cluster/storage.md): WAL, terminal outbox,
  artifact integrity and GC roots.
- [Native lifecycle](../../docs/src/en/design/cluster/lifecycle.md): controls,
  local checkpoints, suspend, restore and fork.
- [Service deployment](../../docs/src/en/guides/cluster/service.md): separate
  Controller and live-data failure boundaries.

## Implementation ownership

| Module | Responsibility |
| --- | --- |
| `scheduler.rs`, `scheduler/{graph,indexes}.rs` | Task intentions, placement, reservations and atomic graph/fork creation |
| `scheduler/inference.rs` | Cooperative wait ownership and CPU readmission |
| `journal.rs`, `server/dispatcher.rs` | Single-writer WAL and fsync-before-response batching |
| `artifacts.rs`, `artifacts/{quota,gc}.rs` | Lease-bound evidence, integrity, quotas and collection |
| `physical_memory.rs`, `admission.rs` | Observations and local admission; observations alone never authorize memory release |
| `client.rs`, `server.rs`, `main.rs` | HTTP client, authenticated endpoints and Controller CLI |
| `../pvisor/src/bin/pvisor-worker.rs`, `worker/` | Native execution, Gateway, controls, environment preparation and terminal delivery |

## Local execution checkpoints

A full checkpoint captures CPU, RAM, devices and owned filesystem state.
`checkpoint` continues the source; `suspend` seals and stops it. Restore and fork
create new Run/Attempt identities and private writable state, with independently
charged resources and verified read-only backing sharing.

Checkpoints stay in the source Worker's local SnapshotStore. Continuations and
forks are placed only on that owning Worker with native restore support. Worker
process restart can reuse retained local state, subject to exact runtime
compatibility. Cross-node checkpoint publication, repository import and remote
restore placement are not provided. Controller artifact retention covers the
Bundle, trace and private writable-layer evidence, not complete checkpoints.

## Cooperative inference waits

The opt-in VM Worker Gateway profile uses `enabled = true` and
`release_cpu_on_idle = true`. An Agent sends `x-pvisor-inference-idle: true` only
when its whole guest, including background work, is quiescent. Gateway groups
up to 64 concurrent calls; the Controller releases logical CPU only after a
confirmed native pause. Reply delivery waits for CPU readmission, native resume
and fresh Worker ownership confirmation. RAM and slots stay reserved.

Manual controls revoke automatic pause ownership. Cancellation, stale identities
and Controller restart cannot authorize reply delivery without reconciliation.
See the [wait lifecycle](../../docs/src/en/design/cluster/lifecycle.md#inference)
and its bilingual counterpart for protocol details. Longer outages and
representative workload density remain separate validation work.

## Build and validate

```sh
just cluster-build
just test pvisor-cluster pvisor-core
just test-cluster
just test-cluster-gateway
just test-cluster-vm
just test-cluster-vm-gateway
```

The native gates require actual KVM/FUSE, working user/mount/network namespaces
and compatible libkrunfw; denied devices or namespaces fail the gate. Set
`PVISOR_TEST_LIBKRUNFW_DIR` when firmware is outside the loader's default paths.
Hardware gates run sequentially, while concurrency inside a scenario remains
real. Each gate pins its own executable so unrelated builds cannot invalidate
checkpoint compatibility. Loopback/Unix-socket tests require local networking.

Conventional tests cover durability, fencing, idempotency, local checkpoint
placement, native suspension receipts, inference wait ownership, artifact
integrity and terminal outbox recovery. Passing them does not establish
multi-host throughput, live execution recovery or sandbox density. Performance
results belong in the [benchmark documentation](../../docs/src/en/benchmarks/index.md).
