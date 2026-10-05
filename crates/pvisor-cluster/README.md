# pVisor distributed control plane

The complete design is maintained in the project documentation:
[中文](../../docs/src/zh/design/cluster/index.md) ·
[English](../../docs/src/en/design/cluster/index.md). It covers state authority,
reconciliation, scheduling, native lifecycle, storage, APIs and operations.

For a verified walkthrough with two single-slot Workers and hard CPU/memory
limits, start with the [Cluster quickstart](../../docs/src/zh/guides/cluster/index.md)
([English](../../docs/src/en/guides/cluster/index.md)).

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
| Distributed user task execution | HTTP submit/show/cancel, durable atomic dependency graphs, multi-worker process execution, common RunSpec/RunResult; remote native Bundle, trace and private VM writable-layer retention; Attempt-local Gateway and model/tool Agent loop | Multi-host workload, persistent scaffold state, external artifact distribution |
| Backend/isolation selection | Exact execution class and label matching; host/rootless/container/VM workers; real HTTP-dispatched VM execution on Linux | Multi-host VM/container execution and host-policy failure experiments |
| Reliable control | fsync-before-ack WAL, fencing, cancellation, expiry, drain, idempotent submit/completion; unstarted rejection/requeue; durable terminal-result outbox and restart export/delivery | Recovery of live execution, disk-full faults, multi-host failure tests |
| Scalable scheduling | Bounded ready window with cancelled entries removed, indexed phase counts/expiration, batched leases, reservations, tenant quotas; bounded single-writer HTTP queue and fsync-before-response WAL group commit; streaming replay, compact graph topology and boxed task storage; single-controller million-record history and dense-ready validation | Sharding, replicated authority, multi-host admission/load measurements and HTTP/task-throughput benchmarks |
| Independently versioned base/workspace/toolkit layers | Durable immutable template registry; lease-bound revision handles; VM worker composes native lazy-cache layers with private upper and shared live read mounts; real Linux VM composition/upper isolation gate; independent Worker states fetch pinned read-only S3 layers without publisher storage | Multi-host distribution deployment and measured startup/density benefit; container composition |
| AgentENV pause/resume | Durable lease-bound desired/observed pause/offload/resume; bounded inference waits connected to the Attempt Gateway and Worker; real Linux VM gates verify CPU readmission, manual pause ownership and controller SIGKILL/restart with a lost durable Ready response | Parallel-call fault gates; networked RAM reclamation, longer outages and multi-host VM lifecycle experiments |
| Incremental execution checkpoints, fork and recovery | Full CPU/RAM/device/owned-overlay capture with native forest ownership transfer and direct RAM sealing; Linux compressed incremental RAM recapture of restored VMs with independently retained inherited frames; coordinated save-and-stop of running, paused and offloaded Linux VMs without guest resume; durable live capture/fork handoff and atomic branch creation from sealed checkpoints; same-Worker continuation into new Run/Attempt with lineage, private writable files, shared verified read-only lower copies and private COW RAM; real Linux cold restore after Worker restart; immutable FS/S3 full-checkpoint transport and verified same-host recovery after deletion of the original snapshot object; opt-in Worker publication with durable terminal retry and controller-bound receipts, compatible cross-Worker import after source deletion and controller restart; opt-in native v5 capture retains immutable lower inodes across initial/restored-VM recapture with supervisor-owned seals and slot-bound references; different VMs share lower inodes on their first capture without temporary data copies on pool hits; private file payloads are sealed directly from authenticated frozen roots without an intermediate data-tree copy and retain independent 64 KiB compressed frame references, reusing unchanged content across captures without recompression on verified hits, with native encoding-work counters | Capture-side private filesystem deltas and cross-host runtime compatibility/recovery tests |
| Dense memory use | VM size from task budget; shared read-only snapshot RAM with private COW writes; bundled-kernel restores skip duplicate firmware loading; sparse RAM capture/publication; opt-in VM/Worker RSS/PSS, system and cgroup memory observations; dedicated user-systemd Worker scope; native hibernation releases all reservations; durable post-teardown artifact delivery reuses execution slots with bounded memory/CPU reservations | Shared-cache/hugetlb coverage, controlled reclaim/memory overcommit and workload density benchmarks |
| CPU QoS/controlled overcommit | CPU reservations; optional Linux PSI, affinity and visible cgroup v2 CPU/memory admission; opt-in bounded CPU reservation overcommit under a finite local quota; fresh pressure gating of admission/resume; whole-Worker kernel CPU quota; opt-in native BE SCHED_IDLE and shared LS core scheduling group; class-preserving capture/fork/restore; opt-in native per-Attempt live rates and durable final CPU counters; controlled SMT/finite-quota native search experiment | Billing/rollout aggregation, whole-node CPU cost and representative agent workload latency/density benchmarks |
| RL preemption/resumption | Lease protocol and per-task evidence | Preserve rollout/scaffold state independently of GPU scheduling; resumable checkpoint coordination |
| Access control and observability | Distinct admin/worker API credentials, explicit task environment; lease-bound verified Bundle/trace/private VM upper downloads; global unique-object byte/count quotas, online evidence retirement/reclamation and durable download protection; Gateway capability placement and task-scoped model authorization with Worker-owned provider credentials | Per-tenant/node identities, TLS deployment, per-tenant artifact quotas and replicated storage, dynamic task policy updates |

Completion requires the whole matrix, not only passing scheduler tests. The
ordinary-Job CLI's unconnected execution restore/fork path is documented in
`crates/pvisor/src/cli/checkpoint.rs::execution_blocker`; VM pause/offload is
not a complete checkpoint, portable migration or a claim of zero resident RAM.

The overlay rebinding prerequisite preserves ordered lower layers, private upper,
work/preimage directories, saved guest file handles and directory cookies. It
also relocates original lower inode identities so later copy-up of an unseen
hard-link alias joins the existing upper inode. Descriptor-ring tests remove the
original tree before restoring and verify a second copy/fork. The coordinator
must verify the complete owned-tree inventory before rebinding, including files
never opened by the guest. Missing copied roots, hard-link origins and invalid
copied state fail without changing the saved server state. Native capture can
collect separately owned backing directories into one sealed environment tree.
Controller-directed cold continuation reuses this rebinding with new Attempt
audit identities and unchanged authorization rules. Linux restore can share
verified lower copies through durable Attempt references. An opt-in host-owned
pool retains those immutable lowers during recapture of initial and restored
VMs, including the first capture of different VMs. Cross-host recovery remains
work.

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

## Worker-reconciled runtime state

The default Controller keeps heartbeat deadlines, Worker admission reports and
Running acknowledgements in memory. Ordinary renewal-only polls neither append
to the journal nor fsync it. Assignments, accepted task/DAG specifications,
control/cancel intents and final receipts retain their existing durable contract.
This also preserves queued tasks that no Worker has received yet.
When a poll also requests a new assignment or pending control, a definite
journal quota refusal defers that new work while returning existing renewals.
Refused expiry commits retain reservations and roots. Uncertain I/O/fsync
failures still fence the Controller; first terminal/intent commits can still fail
when metadata storage is full.

After restart, each unfinished historical lease has
`reconciliation_pending: true`. Its `phase` and deadline are historical hints;
resources remain reserved, and the reaper does not infer completion from an old
on-disk deadline. The Worker's regular poll supplies a complete bounded inventory
of active keys, including terminal delivery until acknowledgement. Matching
Worker/incarnation/generation confirms ownership and reconstructs the runtime
view. An absent unacknowledged assignment can be redelivered to the same Worker
incarnation with the same key; the Worker deduplicates active task IDs. An
unreachable Worker does not permit reassignment to another node.

New native control requests, telemetry and artifact uploads wait for confirmed
ownership. Exact terminal evidence and issued control acknowledgements can be
accepted during reconciliation. Destructive artifact GC waits until pending
leases have been reconciled or explicitly resolved; persisted roots and receipts
remain authoritative during this transition.

For a permanently unavailable Worker, save the task record's complete `lease.key`
as JSON and run `pvisor-cluster resolve-lost key.json`. The admin API is
`POST /v1/tasks/{id}/resolve-lost` with that key. It fences the exact execution,
retains known native outcomes, releases reservations, and never automatically
retries the command. A changed generation/incarnation is rejected. Use the same
key for an idempotent retry. A replacement Worker incarnation must wait until
its predecessor's owned executions have been reconciled or resolved.

Existing journal files are readable without conversion; historical renewal
frames are accepted as hints and require fresh Worker reconciliation. Current
lease deadlines are reconstructed in memory and are no longer persisted. The
old `--durable-leases` switch and `Scheduler::open_durable` API have been removed.
Task, graph, live-fork, Worker and counts queries read the latest derived view
without triggering expiry commits; the periodic reaper and Worker poll maintain
expiry. Thus monitoring remains readable at a definite metadata quota limit,
with normal asynchronous expiry visibility. Terminal outbox recovery also defers
quota-refused expiry of other executions while renewing its exact known keys. The
Worker's monotonic lease watchdog remains active; Controller outages beyond its
local deadline stop execution. This implementation supports a single Controller
and retains low-frequency metadata persistence, not an entirely stateless task
queue or multi-Controller takeover.

## Concurrent requests and WAL durability

HTTP scheduling requests use one dedicated writer thread and a queue of up to
256 waiting operations. A full queue returns HTTP 503 without executing the
rejected operation. Retry with the same immutable task specification, lease key
or control/fork request ID. Once queued, an operation continues if its client
disconnects; a timeout still requires an idempotent retry to learn its outcome.
The single periodic lease reaper reserves the next available queue slot under
overload, so new requests cannot continually displace expiry maintenance.

The writer collects requests already waiting in the queue and shares one WAL
`fsync` across their transactions. It stops collecting after 64 operations,
4 MiB of written frames or 2 ms of processing. These are checks between
operations: a single operation is never split to meet a threshold, and filesystem
sync time is additional. An idle request receives no deliberate batching delay.
Read-only and renewal-only groups do not sync in the default mode. Persistent changes through the synchronous `Scheduler` API and the artifact
GC retirement callback retain immediate commit durability.

Each transaction keeps its existing checksum and frame boundary; graph creation
and fork branches remain atomic within their original transaction. The writer
holds the scheduler lock through the group's durability barrier, and withholds
both success and conflict responses until that barrier succeeds. External
artifact GC and download eligibility checks use the same lock, so they cannot
observe speculative state. An uncertain write or sync failure rejects every
group response and subsequent scheduler access with HTTP 503; `/health` also
returns 503 after the dispatcher stops. Restart and WAL replay decide which
complete frames survived; the controller never rolls back speculative state and
resumes serving from it. A truncated last frame remains
crash debris, while a complete checksum failure remains fatal corruption.

Deterministic tests verify six state-changing transactions share one sync,
ordered reads and conflicts, each collection threshold, queue rejection,
disconnected clients and an actual OS sync failure. A real HTTP/controller test
submits 96 tasks concurrently, cancels 16, assigns the remaining 80 across eight
synthetic worker registrations, and verifies acknowledged leases and terminal
receipts across two controller SIGKILL/restart cycles without duplicate submits.
These checks establish durability and batching behavior; they do not measure
distributed execution throughput, power-loss recovery or agent density.

## Run an Agent through an Attempt-local Gateway

Build the optional model driver with `just cluster-build-gateway`. A default
`just cluster-build` Worker rejects an enabled Gateway profile; tasks requiring
Gateway never fall back to a Worker that lacks the advertised capability.

Configure routes on the Worker, independently of the controller and task:

```toml
[overlaynet]
mode = "auto"

[gateway]
enabled = true
level = "dialogue"

[[gateway.routes]]
name = "agent-model"
upstream = "https://your-model-service.example/v1"
api_key_env = "AGENT_MODEL_PROVIDER_KEY"
```

Set `AGENT_MODEL_PROVIDER_KEY` in the Worker service environment and pass this
profile through `pvisor-worker --config worker.toml`. Inline `api_key` values
are rejected. A configured `api_key_env` must resolve to a nonempty Worker
credential before registration (including Gateway's existing provider aliases).
Public registration records contain only route patterns, protocol version and
capture level, never upstream endpoints or provider credentials.

Add the following top-level field to a task and grant the same model in
`run.capabilities.models`:

```json
{
  "gateway": {"version": 1, "level": "dialogue", "models": ["agent-model"]}
}
```

`models` lists concrete model IDs required for placement. It does not itself
grant access: submission fails unless the RunSpec model capability allows each
required ID. Worker route patterns and RunSpec model patterns are intersected
for each request. A wildcard Worker route cannot authorize an undeclared model.
Capture levels must match exactly; `full` cannot silently replace `dialogue` or
`summary`. Models may use the existing Gateway OpenAI/Anthropic routes and
forwarding configuration. The Worker validates these routes before admission.

Each assigned Attempt starts its own loopback data/admin listeners using actual
ephemeral bindings, injects client base URLs and local placeholder credentials,
and writes model requests, responses and tool messages into the same native
trace as executor facts. VM clients use the existing OverlayNet bridge to reach
their Attempt's Gateway. Provider credentials remain in the Worker; child
processes receive only their explicit RunSpec environment and injected local
client settings. The task-scoped controller preserves existing network policy
checks. This is not a per-tenant host authentication boundary: untrusted tasks
must use an isolated executor, and production node/tenant identities remain
required.

Resolved listener addresses are persisted in the Attempt configuration/Run
record. Listener shutdown is part of Attempt teardown. A Gateway uses one
asynchronous I/O thread instead of a CPU-count-sized Tokio scheduler pool for
each sandbox; blocking operations may still create helper threads. This removes
per-Attempt scheduler-thread multiplication, but is not a measured Agent density
or throughput improvement.

Validation commands:

```sh
just test-cluster-gateway
# Linux KVM/FUSE, host Python3, and a regular firmware directory are required:
PVISOR_TEST_LIBKRUNFW_DIR=/path/to/firmware just test-cluster-vm-gateway
```

The Agent fixture consumes deterministic model-service replies but runs real
Python tools: it writes a module, runs three assertions in a subprocess, returns
the tool result to the model and verifies that an unauthorized model gets HTTP
403 without reaching the upstream. The ordinary test covers actual HTTP
controller placement, a legacy Worker rejection, credential isolation, mixed
executor/Gateway trace and verified remote Bundle retention. The native gate
composes separately versioned Python and scaffold toolkits over an immutable
base and runs two concurrent KVM guests with private writable workspaces. Held
model replies prove both VMMs are live simultaneously. Native identity, vCPU
threads, resident guest RAM, separate listener ports, tool outputs, trace,
Bundle lineage and teardown are checked. This establishes protocol and native
execution correctness, not model quality or production-scale performance.

Gateway/network-enabled native execution does not yet have the offline-only
checkpoint continuation guarantee. Cooperative inference waits release CPU;
automatic inference-wait offload with demonstrated RAM reclamation,
persistent scaffold/rollout state, external artifact distribution and real-model
workload density measurements remain separate acceptance work.

## Execute a durable task graph

A graph atomically commits multiple ordinary task specifications and their
success dependencies. It gives an external agent/scaffold durable orchestration
state independent of the submitting process. DSec's
[§6.2](https://arxiv.org/html/2609.22978v1#S6.SS2) motivates retaining rollout
authority outside preemptible training jobs; this DAG is an orchestration
building block, not a complete rollout/scaffold state store.

```sh
target/debug/pvisor-cluster graph submit crates/pvisor-cluster/examples/graph.json
target/debug/pvisor-cluster graph show hello-graph
target/debug/pvisor-cluster show graph-verify
target/debug/pvisor-cluster graph cancel hello-graph
```

The admin endpoints are `POST /v1/graphs`, `GET /v1/graphs/{id}` and
`POST /v1/graphs/{id}/cancel`. Worker credentials cannot access them.
Upgrade the controller before submitting graphs. Existing Workers still receive
ordinary ready-task assignments. Older controllers cannot replay graph journal
events; rollback requires a compatible controller or an appropriate pre-upgrade
backup, rather than discarding committed graph state.
`TaskGraphSpec` contains `version`, immutable `id`, `tenant` and `nodes`;
each node contains a complete `task: TaskSpec` and `depends_on` task IDs.
IDs remain cluster-wide, including Run IDs; a graph cannot adopt an existing
task. All nodes share its tenant. Graphs accept 1–256 nodes, at most 4,096 edges
and at most 2 MiB of serialized specification. Missing, repeated, self or cyclic
dependencies and invalid task specifications reject the entire submission.
Retry identical content under the same graph ID after a timeout; changed content
conflicts. Submit new graph/task/Run IDs for a deliberately repeated workload.

Roots enter the ordinary ready queue. Other nodes are `waiting_dependencies`
without leases, slots or resource reservations. A node enters the ready queue
only after **all** predecessors are durably `succeeded`, including required
Bundle retention. Placement then uses the ordinary execution class, environment,
labels, quota and Worker admission policy. Dependencies impose ordering, not
co-location. Each node returns its own fenced RunResult and optional Bundle.
Input values, files and environments must be explicitly provisioned in each
RunSpec; graph edges do not transfer a workspace or interpolate predecessor
output across hosts.

A failed, cancelled, lost or suspended predecessor marks unstarted dependent
nodes `failed`, with no native RunResult and an explanation that execution never
started. This propagates through that dependency chain; independent nodes may
still finish. `lost` remains an unknown execution outcome, and neither the graph
nor controller retries unknown side effects. A sealed/suspended VM is not a
successful completed graph step; continuation still requires the explicit
checkpoint/restore workflow.

Graph cancellation durably records intent and cancels all unfinished nodes in
one transaction. Waiting/queued nodes become `cancelled`; active nodes use the
normal `cancelling` lease path and retain reservations until completion or
expiry. Repeated cancellation retains the first request timestamp. Completed
graphs keep their outcomes. The graph phase is `queued` before the first lease,
`running` while work remains, `cancelling` while a graph cancellation is pending,
then `succeeded`, `failed` or `cancelled` from all node outcomes. A failure or
unknown outcome remains `failed` even if the remaining work was cancelled.

Graph creation shares one fsync-before-ack WAL frame with every node. Dependency
activation/failure is deterministic with the predecessor's terminal frame, so
replay cannot expose half a graph, double-activate a join or rerun a completed
step. Reverse-edge indexes visit affected descendants when a terminal event
arrives; Worker polling never scans blocked graph nodes. Graph history remains
subject to the controller's existing task retention limit.

Scheduler tests exercise diamond joins, restart/idempotency, failure and expiry,
graph cancellation, bounds, deep failure chains and incomplete-frame recovery.
The real HTTP test assigns successive steps to two independent host Workers,
checks native Bundle evidence and exactly one execution per step, prevents
failed/cancelled successor side effects, checks admin/worker role separation,
and reopens the controller journal to verify final graph state. This does not
establish multi-host transport, shared-workspace portability, dynamic graph
extension, arbitrary agent-loop checkpointing or RL preemption recovery.
The native environment gate also executes a two-step graph in real KVM VMs,
checks the blocked node has no lease/reservation, verifies ordered completion
after Bundle retention and checks both independent writable environments and
native VM Bundle identities.

## Retain and download native execution evidence

Set `"retain_bundle": true` in a task specification to require retention of
the native `run-bundle.json` before successful cluster completion. The default
is false. Only workers advertising the artifact protocol can receive such a
task. This requirement is part of the immutable submission specification.
The checked-in `hello` task example enables retention.

To require a trace and/or a VM's private writable layer, specify:

```json
{
  "retain_artifacts": {"version": 1, "trace": true, "workspace_upper": true}
}
```

At least one flag must be true. This field also requires the native Bundle,
independently of the legacy `retain_bundle` flag. Extended requests require
a Worker advertising the matching version and capabilities; a Bundle-only
Worker cannot silently satisfy them. `workspace_upper` requires native VM
isolation; host and rootless processes can request traces. The new strict outer
field makes older controllers reject an unsupported request. Both fields remain
absent from legacy serialization when unset.

The downloaded files are `run-bundle.json`, optional `trace` (the original
pVisor fact-journal format) and optional `workspace-upper.tar`. Tar entries are
rooted at `upper/`: private files, directories, symlinks and deletion whiteouts
are preserved; shared immutable input layers remain referenced by the Bundle's
environment provenance. Symlink targets are recorded without following them.
Special files fail export rather than blocking on FIFOs or reading devices.
Archives are bounded to 64 MiB and 100,000 entries. The client downloads the
archive as data and does not extract/apply it automatically. This representation
delivers Agent outputs and filesystem changes; complete CPU/RAM/device state
continues to use the execution-checkpoint protocol.

For a trace-only task on a trusted host Worker:

```sh
target/debug/pvisor-cluster submit crates/pvisor-cluster/examples/trace-task.json
target/debug/pvisor-cluster artifacts trace-task --out /tmp/trace-task-evidence
```

```sh
target/debug/pvisor-cluster artifacts hello
target/debug/pvisor-cluster artifacts hello --out /tmp/hello-evidence
```

The first command prints the manifest; `--out` downloads files, verifies each
chunk and the complete file, and publishes verified files without overwriting
existing destination files. The native Bundle bytes are preserved, including
executor observations and native output; the separate inline RunResult still
obeys its wire output limit.

Workers upload content-addressed BLAKE3 objects of at most 1 MiB, with a 64 MiB
limit per file. The Worker exports the native Bundle plus the explicitly
requested trace and writable-layer archive. The manifest
binds files to the exact task, lease generation, worker and incarnation. Before
accepting completion, the controller verifies every chunk, whole-file digest
and Bundle Run/Attempt identity, terminal state, timestamps and exit code
against the completion result. The worker additionally validates the complete
native Bundle schema. The controller refuses a verified manifest that omits
any requested file. Bulk verification runs outside the scheduler lock.
Objects are fsynced and published without replacement in a private directory
beside the journal, with its extension replaced by `.artifacts`; the WAL stores
the manifest reference only after verification. Back up both together.

Upload requires a live lease before and after object publication. Duplicate
uploads safely reuse identical bytes; transient failures retry while the worker
continues lease renewal. Cancellation and lease expiry interrupt retention.
Expired or superseded attempts cannot attach artifacts to tasks. Admin
credentials read manifests and objects; worker credentials upload and cannot
read other tasks' artifacts. These are trusted deployment roles, not tenant
isolation or attestation of an untrusted worker.

If native execution finishes but retention fails, the task records the native
RunResult and a separate `artifact_error`. A required archive failure makes
the aggregate cluster task Failed, even when native execution Completed; it
does not rewrite that native result or automatically repeat side effects.
Local assignment, Bundle, export manifest/reference when available, and final
completion records remain in worker storage for inspection.

Before the first upload, the Worker atomically publishes an immutable
`STATE/tasks/TASK-GENERATION/retained/` spool containing all files and their
chunk manifest. The durable outbox stores the retention request with the native
terminal result and forbids downgrading it during delivery. After an upload
interruption, restart uses this same spool; losing the original local trace
does not alter the delivered data or re-execute the Agent. Preparing and syncing
the spool runs outside async lease handling. Cancellation waits for the blocking
producer to finish before completion can release its reservations; it cannot
detach bulk copies and admit an accumulating queue of replacement producers.
Hashing/uploading reads at most
one 1 MiB chunk at a time, with a bounded retry copy. Trace snapshotting holds
the journal's append lock and exports only committed bytes; validation parses
one event at a time without retaining its payloads. A suspended VM's upper is
read from its verified, pinned execution snapshot; cold continuations archive
their own private restored layer and new Attempt trace.

After native teardown, final mount release, the terminal outbox fsync and the
joined spool producer, the Worker can send version 1 `/v1/workers/native-done`.
The controller validates the same terminal result and VM CPU/checkpoint evidence
as final completion, fsyncs that result, and changes the task to
`retaining_artifacts`. Only a receipt with the exact version, lease key and budget
lets the Worker reduce its local charge. The delivery retains the existing fenced
lease and reserves 16 MiB and 100 CPU millis, with no execution slot, in both
Worker and tenant accounting. These are conservative admission reservations for
bounded chunk buffers, retries and inline evidence, not measured usage or new
kernel limits. If the current reservation cannot cover this budget, the handoff
is refused and full accounting remains in effect. At most 64 concurrent delivery
handoffs per Worker are permitted; all deliveries still consume memory/CPU budget
and undergo final node-pressure admission before replacement work starts.

A required task remains nonterminal until all requested artifacts are verified or
an explicit export failure arrives. DAG successors therefore remain blocked even
when execution slots are reusable. Native results cannot change during later
completion, cancellation or restart. A delivery lease expiring after this durable
handoff produces a failed task (cancelled if cancellation was requested), retains
the known native result and records `artifact_error`; the expiry WAL frame
references the prior result instead of repeating all native output. It never re-executes the
Agent or permits late artifact attachment. Unknown execution still expires as
`lost`. Live CPU/memory reports and new VM control commands no longer apply to an
Attempt in delivery. Observed control results are acknowledged before handoff;
other pending controls prevent it, while matching suspend receipts can settle in
the same WAL frame.
Unsupported endpoints and invalid/lost receipts keep the Worker's original local
budget through normal upload/completion; deploy the controller before Workers,
and do not downgrade a controller whose WAL contains the new phase/change.

Scheduler tests cover WAL recovery, exact idempotency, contradictory results,
expiry, cancellation, DAG gating and the 64-delivery bound. Real HTTP tests inject
unsupported and corrupted handoff receipts and verify that queued replacement
commands cannot execute early. The KVM Gateway gate holds both completed Agent
VMs' artifact uploads, checks their native processes are gone and their delivery
charges/renewals persist, and completes a third native model/tool VM before
releasing the uploads. This establishes capacity reuse during delivery; it is not
a representative throughput, whole-node memory or Agent-density benchmark.

Legacy Bundle-only requests continue to export one file. Other artifact paths
inside the Bundle remain local references unless explicitly covered above.
Object storage is controller-local. Global byte/count quotas and explicit online
evidence retirement/reclamation are implemented below; per-tenant quotas, remote
replication and live execution recovery remain implementation gates. Restart
export/delivery of already terminated attempts uses the outbox below.

## Controller storage bounds

`serve --max-journal-bytes N --max-artifact-bytes N` configures retained WAL
and artifact payload ceilings (defaults: 1 GiB and 8 GiB). A WAL frame is bounded
to 16 MiB while serializing and reading. Restart replays one frame at a time,
then truncates only a partial final frame after successful replay. Complete
corruption and oversized frames remain intact and fail closed. HTTP groups share
one fsync while every response waits for that sync; direct mutations sync before
returning. Capacity rejection returns HTTP 507 and never silently deletes task
receipts, idempotency keys or evidence. Journal compaction remains unimplemented.

The artifact ceiling includes concurrent and failed publication reservations,
with a hard maximum of one million objects. Startup refuses retained usage above
these ceilings; retain the data and raise the byte ceiling or use explicit rooted
reclamation. The persisted byte/count policy below can impose smaller limits.
The legacy `.capacity` reservation marker is reconciled on open under exclusive
store ownership; a dirty marker fences new publications until reconciliation.
Quotas cover logical payload bytes, excluding filesystem overhead and other
programs' data.

## Resident worker delivery

Poll/renewal is independent of completion, decline and control-ack delivery.
A bounded set of at most 16 background jobs performs HTTP and durable outbox I/O;
exact lease keys and control revisions guard late replies. Pending terminal
requests remain in a separate resident queue even after their active lease has
expired. A native terminal reservation is released after acceptance/fencing, or
expiry once durable delivery ownership is confirmed. Unknown/running attempts
remain supervised; renewal never revives an expired local deadline.

A terminal backlog of 16 pauses new assignments until delivery progresses.
Shutdown may leave durable pending evidence for restart rather than wait forever
on the controller. Persistence uncertainty stops local admission. No retry starts
the native command again.

## Bound controller artifact storage

Configure unique-object byte and object-count limits at controller startup:

```sh
target/debug/pvisor-cluster serve --max-artifact-bytes 17179869184 --artifact-limits crates/pvisor-cluster/examples/artifact-limits.json
target/debug/pvisor-cluster artifact-storage
# Change limits online without restarting the controller:
target/debug/pvisor-cluster artifact-storage --limits crates/pvisor-cluster/examples/artifact-limits.json
```

Use the existing admin/Worker host credentials and normal serve arguments. The
example sets 16 GiB and 250,000 unique objects; these are deployment policy values,
not performance measurements or limits on task execution RAM. The strict version
1 JSON has nullable `max_bytes` and `max_objects`; non-null limits must be positive.
Both null disable the persisted policy limits; the configured artifact payload
ceiling and one-million-object safety bound still apply. A new unconfigured store
defaults to those safety bounds. Configuration is atomically persisted as `.limits.json` inside
the artifact root with file and directory fsync. Restart without the flag retains
the saved policy. A later explicit startup configuration or admin-only
`POST /v1/artifact-storage/limits` replaces it. The online receipt includes the exact
committed policy and usage observed under the same lock. Lowering a limit
below usage preserves existing evidence and stops new unique objects; downloads
and identical retry uploads remain available. Back up the policy with the object
root and WAL. The store is bound to a durable authority ID in the WAL and
`.authority.json`: an unrelated/replacement WAL cannot reclaim these objects,
even after the original controller exits. Matching backups can move together to
a different path. Restore the WAL and all object metadata as a pair, and upgrade
the controller before enabling GC; older controllers cannot replay the authority
and retirement changes. Controllers predating storage quotas do not enforce them.

`artifact-storage` reads admin-only `/v1/artifact-storage` and reports configured
limits, unique stored bytes/count and bytes/count reserved for in-flight
publication or failed temporary cleanup. Admission checks their combined usage **before**
creating an object temporary. Duplicate chunks/manifests share their producer
and reservation; a duplicate acknowledgement waits for successful publication
and fsync. Empty objects still consume count budget. Bulk writes and fsyncs happen
outside the inventory mutex and scheduler lock, allowing independent objects to
publish concurrently. Publication failure refunds unwritten reservations; an
object already linked is still charged even if subsequent persistence fails. An
unpublished temporary that cannot be removed retains its conservative byte/count
reservation until exclusive startup recovery removes it, preventing failed
cleanup from admitting an accumulating set of replacement uploads.

The store has an exclusive OS owner lock. Independent opens in the same process
share accounting; another controller/process cannot publish through a separate
inventory into that root. Startup under this ownership inventories published
objects and removes only recognized regular upload/config temporaries left by
interrupted publication. Symlinks, unexpected object names and malformed policies
fail closed. Startup also recovers checksummed upload-pin journals and valid
download leases. Retained evidence is reclaimed through the explicit GC below.
There is no periodic full-store scan during Worker polling or upload admission.

When either configured quota is already exhausted (including pending and failed
cleanup reservations), the scheduler leaves new artifact-required tasks queued
without a lease or execution charge. Other tasks, renewal, cancellation and
controls continue. Raising the policy online restores admission with the same
task identity. Available headroom is an admission hint, not a reservation for an
unknown-sized future Bundle/trace/upper: final object publication always enforces
actual bytes/count, and a running task can still encounter quota exhaustion.

Quota exhaustion returns HTTP 507. Workers treat it as a final export failure,
keep the original native result and sealed local Bundle/trace/upper, record
`artifact_error`, and release delivery reservations through normal completion.
It does not become an unbounded upload retry or re-execute the Agent. Other
transient 5xx/network failures still retry with lease renewal. Existing tests use
32 concurrent publishers with repeated/unique contents, abandoned/failed
publication, count-only pressure from empty objects, shrinking limits, strict
recovery and actual controller/CLI restart without repeated flags. A real Worker
test proves a quota-denied completed command keeps its Attempt and one side
effect after Worker restart. Per-tenant accounting and external storage remain
required work for sustained distributed deployments.

## Retire evidence and reclaim artifact quota online

Preview orphan reclamation without retiring any committed task evidence:

```sh
target/debug/pvisor-cluster artifact-gc
# Also propose retiring at most 256 terminal tasks finished before a UTC Unix-ms cutoff:
target/debug/pvisor-cluster artifact-gc --retire-before-ms 1791000000000
# Apply the exact immutable server-side plan ID returned by the chosen preview:
target/debug/pvisor-cluster artifact-gc --apply PLAN_ID
```

The cutoff is an explicit retention policy, not an automatic expiry. Preview
returns exact task IDs/generations/original manifest references, candidate object
references and logical bytes. It does not delete object bodies or retire evidence.
`--max-objects` bounds each plan to 1–4096 object candidates and each apply
to the same maximum number of stale upload-pin journals. A controller retains at most
16 plans for five minutes; plans are invalid after restart. Editing preview JSON
cannot alter the server plan. Admin-only `POST /v1/artifact-storage/gc/plan` and
`/apply` expose the same operation. Apply revalidates each task, fsyncs one WAL
retirement transaction, then removes objects outside the scheduler lock. Native
results, original completion references and terminal timestamps remain intact;
`artifact_retired_at_ms` records retirement separately. Exact repeated completion
and Worker recovery still receive the original outcome after bodies disappear.
New task manifest/download requests return HTTP 410 for retired evidence.

Each successful Worker upload is durably pinned to its exact lease before
acknowledgement. One checksummed append journal per lease bounds the metadata
cost; renewed delivery leases preserve earlier chunks across controller restart.
A verified completion holds all manifest/chunk pins across WAL commit, then
retains them until retirement. Shared chunks stay reachable through other retained
manifests. Legacy live assignments from an older WAL lack upload-pin negotiation;
plans conservatively report `blocked_by_legacy_leases` and select no objects until
those assignments end. New assignments use the pin protocol automatically, so
ordinary execution does not stop reclamation.

Bulk CLI/client downloads acquire a durable full-manifest download lease before
reading chunks. Download protection lasts five minutes, renews every 30 seconds
while transferring, and has a one-hour maximum lifetime; at most 128 downloads
are protected concurrently. It survives controller restart and protects an
already-started transfer even if its task is subsequently retired. Clients release
it after successful or failed transfer. Expired leases are cleaned during planning,
application or new download admission. Old controllers lacking this endpoint use
the prior download path. Separate raw manifest/chunk HTTP requests do not acquire
a full-transfer lease; use the download API for transfers concurrent with GC.

At each unlink, apply checks live pins, object publication generation and inode
identity. A candidate published/recreated or pinned after preview is skipped.
Publications and reads remain concurrent with one another; a brief object-store
barrier closes the check/unlink race. No bulk I/O or store scan holds the scheduler
mutex. Accounting refunds only durably removed objects; uncertain persistence
keeps a conservative charge until exclusive restart inventory. Partial failure
can leave tasks retired with some objects still present: inspect/retry the plan,
or generate a fresh plan after restart. Completed apply receipts are cached for
idempotent retry during the plan lifetime. Reports count that apply's deletions,
not a durable historical total across partial failures. Reclaimed headroom allows
queued artifact-required tasks to be admitted on a later poll.

This implements explicit controller-local retention and orphan reclamation.
Automated retention schedules, Worker-local spool GC, journal compaction,
per-tenant artifact accounting and remote replication remain implementation gates.

## Worker restart delivery

Each worker state directory contains a private `outbox` bound to its worker ID
and a digest of its controller URL. Keep both ID and URL stable when restarting
with the same `--state`; a different controller/identity needs a different state
directory. Credentials are supplied by the host at restart and are not saved in
these records. Upgrade the controller before deploying workers using recovery.

For required Bundle retention, the worker durably records the bounded native
terminal result **before** starting upload. It then records the final completion
request after retention succeeds or fails. Other completions enter the queue
before delivery. Writes use private temporary files, file fsync, atomic rename
and directory fsync; partial temporary writes cannot truncate an existing record.
Readers reject symlinks, special/oversized files, malformed identities and
corruption. The queue admits at most 4,096 pending records, each at most 16 MiB;
final queue write failure stops new local admission and keeps delivery pending until
durability can be established. Controller evidence can be explicitly retired as
below; Worker-local outbox/spool retention and WAL compaction remain separate work.

Before registering a new incarnation, restart drains this queue:

* A record awaiting export uploads the existing native Bundle, keeping the same
  Run/Attempt IDs, generation and bytes. Background recovery requests renew only
  the recorded keys. Cancellation or local lease expiry stops export and records
  an explicit export error; native execution state remains unchanged.
* A ready record sends the exact completion again. The client verifies the reply's
  terminal phase, full lease key, result, errors and artifact references before
  accepting it. If the controller committed completion but the reply was lost,
  the idempotent exact reply remains valid after the old lease TTL expires.
* A stale/expired/conflicting completion receives a durable `fenced` receipt.
  Local native evidence remains available; a `lost` cluster task stays `lost`.
  Transient HTTP errors retry. Authentication, unsupported protocol or malformed
  replies leave pending evidence for a corrected deployment, rather than silently
  clearing it.

The receipt is fsynced before removing its pending entry, so a crash in that
window does not require another delivery. Receipts identify the request digest
and the accepted terminal phase or fencing outcome. Startup checks pending
records rather than scanning every historical task directory, retains a small
key/digest index, and loads terminal output bodies one at a time. Existing evidence
written by older workers without an outbox is not automatically imported.

The worker-only `/v1/workers/recover` endpoint has no new-work budget. It neither
redelivers assignments, renews unseen keys, issues controls nor scans the queue.
It does not reopen expired leases. An attempt with no durable terminal result
is never adopted or re-executed during restart. After known evidence is delivered,
the new incarnation waits for any remaining unknown old leases to expire before
registration succeeds. This is completion delivery recovery, not VM checkpoint
restore or proof that processes orphaned by SIGKILL have stopped. Controller URL
binding also requires a stable service address when restarting the controller.

## Immutable VM task environments

Environments version the base image, optional workspace and ordered toolkits
independently, following DSec's composition model. The native cache provides
immutable metadata revisions and demand-filled reads; this controller does
not build another image cache or eagerly extract every layer on each attempt.

First publish each component with the existing native cache CLI:

```sh
pvisor cache --backend filesystem --location /srv/pvisor-cache publish BASE_IMAGE
pvisor cache --backend filesystem --location /srv/pvisor-cache publish WORKSPACE_IMAGE
pvisor cache --backend filesystem --location /srv/pvisor-cache publish TOOLKIT_IMAGE
```

Save each response's `image_handle` and `digest` in an environment JSON file.
Its shape is:

```json
{
  "version": 1,
  "architecture": "amd64",
  "base": {"handle": "pvisor-v1:IMAGE_KEY:linux-amd64:REVISION", "manifest_digest": "sha256:MANIFEST_HEX"},
  "workspace": {"handle": "pvisor-v1:IMAGE_KEY:linux-amd64:REVISION", "manifest_digest": "sha256:MANIFEST_HEX"},
  "toolkits": [{"handle": "pvisor-v1:IMAGE_KEY:linux-amd64:REVISION", "manifest_digest": "sha256:MANIFEST_HEX"}]
}
```

Replace the placeholders with the actual 64-character lowercase hex values.
For arm64 use `"architecture": "arm64"` and the native `linux-arm64-v8`
handle platform. Workspace may be omitted/null; toolkits may be omitted/empty
or contain at most 16 layers. Mutable OCI tags, arbitrary URLs, host paths and
platform mismatches are rejected in registered templates.

```sh
pvisor-cluster environment publish environment.json
pvisor-cluster environment show ENVIRONMENT_DIGEST
```

The publish response includes a BLAKE3 digest of the canonical template. Repeat
publication returns the same record. Updating only a toolkit creates a new
template digest while preserving base/workspace revisions. Templates persist
in the controller WAL, with a current retention bound of 10,000 records.
Publication records the immutable descriptor; it does not prove the referenced
cache revision is present or compatible on every node.

Set task `"environment": "ENVIRONMENT_DIGEST"` and execution class
`{"executor":"virtual_machine","isolation":"virtual_machine"}`. The scheduler
requires a registered template and a worker advertising its architecture and
environment protocol. Assignment/redelivery carries the same full descriptor
alongside the immutable task reference. Host and container tasks cannot silently
ignore this requirement. Cache affinity includes template layer handles as well
as explicit task cache keys; worker cache inventories remain operator hints.

Enable the VM worker's host-owned profile and native cache configuration:

```toml
[environments]
enabled = true
max_layers = 128
```

```sh
export PVISOR_CACHE_BACKEND=filesystem
export PVISOR_CACHE_LOCATION=/srv/pvisor-cache
# Or set PVISOR_CACHE_BACKEND=s3 and PVISOR_CACHE_LOCATION=s3://BUCKET/PREFIX.
# Configure storage credentials or workload roles only on the Worker host.
pvisor-worker --backend vm --id vm-1 --config worker.toml
```

The environment mount path forces read-only cache access, opens exact revisions
without HEAD/tag resolution, and checks each revision's OCI manifest digest.
The default server backend lacks these immutable revisions and is rejected.
No unversioned `lower_layers` may accompany this enabled profile. Task-selected
environments override the profile's base rootfs; tasks without an environment
continue using the ordinary profile. Upgrade the controller before deploying
workers with these new capabilities.

The root stack is toolkit(s), workspace, base from highest to lowest priority;
later toolkits win conflicts. All share read-only mounts and native block caches
across concurrent attempts, while native Run preparation gives each attempt
separate upper, work and staging storage. Mounts are held through native teardown
and released after the final owner; idle bookkeeping is removed. `max_layers`
bounds distinct live/preparing revisions on this worker, not sandbox count.
Cache failure, unavailable mounting support, manifest mismatch or mount-limit
failure stops preparation without native task execution or a host fallback.
Preparation follows live renewal and is interrupted by cancellation/lease expiry.

Task commands and explicit process environment remain in RunSpec; image
entrypoints/environment variables are not implicitly substituted. `cwd` is an
absolute guest path, defaulting to `/`. Reserved VM preparation metadata cannot
override the registered root stack. The exact descriptor is recorded under
`pvisor.orchestration.environment` in native Bundle orchestration evidence.

The opt-in `just test-cluster-vm` gate runs an independent VM worker and real
Linux VMs against a local HTTP controller. It publishes small native-cache fixtures,
runs two simultaneous template versions, checks later-toolkit precedence,
private writes and clean subsequent attempts, and compares remotely retained
native Bundle bytes with local evidence. It also checks pause/offload/resume
and guest memory continuity under a CPU-capacity conflict. This gate has passed
on one Linux amd64 host with KVM/FUSE; it establishes neither multi-host scale
nor startup/density improvement. Shared-mount coordination tests separately
prove per-key sharing, bounded admission, final-owner release and retry after
preparation failure/cancellation. Template lineage is not an execution
checkpoint or fork.

## Distributed environment reads and bounded S3 I/O

The same immutable layer descriptors work through the native S3 backend on
independent Workers. Publish with a writable host identity, then configure each
Worker with the same bucket/prefix and a read-only storage identity:

```sh
pvisor cache --backend s3 --location s3://BUCKET/PREFIX publish BASE_IMAGE
pvisor cache --backend s3 --location s3://BUCKET/PREFIX publish WORKSPACE_IMAGE
pvisor cache --backend s3 --location s3://BUCKET/PREFIX publish TOOLKIT_IMAGE
# Register the returned handles/digests through environment publish as above.
export PVISOR_CACHE_BACKEND=s3
export PVISOR_CACHE_LOCATION=s3://BUCKET/PREFIX
export PVISOR_CACHE_READ_ONLY=true
pvisor-worker --backend vm --id vm-a --config worker.toml
```

Use host-owned AWS credentials/workload roles and region configuration; an S3
compatible service can set `AWS_ENDPOINT`. HTTPS remains the normal transport.
Templates carry immutable identities, never provider credentials or URLs selected
by tasks. The environment path forces read-only operation, validates full revision
and manifest identities, and fills local verified block caches on demand. Configure
each node's `XDG_CACHE_HOME` to its node-local cache volume. Neither a registry
connection nor access to the original publisher's extracted tree is required.
Updating mutable HEAD leaves already registered template revisions unchanged.

All S3 cache clients in one process share one asynchronous I/O host thread.
Previously each mounted revision owned a dispatcher plus two Tokio workers;
a base/workspace/toolkit stack could therefore create nine S3 runtime threads.
The pool uses a single current-thread runtime and bounds queued plus executing
requests to 32 across the process. Each job retains its own ObjectStore client,
credentials, endpoint, retry policy and namespace; pooling execution does not
merge storage authorization. Failed requests release their admission charge.
The final client closes admission and joins completed operations; idle runtimes
are released. Inherited clients after fork fail closed and must be reopened.
This removes per-layer runtime thread multiplication. FUSE, Worker, resolver and
other host threads are separate costs, and the limit is not a per-tenant fairness
policy or a promise about network throughput.

The native `just test-cluster-vm` S3 gate starts two Worker processes with distinct
state directories and local caches. An owned HTTP service independently verifies
SigV4, including session tokens. After publication, the test updates toolkit HEAD,
removes the publisher directory and denies remote writes. Both Workers execute
real KVM VMs from the original pinned base/workspace/toolkit stack, report native
PIDs/RAM, and each has exactly one S3 I/O thread while three layers are active.
Private writes, original toolkit contents, guest credential absence, remote native
Bundle/trace/upper retention and the newly selected toolkit revision are checked.
Reader caches must not contain fully extracted OCI roots. A separate 64-client
concurrency test covers distinct stores/namespaces and final-owner runtime release.
These tests use independent logical nodes on one Linux host; physical multi-host
fault tests and representative startup/task-density benchmarks remain required.
S3 environment caches also need their own storage-retention policy; controller
artifact GC does not reclaim native image data.

## Capture an ordinary VM's execution state

An ordinary native VM Worker with an explicit no-network profile advertises
the `checkpoint` control action. Set these fields in its host-owned profile:

```toml
[overlaynet]
mode = "off"
policy = "deny"
```

With the task running, the admin can request a durable compressed snapshot:

```sh
target/debug/pvisor-cluster control task-1 checkpoint --request-id save-1
```

The desired/observed control history carries the exact lease key and a
`checkpointed` outcome containing the sealed snapshot id, Worker-local store,
source Run/Attempt and RAM encoding. Repeating the request id returns its
original observation. Native `RunHandle::capture_execution` also accepts raw
RAM encoding. Checkpoint capture preserves the running task's CPU, memory and
slot reservations; it does not release admission capacity or suspend the Job.

Capture freezes vCPUs and all devices, saves CPU/RAM/device state and verified
copies of every configured overlay backing directory, then rebinds saved file
handles and hard-link origins to those copies. The host publishes and fsyncs
the complete environment object and request receipt before acknowledging
commit. The runner stays frozen through host publication and resumes the source
only after that acknowledgement. Host filesystem inventory runs outside the
runner user namespace so persistent ownership can be validated later. A lost
supervisor or malformed commit terminates the frozen runner; uncertain sealing,
receipt or transition failures cancel the attempt. In particular, a directory
fsync can fail after a sealed object has been renamed into visibility: that
error cannot authorize another capture under the same request id. Rejection
during preparation drains the final reply before another native control may use
the connection.

The native producer creates fresh private copies of all backing directories,
then closes its temporary verification descriptors. While CPUs/devices remain
frozen, the host verifies the complete captured forest, syncs its files and
directories, and transfers it into the pending object with a no-replace rename.
It verifies the forest again after transfer. This preserves the captured
directory/file inode identities and eliminates the second full filesystem copy
previously performed at publication. The original logical capture root remains
in the manifest and machine bindings for later explicit relocation.

The host also seals directly from the validated native RAM FD, checking its
device/inode against the capture's regular RAM file. It skips the intermediate
pending RAM copy; final raw RAM still uses a separate sealed inode, and
compressed RAM still produces independently retained content blocks. A failure
after consuming the forest is uncertain and terminates the frozen source.
Ordinary SnapshotStore publication APIs continue copying borrowed trees and
detaching writable RAM descriptors.

Linux x86-64 compressed recapture of a restored VM additionally supports
incremental RAM capture. The runner binds every private RAM mapping to the
supervisor's immutable baseline device/inode, offsets and length, then scans
Linux pagemap flags while all CPUs/devices are frozen. Private anonymous COW
pages and swapped pages mark 64 KiB blocks as changed; CPU writes, device writes
and clearing a previously nonzero block are included. Only changed blocks are
read from the live mapping into the sparse native capture. Unchanged baseline
blocks therefore do not fault into the live VM just to capture it. Missing page
observations or unsupported mappings conservatively use full capture.

The host seals the child with its own references to verified inherited
compressed frames and newly compressed changed blocks, and computes its full
decoded RAM digest. This uses self-contained v2, or v5 with encoded private files and immutable filesystem
layers enabled: restoring or exporting the child needs no parent snapshot. The live RAM reader's persistent
frame references also permit recapture after deletion of the parent object.
Inherited frames use hard links on the same filesystem; across volumes the host
copies the encoded frame. A partial native delta is internal staging data and
is never accepted as a standalone restore payload. Original and raw snapshots
still use full capture. Publication performs complete integrity reads, and
filesystem backing is copied by default. The opt-in filesystem pool below
retains eligible lowers during recapture of initial and restored VMs. Neither path establishes
a measured latency/density improvement.

The store is private to the Job under `execution-snapshots`. CPU/RAM/device
payload and complete owned-tree inventory remain usable after transient capture
directories and the source VM are gone. A real Linux KVM/FUSE test verifies
host-side integrity/materialization, a saved open-file backing, full machine
sections, compressed RAM, continuing guest memory state and retained resource
charges. After the source finishes, it deletes original backing and detaches
the environment cache, then restores into a new Run/Attempt. The restored
guest reads an already opened file descriptor and prints an in-memory value
that a fresh command invocation cannot reconstruct.

This capture profile excludes networking, ordinary RAM compression/shared memory
pools, host `/`, special files, external hard links and external host writers.
Capture copies lower directories by default, including inputs mounted through
the lazy cache. With an explicitly configured filesystem pool, the first user
of an eligible immutable lower creates one pool copy; other VMs reuse it even
on their first capture, and later captures retain the pinned object. Private
capture-side filesystem deltas and measured snapshot/density improvements remain work. The opt-in v5 sealer already reuses unchanged private file content frames; restoration still creates private writable inodes. The controller records Worker-local references by default. The optional
repository workflow below
adds durable publication and compatible cross-Worker placement on the same host.
Cross-host runtime compatibility and recovery remain work. The ordinary
Job checkpoint CLI still rejects its unconnected execution workflows; use the
cluster submission contract below for explicit continuation.

## Publish and restore checkpoints through Workers

Configure a named host-owned repository on a Linux x86-64 VM Worker using
`--config`. This profile requires OverlayNet off, no shared memory pool and no
ordinary RAM compression. Endpoints and credentials stay on the host:

```toml
[overlaynet]
mode = "off"

[checkpoint_storage]
repository = "checkpoints"
backend = "s3" # Or "filesystem" with an absolute shared directory location.
location = "s3://BUCKET/PREFIX"
publish = true
```

A receiving Worker uses the same repository name/location and `publish = false`
(the default), with a storage role that permits reads only. Publication and
cache storage use independent host profiles; AWS credentials and endpoint
configuration follow the existing S3 cache host configuration. Registration
advertises only the repository name, read/write capability and actual host-boot,
executable, firmware and native profile identities, never a location or key.

Request publication explicitly in the source task:

```json
{
  "retain_artifacts": {
    "version": 1,
    "trace": false,
    "workspace_upper": false,
    "execution_checkpoint": {"version": 1, "repository": "checkpoints"}
  }
}
```

The task requires a matching writable repository and native suspend/export
capabilities. Submit the task, then issue `control TASK suspend --request-id ID`.
After native save-and-stop, the Worker preserves the exact terminal result in
its outbox before publishing the complete snapshot. It retains the full task
reservation while bulk publication runs and continues renewing its lease. One
bulk import/export producer per Worker bounds blocking work; preparation and
sealing are joined before local admission is released. A restart retries the
saved terminal publication without executing guest argv again. Failed export
keeps the native outcome and fails the aggregate task explicitly. A task that
finishes without save-and-stop cannot fulfill this publication requirement.

The Worker includes bounded `execution-checkpoint.json` beside the native Run
Bundle. The controller verifies its file hash, schema, named repository and exact
native suspension identity, then commits `TaskRecord.checkpoint_publication` to
the WAL. Retirement of downloadable artifacts preserves this publication record;
remote snapshot retention is independent and still needs an operator policy.

Submit an explicit continuation using the existing `restore.task_id` and
`restore.request_id` contract, a new Run/Attempt identity and the original command,
inputs, resources and environment. Labels may select a different Worker. Clear
the source's `execution_checkpoint` retention requirement if the continuation
should finish normally on a reader. Placement requires exact repository and
host-boot/build/firmware/profile compatibility. The original Worker can still use
its local checkpoint; an independently owned Worker imports and verifies the
snapshot in `state/checkpoint-imports`, seals original source provenance, and
starts the VM from that local copy without fetching the original environment.
Tasks cannot choose storage URLs or override publication provenance.

The real KVM/FUSE gate `worker_checkpoint_publication_recovers_after_crash_and_restores_on_an_independent_worker`
first checkpoints a freshly started VM, removes that checkpoint object and
runs GC while it continues on its original environment lowers. Its next native
capture retains the same lower inodes without private lower copies. The gate
then interrupts a signed S3 PUT after commit, holds publication beyond a lease while
checking renewal/reservations, kills and restarts the source Worker, and verifies
unchanged native result/Attempt/generation. A second, freshly started VM on that
Worker captures without restore/owner hints and retains the same lower inodes.
Filesystem events confirm no temporary pool tree is created on this hit; the
private forest also contains no lower copies. It then deletes the source snapshot,
source environment, caches and independent filesystem pool, retires downloadable
artifacts and restarts the
controller. A second Worker with a read-only repository and unavailable environment
cache continues the RAM variable and already-open file descriptor into a new
native Run/Attempt. It deletes the live VM's imported parent object, recaptures
with incremental RAM and retained immutable lowers, checks unchanged lower
inodes and absence of private lower copies, then removes the complete imported
store after that VM exits. A further native Run/Attempt restores the self-contained child and again
continues its RAM value and open writable/read-only descriptors, including a
lower payload not opened before capture. SDK tests additionally cover dirty
block clearing, partial tails, inherited-frame ownership on either volume,
child transport after deletion of both stores, and corrupt/unbound inventories.
Both Workers share one physical host and boot; this does not
establish physical cross-host recovery or a density/latency improvement.

## Distribute sealed execution snapshots

Linux `environment_snapshot::SnapshotRepository` publishes a full sealed
checkpoint to filesystem or S3 storage and imports it into an independently
owned `SnapshotStore`. This is the storage prerequisite for remote recovery.
Endpoints, credentials and write/read roles come from the host configuration;
`SnapshotTransfer` contains only its version, original snapshot hash and
transfer-manifest hash. Tasks cannot use a receipt to choose an endpoint.

```rust
let source = SnapshotStore::new(&source_store)?;
let published = source.open(&snapshot_id, &source_compatibility)?;
let writer = SnapshotRepository::s3("s3://BUCKET/PREFIX", false)?;
let receipt = writer.publish(&published)?;
// Persist this trusted receipt before allowing the source to be removed.
drop(published);
let reader = SnapshotRepository::s3("s3://BUCKET/PREFIX", true)?;
let destination = SnapshotStore::new(&destination_store)?;
reader.import(&destination, &receipt, &destination_compatibility)?;
```

The destination compatibility must come from the receiving runtime: host boot,
Worker executable hash, firmware hash and profile must match. Import preserves
the exact original manifest bytes, machine state, logical filesystem roots,
Run/Attempt identity and snapshot id. It does not rewrite a seal to make an
incompatible host acceptable. The Worker import workflow below binds original
Run/Attempt ownership to a durable publication receipt and a private local import.
Exact host-boot compatibility still prevents physical cross-host migration.

Immutable `pvisor-checkpoints-v1/{chunks,manifests,transfers}/` keys use SHA256
identities. Payloads stream in 1 MiB chunks, deduplicate equal contents and omit
all-zero chunks. Conditional creation compares existing bytes before reusing a
key. The transfer manifest is committed last; interrupted uploads cannot yield
a successful receipt. The original snapshot reference remains locked throughout
export. The current conservative Job-store gate also fences other publication
and deletion while that reference is live; schedule bulk export after save-and-stop
or task completion. S3 operations reuse the bounded process-wide native cache I/O runtime,
while storage authorization stays in the supplied ObjectStore client.

Import checks the receipt, metadata hashes, exact compatibility and complete
inventory before bulk reads. It reconstructs only its private pending tree,
rejects traversal, symlink parents, duplicate paths and external hardlink origins,
preserves byte filenames, hardlinks, symlinks, ownership, modes, nanosecond
mtime, xattrs and Linux POSIX ACLs, and writes zero RAM/file chunks as sparse
holes. Every chunk and complete payload is verified; raw RAM block indexes are
checked against actual bytes. Compressed frames also verify decoded identities
and join the existing local content pool through durable hardlink references.
Full RAM, machine and tree seals are checked before fsynced atomic publication.
Repeated import validates the existing object rather than replacing it. Active
readers fence publication; a caller may retry after they release their references.

The format bounds each metadata object to 16 MiB, the tree to 65,536 entries,
encoded payload to 64 GiB and 65,536 chunks, decoded RAM to 64 GiB, and machine
state to 128 MiB. Storage reads enforce the per-object bound before receiving
payloads. Partial local imports are unpublished and cleaned; unreferenced RAM
frames can be collected by the existing local collector. Remote orphan chunks
need a separate retention policy. This path transfers self-contained full
inventories, including children produced by incremental RAM capture, and eagerly
verifies import. Incremental filesystem capture and remote page-fault loading
remain work.

Six signed-S3 tests cover raw/compressed round trips after deleting every source
copy, sparse data and full ACL/xattr/hardlink inventories, read-only import,
idempotency, corrupted existing content, missing/truncated/corrupt chunks,
unsafe metadata and geometry, false raw indexes, exact compatibility, interrupted
publication and retained-reader fencing. A filesystem-repository test also checks
chunk reuse, sharing of imported RAM frame inodes across snapshots, and collection
only after the final snapshot reference disappears. The real KVM/FUSE control-plane gate
exports a sealed native checkpoint, physically deletes its original object,
imports both an independent replica and the original Worker-owned store through
a service that denies writes, then continues a new Run/Attempt from saved CPU,
RAM and an open descriptor. The rebuilt backing uses new inodes. That gate
retains the original Worker assignment and host boot, so it proves storage
recovery while cross-host runtime compatibility and placement remain unverified.

## Reuse read-only filesystem layers during restore

Linux continuation on the same filesystem volume now shares verified lower
copies between Attempts. For v1/v2/v3 snapshots, the first restore copies a
sealed lower into `<pool>/filesystems/<id>/tree`; later restores authenticate
the complete source snapshot and shared inventory again, then reuse that tree.
The pool defaults to `execution-snapshots` unless explicitly configured. V4
snapshots already retain their lower pool objects, so restore pins those trees.
The identity binds both the complete filesystem inventory and the captured
logical root, so equal contents at distinct lower positions retain distinct
host inode identities. Data files preserve their original hard-link topology.

Upper, work, preimage journal, apply target and baseline backing remain private.
A lower that also serves any of these roles is copied privately. The native
rebind rejects shared layers with saved writable handles; the runner's Linux
Landlock rules permit only reads of lowers. Restored overlays reject apply to
their protected target. Guest copy-up and writes use the new Attempt's upper,
including future copy-up of previously unseen hard-link aliases.

Each retained Attempt owns a hard link to the object's reference marker under
`execution-restore/filesystem-references`. Only markers are linked between
Attempts. These disk references survive Worker restart and native VM exit so
retained Run records continue to resolve their backing. Collection removes an
object only after all Attempt references disappear and no active owner holds
its marker lock. Source snapshot deletion leaves existing branches available
and still prohibits any new restore from that deleted snapshot.

Restore sharing removes repeated lower copies between Attempts. The opt-in
native recapture path below also retains these lowers during capture; full
integrity reads remain required. Different filesystem volumes
and macOS use private copies. The snapshot store and its filesystem objects
must be retained while any retained Attempt references them; this is local
storage. The full source checkpoint can be transferred through the host-owned
repository below; shared layer references are rebuilt locally. Cross-host
placement and recovery remain work.

## Retain immutable lowers during native recapture

Linux x86-64 no-network VM Workers can opt into v5 snapshots containing private
file block references plus independently retained immutable lower trees:

```toml
[vm]
snapshot_filesystem_pool = "/srv/pvisor/snapshot-filesystems"
```

Use an absolute host-owned path on the same volume as the Worker task stores,
outside every VM-writable root and Job snapshot store. The Worker validates and
canonicalizes this setting before recovering publications or importing snapshots.
Tasks and transport metadata cannot choose this path. The profile excludes shared
memory pools, ordinary RAM compression and explicit writable RAM backing.

Native capture keeps eligible read-only lowers on their original roots while
copying private backing. During the same full-machine freeze, the supervisor
validates each complete original tree and resolves its inventory/slot identity
in the independent pool. A miss creates one pool copy; a hit retains the existing
tree without a temporary data copy, including the first capture of a different
VM. The saved read-only filesystem state is rebound to that verified pool tree.
The sealed checkpoint and live control connection both retain their own owners.
Restore also pins pool objects independently of the parent snapshot. Later
captures retain these objects and exclude their payloads from the private forest. A bounded request carries only the sealed
id and launch slot; source paths come from the host's trusted launch binding.
All 128 references fit the control frame without full paths or inventories.

An initial VM keeps running on its original environment-cache lower mounts.
During recapture the supervisor verifies each complete original lower against
its retained seal, including data never opened by the guest, then rebinds only
that saved read-only layer to the pool tree while CPUs/devices remain frozen.
The runner gains no broader pool access. Captured upper/work/journal identities
remain unchanged; altered unseen source data or a source/destination overlapping
any private role is refused. A restored VM already uses the pinned pool tree.
Owner locks survive capture cancellation and deletion/GC of the parent snapshot.
The handoff reuses the sealer's existing owners without reopening/decoding the
complete RAM payload to obtain references. Saved read-only
handles retain their backing; upper, work, preimage/journal, baseline and apply
roles remain private, including lower paths also used by those roles. Raw and
compressed RAM snapshots support these references; incremental RAM capture still
requires restored compressed RAM.

The default remains v2/v3 capture. Pool misses are serialized per immutable id,
so concurrent first captures of the same lower build one data tree; unrelated
ids and hits can proceed concurrently. Every creator/waiter holds the pool gate
before opening its key lock, allowing GC to remove idle lock files under its
exclusive gate without splitting a key between old and new lock inodes. Shared
objects keep their metadata unchanged. Per-layer seals are bounded before any
new tree is created, so an oversized manifest cannot poison the pool.

Private v5 file payloads use the existing verified compression codec in 64 KiB
frames. The complete private inventory retains metadata, ACLs/xattrs, symlinks
and closed hard-link groups; zero frames need no content object. A snapshot
retains every encoded frame it needs in its own `filesystem-blocks` directory.
Unchanged content reuses existing pool frame inodes, including across Jobs,
while a changed frame creates a new object. Encoded frame references never
become guest file inodes. Restoration reconstructs independent writable files,
restores internal hard links and checks complete file digests and metadata.
The sealed object contains no complete private `rootfs` tree.

Compressed RAM and private-file sealing compute the existing decoded-content identity
before invoking compression. Pool hits decode the stored frame, verify its
identity/length and compare every byte with the supplied source bytes, then
retain an independent reference to the existing inode. Only misses invoke the
encoder. Existing raw/fill/zstd encodings remain intact; corruption, symlink
objects and wrong reference targets fail closed. Content opens are nonblocking,
so a substituted FIFO is rejected without waiting for a writer.

Native host diagnostics include `stage=checkpoint.filesystem` work counters:
`encoded_frames`, `reused_frames`, `zero_frames` and `payload_bytes`. The last
counts the encoding stream, excluding additional integrity inventory/verification
reads. `PVISOR_STARTUP_TIMING=0` suppresses these diagnostics with ordinary timing
records. A real native recapture reads the same 5,268,224 payload bytes while
encoder calls fall from 102 to zero after parent deletion/GC; all 102 frames keep
their original inodes. The first capture of another fresh VM in the same pool
also reuses all 102 frames without encoding; a gate compares its misses against
the new content identities absent from the earlier capture. A changed-block unit gate encodes one frame and inherits
three, including hard-link aliases and zero/empty files. These work counts do
not establish task latency, CPU savings or sandbox density; full source reads
and integrity checks still run.

The live control connection pins private frames before acknowledging capture;
restore pins them in the new Attempt's own store before releasing the parent.
These owner references survive deletion/GC of the parent object and its whole
Job store. Capture cancellation keeps the previous owner through sealing, and
successful handoff replaces it with the new owner. FS and signed S3 transfers
stream logical private bytes directly from verified frames and rebuild local
references on import, preserving snapshot identity without publisher storage.
Suspended workspace-upper artifacts also stream directly from verified frames;
the Worker and outbox recovery use the host-configured pool and create no
temporary complete private tree. A real crash/restart gate downloads the original
private workspace file before deleting source storage.
The SDK's Linux `PendingEnvironment::publish_chunked_filesystem` selects this
format for borrowed private trees; `publish_layered` remains the v4 API.

Private payloads are bounded to 65,536 entries, 1,048,576 frames (64 GiB logical
primary-file bytes) and a 16 MiB sealed manifest. Packed file paths follow
canonical inventory order; readers use binary lookup. Invalid versions,
geometry, missing/corrupt frames and unexpected private tree data are refused.
Tests verify one changed 64 KiB frame adds one content object, unchanged frame
inodes survive parent-store deletion, restored writable copies remain isolated,
and sparse/empty files, native names, read-only modes, ACLs/xattrs and hard-link
topology survive local and signed S3 transfer.

Opt-in Linux native capture now records version-2 machine bindings from frozen
private roots to logical `layer-NNN` directories. The supervisor authenticates
these against private launch slots and verifies original saved inode identities,
contents and overlay hard-link origins before sealing. It encodes private files
directly from this backing; the capture forest contains only an empty root,
without an intermediate private data tree. Source modes, xattrs, ACLs and link
counts remain unchanged. Import, restore and suspended artifact export use only
the authenticated logical directories and never open the recorded original
private paths. The v5 storage format, default native version-1 captures and
borrowed SDK publication remain compatible.

Tests check an empty capture forest, forged/missing/duplicate launch bindings,
changed original content/inodes, saved writable handles, complete metadata and
two isolated restores after original backing deletion. The real independent
Worker recovery gate checks that version-2 machine state retains the source's
original upper path, then removes all source Worker storage before restoring
and cold-restoring a derived child. Sealing/export still reads complete
inventories and payloads. Capture-side private filesystem deltas and measured
latency/density improvements remain work.

`SnapshotStore::with_filesystem_pool(job_store, pool_store)` selects a trusted
host-owned pool outside the Job's private store. Both roots must be on the same
volume, and independent roots cannot contain one another. Wire metadata cannot
select the pool. Reopening/exporting a pooled Job store requires the same pool
configuration. Backup must include both stores or use `SnapshotRepository` to
export the complete checkpoint.

After capturing RAM/state/private files in one full-machine freeze, a caller
can use `PendingEnvironment::publish_layered` with `SnapshotLayer` values
retaining previously verified `SharedFilesystemLayer` owners. The private
forest must exclude the retained lower paths. Each layer has one canonical
relative directory name, a logical source binding, a complete inventory and
its authenticated pool id. Source bindings describe relocation; readers never
open them to find payloads. Duplicate paths/bindings/ids and private overlap are
rejected. The SDK treats machine state as opaque; a native coordinator must
also validate lower roles and saved handles before using this API.

The child owns hard links to reference markers under
`objects/<snapshot-id>/filesystem-references`; it owns no data-file links to
other snapshots. The original lower inode identities and internal hardlink
topology stay intact, while equal trees in distinct lower positions retain
distinct identities. Parent objects and the entire parent Job store may be
deleted without dropping the child's data. The configured pool is part of its
checkpoint storage. Retained Attempt markers and active owners also fence GC.
Live owners remain small; complete lower metadata is loaded when sealing rather
than retained for every running VM. Collection preserves read-only directory
metadata while any reference or active owner remains; it makes directories
traversable/writable only after establishing exclusive ownership for deletion.

In v4, `manifest.filesystem` describes private files and `filesystem_layers`
describes complete immutable trees. `PublishedEnvironment::complete_inventory`
returns their logical union, and `materialize` makes independently writable
copies of both. FS/S3 export streams every payload and verifies all inventories;
import validates each component and the complete union before bulk reads,
rebuilds local pool objects and snapshot-owned marker references, then commits
the original checkpoint id. A new imported layer is adopted from staging by
rename, preserving its inodes without a second data copy. v1/v2/v3 metadata and
transport remain compatible. Layered manifests are bounded to 16 MiB, 128
lowers and 65,536 total entries across private files and all lower inventories.

`environment_snapshot_layers` tests verify eight concurrent Jobs reusing lower
inodes without data aliases, distinct equal-content positions, parent-store
deletion, last-reference GC, raw/compressed FS and signed-S3 round trips after
all source data is removed, directory traversal order and hardlinks, read-only
import, read-only directory metadata through adoption/materialization and final
GC, read-only file hardlinks with user xattrs and POSIX ACLs, failed publication
cleanup, malformed inventories and oversized unions
rejected after the two metadata reads, and missing/forged/symlinked marker
rejection. A native publication unit test covers raw/compressed capture adoption,
retained inode identities after complete parent-store deletion, and independent
transport after all source storage is removed. `vm_snapshot_fs` additionally
verifies shared-root recapture, saved handles/cookies, private hardlink copy-up
and rejection of a forged retained inode identity. Owner-handoff tests verify
GC protection after complete parent-store deletion and rejection of modified
unvisited source data. Eight concurrent first-seal requests share one pool tree;
Linux filesystem events verify only one temporary tree on the miss and none on
a later hit after GC reaps idle key locks. Equal contents at distinct slots
still retain different objects, and read-only source metadata stays intact. Partial lower-rebinding tests preserve private inode
identities, saved handles/cookies and reject private sources/destinations. The real KVM/FUSE gate above
covers Worker transport, native recapture and continuation. Private filesystem
deltas and representative VM density measurements remain work.

## Suspend a VM and release its execution capacity

Eligible no-network native Workers also advertise `suspend`. Upgrade the
controller before these Workers: older controllers do not recognize the new
control action, task phases or native terminal state.

```sh
target/debug/pvisor-cluster control task-1 suspend --request-id save-stop-1
target/debug/pvisor-cluster show task-1
```

The Worker uses the same full-device freeze, owned filesystem capture,
verified publication and durable request receipt as checkpoint capture. After
commit, the native runner exits **inside the frozen transaction**, without
thawing devices or executing another guest instruction. Suspend capture or
publication failure keeps the source frozen and terminates it; it never
authorizes a successful suspend observation. Continuing `checkpoint` retains
its separate capture-and-resume behavior and still requires a running source.

A successful `checkpointed` control observation proves sealing. The task then
enters `suspending` and retains all current reservations until the parent reaps
the VM and finishes native teardown. Only its durable completion, containing
native `RunState::Hibernated` and a matching suspension receipt, transitions
the task to terminal `suspended` and releases CPU, memory and slot charges.
This differs from live pause's `RunState::Suspended`, which retains a VM and
its memory/slot budget. Files remain staged; hibernation does not auto-apply or
discard a partially completed workspace.

The terminal native result carries the request ID and sealed checkpoint so
outbox restart delivery retains both facts even if native exit races the
separate control acknowledgement. The controller validates the exact issued
suspend command, lease, source Run/Attempt, RAM encoding and existing observation.
When the separate acknowledgement is missing, it commits the checkpoint
observation and terminal completion in one WAL transaction. A pending or
foreign command, changed receipt, cancellation or expired lease cannot turn
an unknown execution into a successful suspend. Cancellation still wins the
aggregate task phase; a stale native completion remains fenced after expiry.

Wait for terminal `suspended` before submitting a continuation using
`restore: {"task_id": "task-1", "request_id": "save-stop-1"}` and the new
Run/parent IDs described below. Resume of a suspended disk checkpoint uses a
new task/Run/Attempt and full admission, including after a Worker process
restart with the same ID and state directory. `control ... resume` remains
the operation for a live paused/offloaded VM.

Workers separately advertise optional `parked_execution_suspend_protocol: 1`
for save-and-stop of a paused/offloaded source. The controller rejects parked
suspension before dispatch when this capability is absent, including old
Workers. Current advertisement is restricted to Linux x86_64 VMs with network
off, regular RAM, and no memory pool or RAM compression. Parked CPUs remain
stopped while device workers finish outstanding host I/O and enter the full
snapshot freeze. The parked source retains its zero CPU reservation throughout
capture and sealing; a competitor can occupy all CPU capacity. Memory and slot
charges remain held until native exit and cleanup. The continuation queues until
its full CPU budget is available.

The real KVM/FUSE gate exercises both pause and offload under a three-second
lease and one Tokio Worker thread. A host continuation signal is published only
after parking; neither successful suspension nor rejected publication may
execute that signal on the source. A new Attempt must recover the shell's RAM
variable and open file descriptor, and execute the continuation in its own upper.
Host boot, build and firmware bindings still apply, so this is not recovery
after host reboot or cross-host migration.

## Continue a sealed VM checkpoint in a new task

Wait for `show task-1` to report the `save-1` checkpoint control as `succeeded`.
Copy the original submitted task specification, choose new task and Run IDs,
set the original Run as parent, and reference that observed control:

```sh
jq '.id = "task-1-restored"
    | .run.run_id = "run-1-restored"
    | .run.parent_run_id = "run-1"
    | .restore = {task_id: "task-1", request_id: "save-1"}' \
  task-1.json > task-1-restored.json
target/debug/pvisor-cluster submit task-1-restored.json
target/debug/pvisor-cluster show task-1-restored
```

Here `run-1` is the original specification's `run.run_id`, which may differ
from its task ID. Keep command, environment, input, policies, timeout, execution
class, environment digest and resource budgets unchanged. This is continuation
of the saved CPU/process state, so replacing the original command is rejected.
The controller resolves the snapshot from its durable control history; callers
do not supply a host path. It checks tenant and parent identity, then admits
the full CPU/memory/slot budget on the owning Worker by default. A matching
publication of the selected save-and-stop checkpoint also permits a different
Worker with the same repository and exact boot/build/firmware/profile identities.
An older Worker without restore capability cannot receive the assignment.
The original task may still be running; a second execution needs its own full
reservation and explicit submission.

The Worker verifies the source Run/Attempt storage binding, host boot, build,
firmware, machine contract and complete filesystem inventory. It restores owned
files into independent Attempt storage and uses read-only snapshot RAM with
private COW guest writes and on-demand reads. Environment cache mounts and
original source directories are unnecessary. The Run record and Bundle carry
snapshot lineage and the resolved checkpoint reference; filesystem authorization
retains its rules while audit/approval endpoints bind to the new Attempt.
Guest process memory and environment retain their captured values, including
source identity variables. Host AgentCtl Unix-socket credentials are removed
from VM guest environments.

Concurrent restores of the same snapshot on one Worker share a single
authenticated read-only RAM inode and its kernel page cache. Each native VM
maps it privately: guest/device writes allocate that VM's own COW pages.
The key includes the canonical snapshot store and content ID; different
snapshots/stores do not share this cache entry. Per-key singleflight avoids
duplicate mount creation, while unrelated preparations proceed independently.
The bounded process-local pool permits at most 4,096 live snapshot mounts or
preparations and keeps weak references only. Mounts live in the source store's
`ram-mounts` directory, independent of the first restored Attempt, and the
last owner releases the FUSE mount, watchdog and RAM content pins after native
termination. New restores always validate the published reference first;
deleting a snapshot prevents new restores while existing mappings retain their
backing. This sharing does not change logical CPU/memory/slot reservations or
authorize physical overcommit, and does not share writable filesystem layers.

New bundled-kernel snapshots also record the kernel's guest address and size.
A restore-only native context derives the original architecture RAM layout
from that geometry and checks it against every captured mapping. It skips
loading libkrunfw (whose constructor expands another private kernel image),
or copying the embedded kernel in static Linux builds. Kernel bytes come from
the snapshot's private COW RAM mapping. Firmware/build identity checks in the
supervisor remain required; geometry cannot authorize a different firmware or
RAM topology. Recapture retains the geometry. Missing geometry uses the original
firmware path if all existing compatibility checks permit the snapshot.

Restored tasks retain normal fencing, cancellation, lease renewal and admission.
Pause, resume and further full capture use native controls. Offload is rejected
for the snapshot COW pager because the ordinary writable-backing reclamation
path cannot handle it. Restore requires a native no-network profile without
shared memory pools or RAM compression; snapshot RAM itself may use compressed
storage. Source loss never implicitly replays work or releases the new task's
resource charge. There is no cross-host migration or automatic retry.

## Fork a sealed execution checkpoint into independent branches

Create a full checkpoint with `control ... checkpoint` and wait for its durable
success, or suspend the source and wait for terminal `suspended`. Specify that
exact checkpoint control and the new Task/Run identities in a fork request:

```json
{
  "version": 1,
  "request_id": "search-round-1",
  "checkpoint_request_id": "save-1",
  "branches": [
    {"task_id": "task-left", "run_id": "run-left"},
    {"task_id": "task-right", "run_id": "run-right"}
  ]
}
```

```sh
target/debug/pvisor-cluster fork task-1 fork.json
target/debug/pvisor-cluster show-fork task-1 search-round-1
target/debug/pvisor-cluster show task-left
target/debug/pvisor-cluster show task-right
```

`POST /v1/tasks/{id}/forks` atomically commits all queued branches and their
immutable creation receipt in one fsynced WAL frame. Retry the same request
after a lost response; it returns the original receipt without creating,
restarting or reviving any branch. Changed content under the same request ID is
a conflict. `GET /v1/tasks/{id}/forks/{request_id}` retrieves that receipt;
inspect each task separately for its current execution state. Both endpoints
require the admin credential. Upgrade the controller before using this API;
old controllers cannot replay the new fork WAL record.

Requests contain identities only. The controller preserves the source tenant,
command, input, environment, policies, resource budgets, labels, cache keys and
Bundle-retention requirement, adds explicit parent/restore lineage, and binds
the receipt to the source lease and sealed checkpoint. All Task/Run identities
must be fresh and distinct. A run-identity index avoids scanning historical
tasks. One fork accepts 1–64 branches, at most 4 MiB of serialized transaction
content, and must fit the task-retention limit. Validation failure creates no
branch and no receipt. A partial final WAL frame exposes neither the first
branch alone nor a success receipt.

Creation does not reserve resources or imply that all branches can run at once.
Each branch independently reacquires full CPU/memory/slot and tenant admission,
on the snapshot's owning Worker or an eligible repository-backed Worker with
restore support. Inherited artifact retention requirements still apply. It inherits the ordinary
restore binding checks and private files/COW writes; branches of the same
snapshot reuse the existing authenticated RAM mount. Cancellation and fencing
apply independently, and retrying fork creation preserves cancelled or completed
branches. A creation receipt does not assert later successful execution.

The real shared-RAM KVM/FUSE gate creates both branches through this API, then
checks their saved shell memory/open descriptor, independent diverging writes,
common read-only RAM inode, native controls and last-reader cleanup. This is
explicit branching from an already sealed point. Use the following live workflow
when the source is still running and a fresh capture is required. Incremental
checkpoints and cross-host branching remain work.

## Capture a running VM and create branches durably

Use the same identity-only JSON request with a fresh `checkpoint_request_id`:

```sh
target/debug/pvisor-cluster fork-live task-1 fork.json
target/debug/pvisor-cluster show-live-fork task-1 search-round-1
```

`POST /v1/tasks/{id}/live-forks` persists the capture control, every branch
identity and a `capturing` plan in one fsynced WAL frame. The source must be
`running`, have a live lease, and advertise native checkpoint and restore
support. Paused/offloaded sources use the existing explicit suspend workflow.
An existing checkpoint request ID is rejected; this API captures a fresh point.
The sealed and live APIs share a request-ID namespace and reject conflicting use.
Both live endpoints require the admin credential.

Children begin in `waiting_checkpoint`, without a lease or CPU/memory/slot
reservation. Their identities occupy task retention immediately. The matching
full checkpoint acknowledgement commits the snapshot observation, immutable fork
receipt and all still-waiting children becoming `queued` in the same WAL frame.
The plan becomes `ready`; inspect each child for admission and execution status.
The source continues normally after capture and keeps its full charge until an
ordinary native lifecycle transition releases it. Each child reacquires its own
full budget on the owning Worker. Source termination after `ready` does not
cancel these independent children, and acknowledgement never revives a cancelled
waiting child.

Capture failure, source cancellation, completion before acknowledgement or lease
expiry makes the plan `failed` and fails every still-waiting child in the same
source-state transaction. An incomplete or locally published object without its
matching control acknowledgement cannot release children. Late or mismatched
receipts are fenced. Source cancellation retains its resource reservation until
native completion, as usual. A failed plan is final; create new request and
branch identities for an explicit new attempt.

`GET /v1/tasks/{id}/live-forks/{request_id}` returns current durable capture
progress and the receipt when ready. Retry the identical POST after a lost
response or controller restart. It returns the current original plan and never
restarts capture or children. Partial final request/acknowledgement WAL frames
cannot expose partial children or admit an unconfirmed capture. Limits remain
1–64 branches, 4 MiB per transaction and the task-retention limit. The pending
source index bounds acknowledgement/failure work to that source's branch batch.
Upgrade the controller before using this API: older versions cannot replay the
new WAL variant or deserialize `waiting_checkpoint` tasks.

The real live-fork KVM/FUSE gate starts a running source, captures through this
API, and restores two children while the source remains running. It verifies
full charges for all three VMs, retained shell memory/open descriptors, a shared
RAM inode, separate owned files, native pause/resume and independent
source/left/right continuations. The Worker uses one
Tokio runtime thread and a three-second lease. Protocol fault tests separately
exercise replay, torn request/acknowledgement frames, stale receipts, capture
failure, early completion, cancellation and lease expiry.

## Cooperative inference waits

An opt-in VM Worker profile can release admission CPU while a cooperatively
idle Agent waits for a model. Enable `release_cpu_on_idle = true` under
`[gateway]`, alongside `enabled = true`; the agent declares a quiescent call
using `x-pvisor-inference-idle: true`. This declares whole-guest idleness,
including tools and background work. Without both settings, forwarding is
unchanged. The local header is stripped before model-supplier forwarding.

The Attempt-bound Gateway lifecycle groups at most 64 simultaneous cooperative
calls. It requests one native pause before upstream dispatch; the first ready
response requests resume for the group. Buffered replies wait for body EOF and
SSE waits for its first nonempty chunk, rather than early HTTP headers. CPU is
released only after native pause acknowledgement and reacquired through the
normal controller and final Worker admission checks before native resume.
The Gateway delivery barrier then permits the response. Later ready calls in
the same group retain CPU rather than pausing a guest already processing a reply.

`POST /v1/workers/inference-wait` takes a Worker-authenticated
`InferenceWaitRequest`: its key binds the full lease, a monotonic wait-group
revision and the first authorized Gateway call ID; intents are `begin`,
`ready` and `observe`. The controller keeps one current wait and four bounded
automatic control receipts per task. Automatic controls never consume the
4,096-entry manual history, while their command revisions share the same
monotonic order as manual controls. The Worker likewise retains four automatic
control evidence files. The finite WAL quota still applies; this does not
implement WAL compaction. Admin-only `GET /v1/tasks/{id}/inference-wait` returns
the current record for inspection, without authorizing delivery.

Cancellation before an uncertain Begin creates a durable Ready tombstone;
a late identical Begin cannot pause the VM. An unissued pause is aborted.
If pause was already issued, its acknowledgement and the requested resume
are committed together. Controller restart retains these intentions, but
default lease reconciliation requires a fresh owning-Worker report before a
wait can authorize progress. Stale lease/call/revision requests cannot revive
execution. Cancellation and native termination abort pending wait controls.
Dropping the last pending group member schedules one cleanup, bounded by
the execution lifetime and lease. Cleanup stops before retained-artifact
delivery can keep the lease alive.

A human pause/offload/resume or checkpoint/suspend request revokes automatic
pause ownership. The wait never overrides it; delivery remains held until an
authorized running state with full admission is confirmed. An already issued
transition must settle before a conflicting manual command, as with ordinary
VM controls. The `pvisor-inference-` request-ID namespace is reserved.

Protocol tests cover CPU competition, cancelled/uncertain publication,
restart, manual override, stale identities and 2,100 waits (4,200 automatic
controls) without manual-history exhaustion. The Linux KVM/FUSE
`cooperative_model_wait_releases_cpu_and_preserves_manual_pause_before_delivery`
gate runs actual Agent/model/tool loops, observes unchanged frozen vCPU
counters, admits a third CPU-consuming VM using released budgets, keeps a
manual pause intact and verifies successful tool results after readmission.
It uses deterministic model replies and measures ordering and admission,
not useful-work throughput or Agent density. RAM and slots remain reserved.
The companion `cooperative_model_wait_survives_controller_restart_with_same_native_execution`
gate stops the HTTP server, releases its journal authority and reopens the WAL
while two actual guests await response delivery and a third holds the CPU budget.
It checks fresh Worker reconciliation, unchanged lease identities and native
PIDs, preserved manual pause and successful model/tool results after readmission.
The `cooperative_model_wait_survives_controller_sigkill_with_same_native_execution`
gate runs the real `pvisor-cluster` CLI as a separate process. Its test proxy
receives a successfully committed Ready receipt, holds it away from the Worker,
then discards it as HTTP 503 after the controller receives SIGKILL and restarts.
Replay preserves pending resume and manual pause ownership; the live Worker
retries, confirms its original leases and native PIDs, and finishes the actual
model/tool work after CPU readmission. Artifact gating stays outside the killed
controller, and the test verifies retained journals, private workspaces and
binary output through the restarted process. The recipe builds the CLI first;
each fixture copies both controller and Worker binaries before launch.
These outages stay within the existing three-second Worker watchdog. Longer
outages, parallel-call fault experiments, networked hibernation, persistent
rollout state and multi-host density remain open gates.
Upgrade the controller before enabling this profile; older controllers do not
understand the new endpoint or WAL variant.

## Worker profiles and scheduling

`--config` accepts a narrow TOML **worker profile**, not the full CLI RunConfig.
Unknown fields fail parsing, so unconnected controls cannot silently disappear.
Task invocation, environment, timeouts and policies come from RunSpec. Stdin
is closed and stdout/stderr are captured with the RunSpec limit (maximum 1 MiB
per stream). Local output is additionally bounded after lossy UTF-8 decoding
and to 1 MiB of JSON-encoded content per stream, leaving space under the 4 MiB
completion request limit. Control bytes can require six encoded bytes each;
additional clipping marks the corresponding `*_truncated` flag. A retained
native Bundle keeps the native capture before inline delivery clipping.

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
limit. Fresh attempts get separate RAM backings; restored attempts use private
COW mappings of the shared read-only snapshot baseline. VM rootfs apply is protected.
Native cache publication/access and rootfs provisioning belong to deployment;
registered immutable environments select revisions without copying arbitrary
host paths across nodes. Workers without this profile use configured rootfs/layers.
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
comes from its configured capacity minus local reservations, optionally reduced
by measured node pressure and limits.

## Node pressure and final admission

The portable default is `mode = "reservations"`. Linux deployments can enable
read-only pressure admission in the worker profile:

```toml
[admission]
mode = "linux_pressure"
memory_reserve_bytes = 268435456
cpu_some_avg10_limit_bps = 5000
memory_full_avg10_limit_bps = 100
max_sample_age_ms = 3000
```

One basis point is 0.01 percent of stalled wall time. The CPU threshold above
is 50%; the memory threshold is 1%. These are configurable starting values,
not measured optimal policies. The sample age limit must allow at least two
worker poll intervals. A separate sampler keeps at most one blocking probe
in flight, so a stalled probe ages out without blocking lease watchdogs.

The probe intersects CPU affinity/cpuset counts and CPU bandwidth limits across
visible cgroup v2 ancestors. Memory headroom is the minimum of system
`MemAvailable` and finite ancestor `memory.max`/`memory.high` minus
`memory.current`; it then subtracts the configured reserve. It reads system and
cgroup CPU `some` and memory `full` PSI averages and uses the larger observed
pressure. No cgroup controls are changed. Probe errors, unsupported v1/hybrid
hierarchies, unresolvable cgroup namespaces and stale samples stop admission.
Low headroom or memory pressure blocks new tasks and VM resume. High CPU
pressure blocks additional CPU use. Pausing, lease renewal, cancellation and
completion delivery continue.

The interfaces follow the kernel's
[PSI](https://docs.kernel.org/accounting/psi.html),
[cgroup v2](https://docs.kernel.org/admin-guide/cgroup-v2.html) and
[proc](https://docs.kernel.org/filesystems/proc.html) documentation.
These are node-wide estimates, not per-VM resident measurements or hard
enforcement. External processes, hidden ancestor limits, changing cgroups and
allocations between samples can change real availability. Keep configured
capacity consistent with deployment limits. The dedicated deployment and opt-in
CPU reservation overcommit below add a local enforcement scope. Controlled memory
overcommit and density improvements still require implementation and measurements.
The native CPU QoS profile below adds per-Attempt scheduling classes.

Inspect local Linux observations without controller credentials:

```sh
target/debug/pvisor-cluster probe-node
```

`pvisor-cluster workers` exposes the last admission report, sample age,
controller receipt time, observations, blocked reasons and probe error.
A persisted report after controller restart is historical evidence; only a
fresh poll can admit work. The controller clamps samples older than its lease
duration to zero availability. Older workers can poll without a report and
retain reservation admission; they do not claim pressure awareness.
The worker's final check uses the stricter of its configured sample age and
the learned lease duration, including when retrying an already-issued resume.

The worker checks current cached observations again after HTTP and before
native execution. An assignment rejected here is durably recorded locally,
then returned through `/v1/workers/decline`. The controller permits requeue
only while that exact lease is live and has not been acknowledged as active.
The task retains its immutable specification, rejection count and latest
rejection evidence, and the next assignment receives a new generation.
Old completions and attempt-scoped controls cannot affect the new assignment.
Already accepted or uncertain execution is never requeued through this path.
If cancellation wins the race, the worker confirms that execution never
started through the ordinary completion path. Replay rebuilds the ready queue
from current task state; it cannot lease a declined task twice in one batch.

Issued VM resume retains its CPU charge even if fresh node pressure temporarily
denies the native transition. The same revision is redelivered and gated again
until it can resume or the attempt is cancelled/expires; the controller does
not infer a successful resume from request delivery.

## Observe native VM and node physical memory

Linux VM workers can enable periodic read-only VM and node observations:

```toml
[memory_sampling]
enabled = true
interval_ms = 5000
```

Sampling defaults to disabled. The interval accepts 1,000..60,000 ms and
applies to a whole pass over node measurements and live native VMs. One probe
and one HTTP upload run at a time; VM reports contain at most 64 Attempts per batch. A slow
read or upload delays subsequent observations, without blocking lease renewal,
cancellation, native controls or creating replacement probes. A controller
without a telemetry endpoint returns 404, after which only that observation
channel stops while the other channel and execution/polling continue.

Inspect a task through `show` or the admin task endpoint:

```sh
target/debug/pvisor-cluster show task-1 | jq '.memory_sample'
```

`report.sample.usage` contains the native PID/start-time identity and three
byte-counter partitions: `process`, `guest_ram` and `non_ram`. Each has mapped
size, RSS, PSS, private/shared clean/dirty counters, swap and proportional swap.
Linux hugetlb counters remain separate because the kernel excludes them from
RSS/PSS. PSS divides shared resident pages among mappings; summed RSS counts
shared pages repeatedly. These semantics follow the
[kernel proc documentation](https://docs.kernel.org/filesystems/proc.html).

The supervisor records the process it actually spawns, retains its proc
directory and checks PID/start time before and after a bounded `smaps` walk.
Guest RAM is identified by its backing file's device and inode, including
private COW pages and deleted/renamed pathnames. `non_ram` covers the VMM's
other process mappings. It does not measure KVM kernel allocations, Worker or
FUSE process memory, unreferenced kernel page cache, or every deployment cost.
Mappings/counters can change during a walk, and Linux huge-page PSS can be
approximate; these observations do not establish a maximum future allocation.

The report binds the Run/Attempt and complete lease key. The Worker sends a
monotonic sequence and its sample age; the controller records receipt time.
Older sequences cannot replace newer data; an exact replay preserves the old
receipt time. Conflicting sequences or changed native process/Attempt identity
within a lease are rejected, including after a failed sample. Errors contain
`usage: null` and an explicit reason, rather than zero resident bytes. Probe
start time, age at send and receipt time expose delayed observations; transport
delay is not included in the Worker's age. Do not treat an old successful sample
as current just because the lease still renews.

`POST /v1/workers/memory` uses the Worker API role. It never renews a lease,
changes a reservation or writes a scheduling WAL record. Ended/expired keys
are ignored and do not revive work. Terminal completion or unstarted decline
retires the cached sample. The controller stores observations in bounded live
task records; they disappear on controller restart and are replaced by fresh
Worker reports. Logical CPU/memory/slot admission remains authoritative; enabling
observations never authorizes memory overcommit.

The same profile also reports idle Worker overhead through
`POST /v1/workers/node-memory`, using the Worker API role. Inspect the admin
Worker records:

```sh
target/debug/pvisor-cluster workers | jq '.[] | {worker: .registration.id, memory: .memory_sample}'
```

`report.sample` contains three separate observations. Each has `status: measured`
and `usage`, or `status: unavailable` and a bounded error. A failed cgroup probe
does not erase successful supervisor/system data or manufacture zero usage.

* `supervisor` uses the Worker's own bounded `smaps` walk with PID/start time
  checks. Its RSS/PSS counters include the snapshot RAM FUSE service threads
  running inside that process. Separate watchdog/native VM processes remain
  outside this process observation.
* `system` records the host boot ID, physical memory total, available estimate,
  free memory, cached pages, buffers, slab and swap totals/free counters from
  `/proc/meminfo`. These describe the host, including unrelated work.
* `cgroup` resolves the Worker's membership through its visible cgroup v2 mount,
  verifies the filesystem and retains a directory FD while reading
  `memory.current`, local high/max limits, optional peak/swap usage, keyed
  `memory.stat` and `memory.events`. Device/inode and resolved directory identify
  the scope; membership, mount resolution and directory identity are checked
  again afterward. Missing controllers, root-only interfaces, inaccessible
  files or unresolved namespaces produce an explicit unavailable observation.

The cgroup usage covers its descendants, including anonymous memory, page
cache and kernel charges tracked by the memory controller. Its stat keys retain
kernel units: `anon`/`file`/`kernel` are byte amounts, while keys such as `pgfault`
are event counters; kernel versions can expose additional keys. Stats overlap
and update independently. Usage may temporarily exceed high/max, and concurrent
peak resets mean peak is not a validation bound. These semantics follow the
[cgroup v2 memory documentation](https://docs.kernel.org/admin-guide/cgroup-v2.html#memory).

These scopes overlap. Do not add system usage, cgroup usage and process PSS to
form a node total. Multiple Workers can share a cgroup and host; identify the
host boot and cgroup device/inode before aggregating, and retain scope changes.
For deployment-level accounting, place a Worker and its descendants in a
dedicated memory-controller subtree before launch and verify coverage of
shared-cache, hugetlb and kernel allocations on the deployment kernel. The
probe neither creates cgroups nor moves processes or changes kernel controls.
The existing pressure-admission probe separately checks visible ancestor limits;
the reported cgroup high/max values describe only this scope.

Node reports bind the registered Worker incarnation, monotonic sequence and
sample age. PID/start-time and host boot cannot change within an incarnation,
including after an unavailable observation. Lower sequences are ignored; exact
replays preserve receipt time and conflicting replays are rejected. Registration
retries preserve this binding; a new incarnation clears it. One latest report
per Worker is retained in memory and discarded on controller restart. It is
historical after a Worker stops: reporting never updates `seen_at_ms`, renews a
lease, reaps executions, writes the scheduling WAL or changes resource charges.
Probe age at send excludes HTTP transit time; it does not prove freshness at
receipt. Node telemetry continues when there are no task leases.

## Dedicated Worker deployment and guarded CPU overcommit

The repository provides a user-systemd service and matching profiles in
[`deploy/systemd`](../../deploy/systemd). Each instance runs a Worker and its
native VMMs, shared-RAM watchdog and in-process FUSE threads in a separate cgroup.
The controller and other Workers have separate scopes. The defaults are a
768 MiB memory high watermark, 1 GiB maximum, no cgroup swap, and a 100% CPU
quota; logical reservations allow two 256 MiB, one-vCPU tasks. The admission
profile reserves another 128 MiB of observed memory headroom. These are fixture
starting values, not a production sizing recommendation. Check visible ancestor
limits and pressure as well as this unit's limits.

The unit uses `KillMode=mixed` so the Worker receives SIGTERM and cancels/reaps
native runs; remaining processes are killed after the 30-second stop deadline.
`OOMPolicy=kill` groups the unit for OOM termination. A crash is restarted with a
new Worker incarnation; the existing lease fencing and recovery rules still
apply. The properties follow systemd's
[resource control](https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.resource-control.xml)
and [service](https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.service.xml)
definitions. A Linux host needs a working user manager with delegated CPU/memory
controllers, readable PSI/cgroup interfaces, KVM/FUSE access and native VM
prerequisites. The Worker itself does not create or move cgroups.

Example installation from the repository root after `just cluster-build`:

```sh
install -d -m 700 "$HOME/.config/pvisor" "$HOME/.local/state/pvisor" "$HOME/.config/systemd/user" "$HOME/.local/bin"
install -m 755 target/debug/pvisor-worker "$HOME/.local/bin/pvisor-worker"
install -m 644 deploy/systemd/pvisor-worker@.service "$HOME/.config/systemd/user/"
install -m 600 deploy/systemd/worker.env.example "$HOME/.config/pvisor/node-1.env"
install -m 600 deploy/systemd/worker.toml.example "$HOME/.config/pvisor/node-1.toml"
# Edit node-1.env: set the Worker token, controller URL and absolute cache path.
# Edit node-1.toml: set [vm].library_dir if needed for libkrunfw.
systemctl --user daemon-reload
systemctl --user enable --now pvisor-worker@node-1.service
systemctl --user show pvisor-worker@node-1.service -p ControlGroup -p MainPID -p MemoryCurrent -p MemoryPeak -p MemoryHigh -p MemoryMax -p CPUQuotaPerSecUSec
```

The scope records kernel charges belonging to the unit, including page cache
and kernel memory. Shared caches may have been charged to another scope before
this Worker accessed them; scope membership alone does not account for all
shared physical costs. Keep host observations and verify cache/hugetlb accounting
under the deployment kernel before using these measurements to size memory
overcommit.

`[admission].cpu_overcommit_bps` defaults to 10000 (1x) and accepts 10000..40000.
A value above 10000 requires `mode = "linux_pressure"` and a finite, nonzero
**local** `cpu.max` quota. The measured physical CPU limit still intersects
affinity, cpusets and visible ancestor quotas; admission allows at most that
limit multiplied by the configured ratio, capped by logical Worker capacity
and current reservations. `local_cpu_quota_millis` identifies the local quota;
the ratio appears in admission reports only when enabled. Missing/inconsistent
quota observations, stale probes, high CPU PSI, low memory headroom or high
memory PSI block additional use and VM resume. Memory and slot budgets remain
fully reserved. Already issued resume keeps its precharged CPU budget while
waiting for healthy observations. The kernel quota applies to the whole unit,
including Worker/FUSE work. The CPU QoS profile independently selects each
VM Attempt's scheduler policy and core scheduling group.

Full-machine RAM capture retains zero-filled holes. Shared writable backing is
read through its file while the machine is frozen, avoiding faults into unused
live RAM. Compressed recapture of private restored mappings reads only dirty
64 KiB blocks and inherits independently retained baseline frames as described
above; the full-capture fallback reads private mappings through memory to keep
COW writes and cleared bytes. Staging and raw sealing use distinct sparse
inodes, preserving logical bytes, exact length, hashes, and isolation from stale
capture writers. Restore compatibility checks remain unchanged.

`just test-cluster-cgroup` is an opt-in single-host KVM/FUSE gate using a
temporary user-systemd service with these limits. It verifies local quota and
CPU reservation ratio, actual Worker/VMM/watchdog membership, shared private-COW
restore, native controls under slow telemetry, last-reader RAM cleanup, and unit
shutdown while a VM is paused. The paused VMM must report native cancellation,
release reservations, and leave no unit processes or cgroup. This does not
establish production throughput or latency under CPU contention.
The gate explicitly gives the Worker one Tokio runtime thread. Bundle capture
and durable result persistence run on blocking threads while retaining exclusive
Run ownership; terminal events and controller completion still wait for the
result commit. VM checkpoint binding hashes the executable and firmware on a
blocking thread as well. Final shared environment-lower unmount and FUSE-thread
join also run on blocking threads; completion waits for release, and cancelled
preparations preserve mount ownership through blocking runtime construction.
The last-reader unmount had independently starved the poll thread during
competitor cancellation in the parked-suspension gate. These filesystem/hash
operations previously occupied the async thread and contributed to three-second
lease expiry during concurrent debug VM tests. The dedicated cgroup gate has
passed both alone and alongside other native execution gates. The six-test
native execution suite, including parked suspension and its competitor
cancellation, passes after the shared-lower release change. No pressure
thresholds or lease fencing were relaxed. These checks do not establish
supervisor CPU priority or an upper bound on lease latency under arbitrary host
contention.

Native execution gates pin a private Worker executable per test, including cold
Worker restart, so concurrent Cargo builds cannot replace an active executable
or invalidate its checkpoint binding. Deployment likewise needs a stable
executable for each Worker lifetime: drain and stop before replacing it. Build
identity checks remain required for continuation; an upgrade cannot silently
reinterpret a checkpoint from a different build.

With `PVISOR_STARTUP_TIMING=1`, `vm.checkpoint_binding_begin/ready`,
`attempt.teardown_begin/ready`, `attempt.bundle_begin/ready` and
`image.unmount_begin/ready` mark the hashing, teardown, durable Bundle and lazy
image release paths so delays can be compared with lease renewals.

The pure-Rust FUSE helper keeps its parent control socket close-on-exec and
clears that flag only in the helper child before exec. Linux receives the
FUSE channel with `MSG_CMSG_CLOEXEC`, which the
[kernel SCM implementation](https://raw.githubusercontent.com/torvalds/linux/v6.17/net/core/scm.c)
uses when installing received descriptors. This removes the receive-to-fcntl
window and the parent-side inheritance window during concurrent VM starts.
Conventional tests execute the exact vendored handoff code, verify parent/helper
ownership, and prove that a received channel does not cross an unrelated exec.
The full native execution suite passes with the original lifecycle deadlines
after this correction, including the publication-failure and multi-layer
cleanup scenarios that previously timed out under concurrent load.

Live branching additionally exposed mount references copied into unrelated VM
namespaces after a source exits. Linux mount namespace creation copies the
existing mount list, and private mounts do not propagate later unmounts
([mount namespace documentation](https://man7.org/linux/man-pages/man7/mount_namespaces.7.html)).
The source's final image owner could therefore detach its mount yet wait for
both live children to exit before its FUSE request thread finished. The real
live-fork gate now requires both children to map RAM and acknowledge native
pause/resume before allowing the source to complete, keeping the original
completion deadline.

Linux image and snapshot-RAM owners use an explicit interruptible FUSE session.
Final-owner unmount signals a close-on-exec eventfd, wakes an idle request loop,
and joins the serving thread. This closes the channel even if an unrelated
namespace retains an inaccessible copy of the mount. Legitimate users retain
their existing shared ownership; no active reader is stopped and task completion
still waits for cleanup. Ordinary vendored Fuser sessions retain their existing
unmount-driven lifetime, and macOS retains its prior backend. Conventional tests
verify idle wakeup, readable request handling and shutdown precedence using the
exact vendored helper.

## Native Linux CPU QoS

DSec uses BE/LS classes, `SCHED_IDLE` for BE and core scheduling to separate
classes on SMT siblings ([§5.2](https://arxiv.org/html/2609.22978v1#S5.SS2)).
Enable the corresponding pVisor profile on a Linux VM Worker:

```toml
[cpu_qos]
enabled = true
```

Set the strict, outer `TaskSpec.cpu_qos` field to `"best_effort"` or
`"latency_sensitive"`. The Worker advertises both in
`WorkerRegistration.cpu_qos_classes`; the scheduler keeps unsupported tasks
queued. An omitted class retains the inherited scheduler behavior. Enabling the
profile requires working Linux `CONFIG_SCHED_CORE` support and fails Worker
startup if the kernel cannot create a group. Process/container tasks reject
explicit classes. A nested `run.runtime.cpu_qos` may be omitted or equal the
outer class; a hidden or conflicting nested request is rejected by the Client,
controller and Worker. The Client checks before contacting a controller. Older strict
controllers reject explicit task/capability fields instead of silently dropping
the requirement.

Before native VM threads or namespaces are created, BE installs `SCHED_IDLE`;
LS installs `SCHED_OTHER` and joins one nonzero cookie shared by that Worker's
LS Attempts. BE retains the Worker's inherited cookie, distinct from LS. LS
Attempts can share SMT siblings with each other. A sleeping self-exec helper
pins the LS cookie using a private pipe and cleared environment; the Worker
parent's policy and cookie stay unchanged. Startup waits at most five seconds
for helper readiness, and the final owner kills and reaps it. Worker exit closes
the pipe. These controls are separate from CPU quota and tenant isolation.
Linux documents [cookie inheritance and scheduling groups](https://docs.kernel.org/admin-guide/hw-vuln/core-scheduling.html).

The Worker projects the outer class into the native RunSpec. Native entry must
install the policy successfully, and terminal Bundle observations record the
class, kernel scheduler policy and observed cookie only after a valid private
runner receipt. The parent checks the LS cookie against the pinned group. A
successful guest exit without valid required evidence becomes an infrastructure
failure. Capture persists the class; fork and restore preserve it. Restore after
Worker restart joins the current Worker's group; saved PID/cookie values are
never restored. Every active class and fork branch pays the full execution reservation.
CPU QoS does not alter the existing pause/resume protocol: only an acknowledged
native pause releases its CPU charge, and resume requires fresh CPU admission;
paused Attempts retain their memory and slots.

The opt-in CPU QoS KVM gate inspects the actual scheduler policy and cookie of
all native source/branch threads while BE and LS VMs coexist. It checks shared
LS membership, distinct BE membership, unchanged Worker policy, full reservations,
pause/resume, sealed class metadata, terminal observations and a cold restore
after Worker restart. It also verifies the helper has no inherited credential
environment and is reaped when its final owner is dropped. Run it through
`just test-cluster-vm`. This establishes installation and continuation fidelity;
LS latency under contention, throughput and completed-task density still need
controlled workload benchmarks. Core scheduling can add overhead and does not
guarantee a performance improvement.

## Measure cooperative Agent execution under a finite resource budget

`just bench-cluster-inference` runs paired single-host native Agent experiments.
Both arms use the same immutable Python/scaffold layers and checked tool work:
each guest requests a model reply, writes `multiply(a, b)`, passes three actual
Python assertions, sends the tool result back and verifies that an unauthorized
model is denied. Both send the cooperative header; only the Worker profile's
`release_cpu_on_idle` setting changes between arms. Each arm starts a fresh
Worker in its own user-systemd service with an enforced 200% CPU quota, 4 GiB
memory limit and zero swap, plus identical admission capacity of eight slots,
2 GiB guest RAM and 2,000 CPU millis. Every guest has 256 MiB RAM and one vCPU.

The model fixture supplies deterministic replies with two seconds of latency
per call. One real tool task without that artificial delay warms each arm's
mounts and interpreter; its time and CPU cost are excluded from the measured
eight-task burst. Three blocks alternate ordinary/cooperative arm order.
Neither a faster run nor a density ratio is a passing threshold: every measured
task must succeed, report the expected tool output and have a VM executor plan
and actual native identity evidence. Native PIDs are checked against their
start ticks, Worker parent and cgroup before counting live processes. Admission
must stay within its envelope, and cgroup OOM counters must remain unchanged.

```sh
PVISOR_TEST_LIBKRUNFW_DIR=/absolute/path/to/firmware \
PVISOR_INFERENCE_BENCH_OUT=/tmp/pvisor-agent-inference.json \
JUST_TEMPDIR=/tmp just bench-cluster-inference
```

`PVISOR_INFERENCE_BENCH_BLOCKS` accepts 1..9; the default is three. The JSON
report retains every arm and task completion, CPU counter deltas, cgroup memory
and live-process samples, native identities, paired ratios, environment digests,
Worker/firmware/scaffold hashes and compiled experiment-input hashes. It remains
`complete: false` if a run fails. Latencies include queueing and 100 ms coordinator
polling; VM identity telemetry has a one-second interval. Tail quantiles of an
eight-task arm describe that small sample, not production p95/p99 estimates.

Worker CPU cost includes its native VMMs, FUSE and child processes. The Controller,
model fixture and publisher run outside that service. Its memory counters measure
cgroup charges; native RSS sums can count shared pages more than once. The cache
is warm, and RAM/slots remain reserved during pause. These are measurements of
result-checked tools with synthetic model latency on one host; real-model quality,
multi-host density, GPU/trainer cost and networked RAM reclamation remain separate
acceptance work.

## Observe actual native VM CPU consumption

Enable cheap CPU observations independently of memory sampling and CPU QoS:

```toml
[cpu_sampling]
enabled = true
interval_ms = 1000
```

The default is disabled with a 5000 ms interval; enabled intervals are
1000..60000 ms. Linux VM Workers advertise `cpu_observation_protocol = 2`,
which adds terminal counters to the result wire contract. This controller also
accepts version 1 live-only Workers; older controllers reject version 2 at
registration, before task assignment. A separate bounded
sampling task reads one native process `stat` record through the existing bound
`/proc/PID` directory, without walking `smaps`, reading RAM, retaining backing
files or acquiring the native control exchange lock. Uploads use the normal
five-second Client timeout. One probe/upload is active at a time; only an
endpoint 404 retires the channel, and timeout/failure permits the next tick.
Batches contain at most 64 Attempts. Sampling runs off the async execution
thread, so telemetry does not own lease renewal or admission.

Worker credentials submit `POST /v1/workers/cpu`; admins inspect
`GET /v1/tasks/{id}`. `cpu_sample.report.sample.usage` contains the native PID,
start tick, thread count, kernel tick rate, monotonic sample time and cumulative
user/system/guest ticks. The process record aggregates VMM, vCPU and device
threads, including exited threads. These are native process costs; shared
Worker/FUSE threads, preparation and independent mount helpers have other
owners and are excluded. Linux's
[proc stat contract](https://man7.org/linux/man-pages/man5/proc_pid_stat.5.html)
already includes guest time in user time. Total CPU consumed is **user + system**;
guest time is a subset and must not be added again.

`cpu_sample.interval` appears after two successful measurements. Its
`total_cpu_time_ns` is the counter difference converted with the reported tick
rate, and `cpu_millis` is average consumed CPU over the monotonic interval:
1000 means one fully occupied logical CPU. Multithreaded VMs may exceed 1000.
Tick granularity limits short-interval precision; wall-clock changes and delayed
HTTP delivery do not enter the rate calculation. Raw kernel fields need not be
an atomic partition. Failed probes are explicit errors, never synthetic zero
CPU; the controller retains the last successful baseline across a probe error.

The controller validates the complete batch before publishing. It fences
Worker incarnation, lease/generation, VM Run, Attempt, PID/start identity and
tick rate; successful new measurements require increasing monotonic time and
nondecreasing counters. Equal sequences must carry identical decoded reports,
older valid sequences are ignored, and CPU/memory streams must agree on their
Attempt/process identities. Expired or terminal leases cannot publish new data.
Reports do not append WAL frames, renew leases, release resources or create an
admission discount. Observations clear on termination, decline and controller
restart. They are live utilization evidence. Terminal counters below have a
separate durable owner; neither stream measures complete Worker/node CPU costs.

With CPU sampling enabled, Linux VM execution also publishes
`result.executor_observations.cpu_usage`. A `measured` outcome contains the same
raw process counters as live samples, captured **after the entire native thread
group exits and before the child is reaped**. One event-driven pidfd per observed
Attempt waits for exit without occupying a blocking thread or periodically
polling every VM. `waitid(P_PIDFD, WEXITED | WNOWAIT | WNOHANG)` confirms the
exact exited child and leaves its status for the existing Tokio reap. The bound
proc directory permits only an identity-matching zombie for this final read;
live CPU/memory probes continue rejecting exited processes. Linux documents
these mechanisms in [pidfd_open](https://man7.org/linux/man-pages/man2/pidfd_open.2.html)
and [waitid](https://man7.org/linux/man-pages/man2/wait.2.html).

Normal completion, hibernation, cancellation and deadline termination retain
this observation. Cancellation sends the existing group termination signals,
captures the leader after exit, then finishes the existing descendant cleanup
before publishing the result. A missing/unsupported pidfd or failed final probe
produces an explicit `unavailable` outcome with a bounded error. It never turns
the last live sample into a final reading or invents a zero charge. Disabled
sampling preserves the old result shape and has no extra pidfd. Startup failure
before the native process exists has no process observation.

The controller validates final identity/counters against both live streams,
including nondecreasing cumulative ticks and an increasing sample clock, before committing
completion. It stores final counters in the result WAL frame; the Worker also
persists them in its completion outbox and authoritative Run Bundle. Repeated
completions must retain identical evidence, including after controller restart.
Live observations clear without losing the terminal record. Each restored/forked
Attempt has fresh process counters; snapshot metadata never restores a previous
process's usage. Lease loss or a Worker crash without a durable completion still
has unknown final consumption. Aggregated billing and shared Worker/FUSE/node
CPU costs remain separate work.

The CPU QoS native gate runs a busy BE guest, compares received counters and
identities with independent kernel reads, checks consumption intervals for
source/branches/cold restore and positive consumption for the busy BE guest,
and delays a CPU upload beyond the three-second
lease and five-second Client timeout. Native pause/resume and renewal must still
complete under one Worker Tokio thread; later CPU reports must continue. The
conventional tests cover thread-group accounting after a CPU thread exits,
PID retirement, malformed counters, overflow, stale/conflicting sequences,
partial-batch rejection, cross-stream identity checks and WAL/budget invariants.
These establish real consumption observations and delivery independence;
controlled workload throughput and tail latency measurements remain required.
The native gate also compares completion counters with the last independently
verified live sample for source, busy BE, fork branches and cold restore, and
checks native cancellation/watchdog outcomes and Run Bundle equality after the
processes are reaped. The separate
suspend/cold-recovery gate checks final counters on the hibernated source.
Conventional gates exercise pre-reap zombie counters, SIGTERM-to-SIGKILL
escalation, final cross-stream fencing, conflicting retries and WAL replay.

## Measure native CPU contention

`just bench-cluster-cpu` is a separate opt-in experiment, excluded from the
ordinary suite and `just test-cluster-vm`. It requires Linux x86-64, an allowed
online SMT sibling pair, core scheduling, KVM/FUSE, `cc` and user-systemd. Supply
a firmware directory containing a regular `libkrunfw.so.5` and an absolute JSON
destination:

```sh
PVISOR_TEST_LIBKRUNFW_DIR=/absolute/path/to/firmware \
PVISOR_CPU_BENCH_OUT=/tmp/pvisor-cpu-comparison.json \
JUST_TEMPDIR=/tmp just bench-cluster-cpu
```

The default runs three blocks with different condition orders. Each condition
uses a fresh Worker service, the same immutable base/toolkit, native guest
program, 256 MiB VM size and resource envelope. The Worker and all native
threads are pinned to two online SMT siblings of the same physical core, with
`CPUQuota=200%`, a 2 GiB memory limit, 1.5 GiB high watermark and no swap.
Four 1000m CPU reservations use the production 2x admission policy under that
finite quota; the controller stays outside the envelope. Both the cgroup
membership and every native thread's actual affinity/policy/cookie are checked.

The four conditions are an LS-only reference, three co-located BE VMs without
QoS, the same contention with `SCHED_IDLE` alone, and the production BE/LS
policy including core scheduling. The IDLE-only condition is a benchmark
ablation: the harness changes all live BE native threads before releasing the
guest workload. It does not advertise an additional production QoS mode.
This follows the comparison structure of
[DSec §8.5](https://arxiv.org/html/2609.22978v1#S8.SS5); its chess/agent results
do not predict this fixture's outcome.

The actual VM guest executes a compiled N=13 queen-placement search and checks
every answer against 73,712 solutions ([OEIS A000170](https://oeis.org/A000170)).
Five searches warm each guest before measurement. LS requests arrive at fixed
100 ms intervals for 60 searches per condition per block; a late request retains
its intended arrival time rather than shifting subsequent arrivals. Response
latency includes scheduling/backlog through search completion, before log
publication. It does not measure HTTP tool round trips, VM creation or inference
time. BE guests continuously solve the same verified problem. Their completed
record counts bracket the host measurement window; raw records also retain
the full background execution. Native private launch metadata, actual RAM
descriptor device/inode mappings and vCPU owners independently establish that
the work executes in real VMs. There is no process-backend replacement.

The report preserves source/guest/Worker/firmware hashes, compiler flags,
kernel/CPU topology, per-step intended/start/end/CPU times, each round's
nearest-rank p50/p95/p99, BE progress and before/after Worker `cpu.stat`, PSI and
memory counters. Per-CPU host ticks, host PSI and instantaneous frequency/governor
readings record surrounding load. CPU affinity pins the Worker but does not
reserve the selected CPUs against other host processes; a dedicated host is
still required for stronger interference/causality conclusions.
CPU usage covers this Worker cgroup, including its native
threads and FUSE/helper work; it excludes the controller and services outside
that scope. VM teardown and Bundle persistence begin after the timed window.
Independent guest clock epochs are never subtracted across trials. Every VM
must finish correctly, retain final CPU evidence and release all reservations;
the Worker service must stop without leaving its process behind. Intermediate
reports have `complete=false` and become complete only after all blocks pass.

PASS establishes a completed, identity-checked experiment. It does not require
an improvement or hide the BE throughput/CPU-cost tradeoff. Three blocks provide
180 LS samples per condition; the empirical p99 is descriptive, with no
confidence interval or production SLO claim. This small synthetic single-host
search fixture does not establish agent application latency, cross-host
scalability or general deployment density. Follow with representative useful
agent tasks, lifecycle/creation latency, memory sharing/reclamation and failure
recovery comparisons under identical workloads.

The checked-in [2026-10-04 report](measurements/cpu-qos-20261004.json) completed
all 12 trials on an AMD Ryzen 7 9700X, Linux 7.2.8, SMT CPUs 7/15. Its pooled
180 LS samples per condition produced:

| Condition | LS p50 (ms) | LS p95 (ms) | LS p99 (ms) | BE searches/s | Worker CPU (mCPU) |
| --- | ---: | ---: | ---: | ---: | ---: |
| LS alone | 25.35 | 27.49 | 28.11 | 0 | 316 |
| Unprotected contention | 57.86 | 85.26 | 93.72 | 56.35 | 1983 |
| SCHED_IDLE only | 28.81 | 31.85 | 32.87 | 57.28 | 1957 |
| Full BE/LS QoS | 25.84 | 30.80 | 43.61 | 47.62 | 1598 |

Full QoS reduced this run's p99 relative to unprotected contention while reducing
BE throughput by about 15.5%. It did **not** improve p99 relative to IDLE-only.
The retained [initial three-block report](measurements/cpu-qos-20261004-initial.json)
used identical source, executable/firmware hashes, kernel and CPU pair; its
unprotected/IDLE-only/full-QoS p99 values were 76.04/32.06/32.36 ms, with BE
throughputs 58.60/57.41/47.93 searches/s. The later report additionally records
host tick/PSI/frequency context. Both complete reports remain available; neither
is selected as a universal performance result. These results motivate workload
specific scheduling/host-service interference experiments and dedicated-host
agent benchmarks before recommending blanket core scheduling or making a
general density claim.

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
  after acknowledgement. Terminal attempts use a durable outbox for restart
  export/delivery; GC and journal compaction remain pending. A restarted worker
  first drains known terminal evidence, then uses a fresh incarnation and waits
  for unknown old leases to expire while the Controller remains online, or to
  be reconciled/explicitly resolved after Controller restart. It never adopts those executions and does
  not prove their processes have stopped.
* Drain blocks new reservations and lets existing work finish. Stopping the
  worker with SIGINT/SIGTERM requests native cancellation and sends completion
  evidence before exit. SIGKILL is an unconfirmed loss case.

API routes: admin credentials can publish/read environment templates,
submit/read/cancel/control tasks, download
artifact manifests/objects and storage usage, list/drain workers and read counts; worker
credentials can register/poll/recover/decline/complete/acknowledge controls,
report VM/node memory and upload lease-bound artifact objects. Health
exposes the protocol version. Both roles are trusted deployment services.
TLS termination and node/tenant credential issuance are deployment work still
to implement. The HTTP request limit is 4 MiB; the single shard retains at most
one million task records by default.

## Controller history load and restart memory

The scheduler retains one authoritative `TaskRecord` per task. Its private
B-tree stores the existing boxed allocation from WAL submission, rather than
reserving the complete record in every node slot. On the measured Linux amd64
build, `TaskRecord` is 4,040 bytes, while its boxed pointer is 8 bytes. The public
Core records, JSON and transaction format are unchanged. Phase counts are
derived incrementally, and terminal/cancelled tasks are removed from the ready
index. Replay rejects a task-budget overrun before proceeding to later
transactions or repairing the WAL tail. It releases the replay-built ready
index before rebuilding deterministic `(updated_at_ms, task_id)` order.

The [history-load example](examples/scheduler_load.rs) exercises cancelled
history plus live work, or a dense all-ready queue. It checks aggregate counts
against the former full-record scan on the same authoritative records, verifies
that reads do not change the WAL, completes one synthetic assignment and
reopens durable state. Version 3 also checks resumed scheduling: terminal work
does not reappear and the next queued task retains the restart order. Source
identities must remain unchanged during a measurement.

The 2026-10-05 paired layout measurements used the repository's release profile
(`opt-level=z`, thin LTO), CPU affinity 4, a local NVMe WAL and a separate process
for each size/mode. Each binary was copied before execution; the provenance
records retain executable/source hashes and commands. The boxed build's source
hashes were checked before/after compilation and execution. The baseline is the
immediately preceding streaming-replay/compact-graph implementation, not the
older clone-based v1 reference.

| One million retained tasks | Inline record layout: RSS after replay | Boxed layout: RSS after replay | RSS reduction |
| --- | ---: | ---: | ---: |
| 999,999 cancelled and one ready | 9.56 GiB | 6.40 GiB | 33.0% |
| All initially ready | 9.76 GiB | 6.61 GiB | 32.2% |

These are whole-process readings, including the synthetic ID fixture and
allocator retention, rather than isolated live-record heap sizes. Dense-mode
process high water fell from 9.84 GiB to 6.61 GiB; the high water includes all
phases, not only replay. The observations are individual local runs, with
changing host load, not confidence intervals. Replay wall time improved in the
history run (23.85 to 16.09 seconds) but regressed in the dense run (15.93 to
22.97 seconds); these data do not establish a restart-speed improvement.

On the boxed million-history fixture, median indexed counts took 169 ns per
call versus 134 ms for the full-record scan; both returned the same two phase
counts. This compares only the monitoring algorithm. A first assignment arrived
in one poll without visiting cancelled entries. A single durable poll, a small
phase-count read and warm local replay do not measure HTTP throughput, native
VM execution, cold-storage recovery or useful Agent density. Default policy and
optional-observation layouts still contribute substantial retained memory.

Raw evidence: [inline history](measurements/controller-indexes-20261005-v2-history-1000000.json),
[inline dense](measurements/controller-indexes-20261005-v2-dense-1000000.json),
[baseline provenance](measurements/controller-indexes-20261005-v2-provenance.json),
[boxed history](measurements/controller-indexes-20261005-v3-history-1000000.json),
[boxed dense](measurements/controller-indexes-20261005-v3-dense-1000000.json) and
[boxed provenance](measurements/controller-indexes-20261005-v3-provenance.json).
The smaller 1,000/10,000/100,000-task measurements are retained alongside them.
An interrupted boxed million-history attempt produced no validated output;
the provenance records this separately from the successful resumed runs.

For comparable local measurements, choose an allowed CPU and use a fresh
output path for each invocation:

```sh
cargo build --locked -p pvisor-cluster --example scheduler_load --release
mkdir -p target/controller-wal-scratch
TMPDIR="$PWD/target/controller-wal-scratch" taskset -c 4 \
  target/release/examples/scheduler_load --tasks 1000000 --ready 1 \
  --samples 20 --indexed-batch 100 --output /tmp/controller-history.json
TMPDIR="$PWD/target/controller-wal-scratch" taskset -c 4 \
  target/release/examples/scheduler_load --tasks 1000000 --ready all \
  --samples 20 --indexed-batch 100 --output /tmp/controller-dense.json
```

The example explicitly raises its finite WAL ceiling to at least
`tasks * 4096` bytes. A million-task graph fixture writes about 1.9 GiB and
exceeds the production default 1 GiB ceiling. It is not evidence that production
storage limits should be removed. Temporary trees are removed on normal exit;
an interrupted process may leave its own WAL fixture behind.

## Validation and next implementation gates

```sh
just test pvisor-cluster pvisor-core
cargo nextest run --locked -p pvisor --test cluster_execution
cargo nextest run --locked -p pvisor --bin pvisor-worker
cargo nextest run --locked -p pvisor --lib -E 'test(runtime::run::) or test(executor::vm::control::)'
cargo nextest run --locked -p pvisor --lib -E 'test(image::cache::portable::tests::) or test(image::cache::lazy::tests::)'
just test-cluster-vm
cargo clippy --locked -p pvisor-cluster --all-targets -- -D warnings
cargo clippy --locked -p pvisor --bin pvisor-worker --test cluster_execution --test cluster_environment_vm -- -D warnings
```

`just test-cluster` runs the Core/controller suites and Worker/HTTP tests above. Loopback HTTP tests
require permission to bind ports; they fail rather than silently skipping
when the execution environment denies networking.

`just test-cluster-vm` explicitly selects the opt-in hardware test. It requires
Linux amd64, accessible `/dev/kvm` and `/dev/fuse`, working user/mount/network
namespaces, and compatible libkrunfw. Device or namespace failures fail the
test. If firmware is not installed in a default loader path, set
`PVISOR_TEST_LIBKRUNFW_DIR` to a directory containing `libkrunfw.so.5` as a
regular file (a symlink to a directory outside the runner's allowed paths is
insufficient). Small rootfs fixtures copy the host shell/sleep ELF programs and
their libraries using `ldd`; they do not pull registry images. The gate creates
temporary cache/state and destroys its worker after completion. Native child
reentry runs before the Worker's Tokio threads start, as Linux user-namespace
setup requires a single-threaded process. A second opt-in test starts a real
rootless Worker, verifies the child has a single-entry user-namespace UID map
and receives no worker credential, and downloads its native isolation evidence.
A third test captures an ordinary no-network VM through remote controls,
requires sealed filesystem root device/inode identities to match those in the
native captured machine state (proving publication did not copy them again),
verifies CPU/RAM/device and owned-layer integrity outside the user namespace,
materializes independent backing, and checks continued guest memory and retained
admission charges. It then removes original backing and cache availability and
verifies cold continuation with a new Run/Attempt, lineage, saved memory and an
open file descriptor. The restored VM seals another full checkpoint and
pauses/resumes under normal CPU admission; offload of its private COW RAM is
rejected. The original snapshot remains unchanged. It also forces receipt
failure after an object is sealed,
verifies native termination of the uncertain source and release of its
reservations, and rejects a successful control observation. The receipt fault
requires an unprivileged host owner so directory write permissions are enforced.
A fourth test suspends a real VM while its shell retains an in-memory value
and an open descriptor. It confirms the source never continues, reuses a
Worker with exactly one VM memory budget and one slot for another VM, restarts
that Worker, removes original backing and cache availability, then restores
the checkpoint under a new Run/Attempt and Worker incarnation. This proves
safe capacity reuse and continuation, not a measured density or speed increase.
A fifth test concurrently restores two real VMs from one sealed checkpoint.
It reads their native `/proc/PID/smaps` RAM mappings, requires the same device,
inode and path, positive shared clean pages and private dirty pages, and lower
combined PSS than combined RSS. On dynamic Linux builds it additionally requires
libkrunfw mappings in the fresh source VM and their absence in both restored VMs.
It verifies divergent guest memory, open-descriptor writes and lower copy-up
in separate uppers. Both Attempts must reference the same verified toolkit lower
while their apply targets remain private; the shared tree must stay unchanged.
The test deletes/collects the snapshot while both mappings live, rejects a new
restore of that deleted reference, and continues the remaining VM after its
peer exits. Retained Attempt references keep filesystem lowers available after
both native owners exit and the store is reopened. The last VM must release
its RAM mount. These measurements cover the small test fixture's
guest RAM, native/Worker process partitions and system/cgroup scopes, rather than
a before/after workload benchmark or completed-task density. The test also requires physical
reports to arrive through the Worker HTTP endpoint, tracks the remaining VM's
PSS increasing when its peer exits, and retires terminal observations. A delayed
VM telemetry request and a separate node telemetry request each exceed both
the Client timeout and lease interval while native pause/resume and lease renewal
continue. The test validates real idle/active Worker reports, cgroup identity,
page-cache/kernel counters, and continued VM observations after a node-endpoint 404.

An isolated Linux KVM run on 2026-10-04 observed about 17.6 MiB process PSS per
restored VM: 6.9 MiB guest RAM and 10.6 MiB other mappings, with two paused
branches of the 256 MiB-budget shell fixture. These observations are informational,
not acceptance thresholds or a controlled before/after density benchmark. Run
the hardware gate with `NEXTEST_SUCCESS_OUTPUT=immediate` to retain its native
memory reports; host mappings and concurrent processes can change PSS.

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
through controller restart. Restore tests reject pending checkpoint observations,
cross-tenant references, reused Run identities, wrong parents and changed
commands/environments/resources; they verify owning-Worker capability, full
admission charges, durable checkpoint resolution and fenced lease redelivery
after WAL replay. Suspend tests retain all charges after sealing, reject
forged/pending/native exit receipts, atomically recover missing control
acknowledgements, and preserve cancellation/expiry fencing. Outbox tests
retain hibernation receipts across restart and reject changed Attempt bindings.
File policy tests reject changed authorization rules without
modifying saved state and allow rebinding audit identity. The HTTP lifecycle
test uses a synthetic worker
client. Native control regressions cover cancellation, readiness, control
acknowledgements and backing-file ownership. The separate real VM gate confirms
native pause and offload acknowledgements, lease renewal beyond one interval,
unchanged memory/slot charges, pending resume when another task consumes the
released CPU budget, cancellation of that competing VM, and successful guest
continuation under the original generation after CPU becomes available. Remote
control retries return the same completed record. This remains a single-host
hardware test, with no inference service or distributed node recovery.

Pressure tests exercise ancestor limits and mount-root mapping, finite PSI
parsing, arithmetic bounds, freshness, telemetry persistence, renewal under
probe failure and rejection generation fencing. A real Worker/HTTP test
deliberately sends a task despite the reported zero budget and proves final
admission returns it to the queue without a native run or command side effect,
then another Worker completes the same task under a new generation.
Fixtures and this adversarial service test do not establish measured CPU QoS,
memory reclaim or production-scale performance.

Artifact tests verify missing/corrupt chunks, whole-file and attempt identity
mismatches, unsupported workers, traversal filenames, role separation, body
limits and late uploads. A real worker test loses successful upload replies
for longer than a lease interval, proves continued renewal and multi-chunk
retention, then restarts the controller and compares downloaded bytes with
the native Bundle. Other real worker tests interrupt uploading with cancellation
and confirm export failures retain native completion without repeating commands.
Restart tests SIGKILL real worker processes after native execution has ended,
resume interrupted upload while losing replies for longer than a lease interval,
reject a mismatching successful completion reply, replay committed evidence after
expiry, repeat restart after a durable receipt, and fence uncommitted expired
results. They compare native/downloaded Bundle bytes and verify commands execute
once. Scheduler tests prove recovery leaves unseen leases and queued tasks alone;
atomic-write tests cover corruption/symlinks and the receipt-before-unlink window.
The queue-capacity test fills 4,096 persisted entries, rejects another, then
accepts it only after a durable receipt releases capacity. A real binary-output
test sends 1 MiB of NUL bytes, confirms inline completion fits the HTTP limit,
and downloads the full native capture from the Bundle.
Extended tests remove the original trace after sealing/upload starts, then
restart the Worker and verify exact downloaded sealed bytes with one command
execution. Protocol tests refuse missing requested files without a WAL commit.
Archive tests exercise deletion whiteouts, absolute symlinks, FIFO rejection
and oversized sparse files. The concurrent native Agent gate downloads both
actual model/tool journals and generated Python modules in separate upper
archives, plus exact non-UTF-8 binary output spanning multiple chunks. The native suspension/restart gate verifies the frozen file in the
source archive and the later file in the cold continuation's archive.
These tests do not exercise filesystem ENOSPC or restore a killed live VM.

Next gates, in dependency order:

1. Extend the [Gateway cooperative wait/delivery lifecycle](../pvisor-gateway/README.md#cooperative-inference-waits)
   with parallel-call fault experiments and longer controller outages.
   The bounded protocol, real Linux VM CPU release/readmission and
   controller-server restart and process SIGKILL/lost-Ready-response gates are
   implemented. Restart requires fresh Worker lease reconciliation before
   response delivery. Extend coverage to uncertain native control acknowledgements
   and external service failures; current process-fault coverage stays within
   the existing Worker watchdog.
   Extend VM controls to multi-host fault tests, and validate shared
   caches, hugetlb and deployment-kernel coverage of the memory observations.
   Never release memory merely because a desired state says idle or a single
   backing-file residency sample is zero.
2. Add capture-side private filesystem deltas and validate cross-node recovery with runtime
   compatibility and independently deployed Workers.
3. Validate multi-host environment/artifact distribution and extend composition to
   containers; add per-tenant artifact quotas and replicated storage,
   and connect persistent scaffold state with Gateway-aware checkpoints and
   worker/node recovery.
4. Extend durable per-Attempt CPU counters with rollout/billing aggregation and whole-node costs,
   and controlled memory overcommit using node reports;
   validate the opt-in CPU ratio under realistic contention and useful task load;
   shard/replicate authority and group durable operations.
5. Measure completed useful tasks/sec, p50/p95/p99 step and lifecycle latency,
   resident RAM per live/waiting sandbox, CPU time, cache/network bytes and
   failure recovery under identical workloads and baselines. Density or speed
   improvements require measured VM/task experiments, not scheduling counters.
