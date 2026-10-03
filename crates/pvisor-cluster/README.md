# pVisor distributed control plane

This crate owns durable task submission, worker placement/admission and the
cluster lease protocol. `pvisor-worker` embeds the existing pVisor execution
kernel; a cluster task carries the same `RunSpec` and returns the same
`RunResult` as local execution. Cluster reservations are admission estimates;
actual enforcement evidence stays in the executor observations and Run Bundle.

The implementation currently provides a **single durable controller shard**
with multiple independently running workers. Production-scale distribution,
stateful sandbox lifecycle alignment and density claims remain unverified.

## Research basis and required final behavior

Primary sources inspected on 2026-10-04:

* [Kimi K3 report, §5.3.2](https://arxiv.org/html/2607.24653v1#S5.SS3.SSS2):
  AgentENV uses microVM isolation, incremental checkpoints, pause/resume,
  fork and recovery snapshots. Its image and memory sharing support dense
  workloads. The paper reports checkpoint/resume minima of 133/49 ms;
  these are source results, not pVisor measurements.
* [AgentENV source](https://github.com/kvcache-ai/AgentENV) and
  [deployment documentation](https://kvcache-ai.github.io/AgentENV/dev/deployment/kubernetes.html):
  gateway, scheduler and per-node runtimes are separate services.
* [DSec report, §§3,5–7](https://arxiv.org/html/2609.22978v1): heterogeneous
  backends, composable versioned environment layers, on-demand reads with
  local writes, memory reclamation/sharing, CPU QoS and rollout lifecycle
  coordination. Placement accounts for in-flight load and workers retain
  final admission authority. Its distributed and GPU-training architecture
  motivates the controller/worker split; its published scale is not an
  acceptance result for pVisor.

| Required behavior | Current authoritative implementation | Remaining acceptance evidence |
| --- | --- | --- |
| Distributed user task execution | HTTP submit/show/cancel, multi-worker process execution, common RunSpec/RunResult | Multi-host workload, task graph/scaffold integration, artifact collection |
| Backend/isolation selection | Exact execution class and label matching; host/rootless/container/VM workers | VM/container distributed execution and host-policy failure experiments |
| Reliable control | fsync-before-ack WAL, fencing, cancellation, expiry, drain, idempotent submit/completion | Restart during live workloads, disk-full faults, worker restart outbox recovery |
| Scalable scheduling | Bounded ready window, indexed expiration, batched leases, reservations, tenant quotas | Sharding, replicated authority, group commit, admission/load measurements and large-scale benchmarks |
| Independently versioned base/workspace/toolkit layers | Worker-owned immutable lower layers and private upper; existing VM/image cache settings | Template registry, immutable digest resolution, per-task layer composition and artifact distribution |
| AgentENV pause/resume | Durable lease-bound desired/observed pause/offload/resume; worker invokes native Run controls; CPU reserved before resume | Hardware-backed distributed VM lifecycle experiments and inference-wait coordination |
| Incremental execution checkpoints, fork and recovery | Ordinary Job executor explicitly rejects full execution capture | Connect full VM state capture/restore to Job driver; independent forks, compatible runtime identity, remote storage and recovery tests |
| Dense memory use | VM size derived from task admission budget; host-local shared RAM/cache profile options; offload observations retain RAM charge | Physical resident-memory accounting, controlled reclaim/overcommit and measured density improvement |
| CPU QoS/controlled overcommit | CPU capacity accounting only | BE/LS policies, pressure-aware overcommit and latency/isolation verification |
| RL preemption/resumption | Lease protocol and per-task evidence | Preserve rollout/scaffold state independently of GPU scheduling; resumable checkpoint coordination |
| Access control and observability | Distinct admin/worker API credentials, explicit task environment, trace and local Bundle | Per-tenant/node identities, TLS deployment, centralized artifacts/traces, dynamic task policies/Gateway integration |

Completion requires the whole matrix, not only passing scheduler tests. The
unconnected ordinary-Job checkpoint path is documented in
`crates/pvisor/src/cli/checkpoint.rs::execution_blocker`; VM pause/offload is
not a complete checkpoint, portable migration or a claim of zero resident RAM.

## Run a controller and workers

Build from the workspace root:

```sh
just cluster-build
```

Use separate shells. Supply distinct random credentials through environment
variables; the values below are local examples. For multiple hosts, set the
controller listen address and worker URL to reachable addresses.

```sh
export PVISOR_CLUSTER_TOKEN=local-admin-example-1234567890
export PVISOR_CLUSTER_WORKER_TOKEN=local-worker-example-1234567890
target/debug/pvisor-cluster serve --journal /tmp/pvisor-controller/journal
```

```sh
export PVISOR_CLUSTER_WORKER_TOKEN=local-worker-example-1234567890
target/debug/pvisor-worker --id worker-1 --state /tmp/pvisor-worker-1 \
  --backend host --slots 16 --memory-bytes 8589934592 --cpu-millis 4000
```

Run another worker with a different ID and state directory. `host` is an
explicit trusted process backend. The worker default is `rootless`; unsupported
isolation fails rather than falling back to host execution.

Submit the checked-in task example:

```sh
export PVISOR_CLUSTER_TOKEN=local-admin-example-1234567890
target/debug/pvisor-cluster submit crates/pvisor-cluster/examples/task.json
target/debug/pvisor-cluster show hello
target/debug/pvisor-cluster workers
target/debug/pvisor-cluster cancel hello
target/debug/pvisor-cluster drain worker-1
target/debug/pvisor-cluster drain worker-1 --resume
```

Every task has an immutable ID. Submitting the exact specification again
returns its existing record; submitting different content under that ID fails.
Use a new task ID for a deliberately repeated workload. The control plane
does not automatically re-execute work with unknown side effects.

For a task on a VM worker, control its current attempt with an explicit
idempotency key:

```sh
target/debug/pvisor-cluster control agent-1 pause --request-id wait-model-1
target/debug/pvisor-cluster show agent-1
target/debug/pvisor-cluster control agent-1 offload --request-id idle-1
target/debug/pvisor-cluster control agent-1 resume --request-id continue-1
```

The response records the request; `show` exposes the pending/issued/observed
transition. Wait for a terminal control record before issuing another control.
Retry the same action and request ID after a timeout. Different content under
that ID fails. The command is fenced to the live lease and its revision;
an expired attempt cannot acknowledge it or control a later attempt. Cancelling
the task aborts outstanding controls.

CPU is released only after the worker durably records native pause/offload
success and the controller durably accepts that observation. Resume waits for
worker, tenant and node-local admission, then charges CPU before issuing the
command. Resume and new assignments share the same per-poll local budget;
unacknowledged resume redelivery conservatively holds its additional charge
out of local availability. Control failure keeps the prior charge until
completion or lease expiry. Slots and the full RAM budget remain reserved
while paused or offloaded. Native `mincore` residency samples do not account
for all compressed/shared host allocations and are not proof of freed memory.

## Worker profiles and scheduling

`--config` accepts a narrow TOML **worker profile**, not the full CLI RunConfig.
Unknown fields fail parsing, so unconnected controls cannot silently disappear.
Task invocation, environment, timeouts and policies come from RunSpec. Stdin
is closed and stdout/stderr are captured with the RunSpec limit (maximum 1 MiB
per stream). Local output is additionally bounded after lossy UTF-8 decoding.

Example VM worker profile:

```toml
lower_layers = ["/srv/task-input", "/srv/toolkit"]

[vm]
rootfs = "/srv/rootfs"
image_store = "/srv/pvisor-images"
ram_compression = false
# memory_pool = "/private/path/pool.sock" # experimental Apple Silicon pool

[overlaynet]
mode = "auto"
policy = "deny"
```

`--backend vm` derives guest memory from task memory bytes (whole MiB) and
vCPU count from CPU millis rounded up to CPUs, subject to libkrun's eight-vCPU
limit. Each attempt gets a separate RAM backing. VM rootfs apply is protected.
Base image/rootfs provisioning and lower-layer distribution currently belong
to deployment; the controller does not copy arbitrary host paths across nodes.
`--backend container` uses the profile's `container` settings.

VM workers advertise their supported control actions. Whole-VM offload is
not advertised with the experimental Apple Silicon memory pool/cold pager;
pause and resume remain available. Older workers without the control protocol
cannot receive these commands. A cloneable native `RunControlHandle` lets
the worker observe completion and lease expiry while waiting for VM controls,
including asynchronous startup. Run deadlines continue while suspended.

`--label arch=x86_64` and task labels constrain admission. `--cache-key DIGEST`
advertises host-resident immutable content; the ready window favors matching
keys. Cache inventories are currently operator-supplied hints, not residency
proof, runtime measurements or distributed prefetch. A bounded rotating window
prevents incompatible tasks hiding later ready work. This is pull scheduling,
not an implementation of DSec's power-of-k placement strategy.

`serve --quotas FILE` loads a JSON map of tenant IDs to concurrent `Resources`
limits. For example `{"team":{"slots":8,"memory_bytes":8589934592,"cpu_millis":4000}}`.
Unlisted tenants have no configured quota. Quotas constrain aggregate
reservations; tenant names are assigned by the trusted admin client and are not
separate authentication principals. Worker resource availability presently
comes from its configured capacity minus local reservations.

## Failure and persistence contracts

* Controller journal entries are checksummed atomic transaction frames. The
  writer lock rejects a second owner. Only a partial final frame is truncated
  after crash; complete corruption fails closed. Writes fsync before mutation
  acknowledgement. A write failure poisons the writer until restart.
* Leases bind task ID, generation, worker ID and process incarnation. Stale
  results and late renewals cannot overwrite a current record. Losing an
  assignment response redelivers the same key without another reservation.
* The worker uses request-start monotonic time for lease deadlines, including
  an independent per-run timer. HTTP waits cannot keep work running beyond an
  expired local lease. Cancellation requests stop the run via its native
  pVisor cancellation handle. Cooperative/forced termination still takes its
  configured grace period; fencing controls records, not arbitrary external
  side effects.
* A live cancellation remains `cancelling` and retains its reservation until
  completion acknowledgement. Lease expiry yields `lost` (outcome unknown),
  including when cancellation acknowledgement was lost. `lost` is terminal
  for this submission and is not a successful or confirmed stopped result.
* Worker assignment, control acknowledgement and completion evidence lives under
  `STATE/tasks/TASK-GENERATION`, alongside native pVisor records and trace.
  Controller records contain small RunResults. Local Bundle paths are not
  automatically uploaded or made portable. Completed evidence is retained
  after acknowledgement; GC, journal compaction and durable restart delivery
  are pending. A restarted worker uses a fresh incarnation and waits for old
  leases to expire; this does not prove old workload processes have stopped.
* Drain blocks new reservations and lets existing work finish. Stopping the
  worker with SIGINT/SIGTERM requests native cancellation and sends completion
  evidence before exit. SIGKILL is an unconfirmed loss case.

API routes: admin credentials can submit/read/cancel/control tasks, list/drain workers
and read counts; worker credentials can register/poll/complete/acknowledge controls. Health
exposes the protocol version. Both roles are trusted deployment services.
TLS termination and node/tenant credential issuance are deployment work still
to implement. The HTTP request limit is 4 MiB; the single shard retains at most
one million task records by default.

## Validation and next implementation gates

```sh
just test pvisor-cluster pvisor-core
cargo nextest run --locked -p pvisor --test cluster_execution
cargo nextest run --locked -p pvisor --lib -E 'test(runtime::run::) or test(executor::vm::control::)'
cargo clippy --locked -p pvisor-cluster --all-targets -- -D warnings
cargo clippy --locked -p pvisor --bin pvisor-worker --test cluster_execution -- -D warnings
```

`just test-cluster` combines the two test commands above. Loopback HTTP tests
require permission to bind ports; they fail rather than silently skipping
when the execution environment denies networking.

The scheduler tests cover restart reservations/idempotency, partial journal
recovery/corruption rejection, cancellation/expiry, stale incarnations, local
admission, quotas, cache preference and bounded queue rotation. End-to-end
tests start a real HTTP service and independent Worker binaries, checking two
workers, credential projection, process-tree cancellation, lease expiry after
controller outage, bounded binary output and API role separation. These run on one host and do not
establish multi-host scale or VM snapshot fidelity.

VM protocol tests use synthetic observations to verify durable control history,
reservation changes, malformed/stale acknowledgements, shared-pool capability
admission, cancellation, capacity-denied resume and lost-response redelivery
through controller restart. The HTTP lifecycle test uses a synthetic worker
client, not a hardware VM. Native control regressions cover cancellation,
readiness, control acknowledgements and backing-file ownership; they do not
establish hardware-backed distributed VM success.

Next gates, in dependency order:

1. Validate distributed controls on VM hardware, connect inference-wait
   coordination, and add physical node/process memory and pressure accounting.
   Never release memory merely because a desired state says idle or a single
   backing-file residency sample is zero.
2. Connect full execution snapshot/restore and immutable checkpoint lineage
   to ordinary Job attempts, then fork, restore and cross-node recovery.
3. Implement immutable task environments and remote output collection,
   Gateway/scaffold state and idempotent worker restart delivery.
4. Add load/pressure reporting, CPU QoS and controlled memory/CPU overcommit;
   shard/replicate authority and group durable operations.
5. Measure completed useful tasks/sec, p50/p95/p99 step and lifecycle latency,
   resident RAM per live/waiting sandbox, CPU time, cache/network bytes and
   failure recovery under identical workloads and baselines. Density or speed
   improvements require measured VM/task experiments, not scheduling counters.
