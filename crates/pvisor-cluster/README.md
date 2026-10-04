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
| Distributed user task execution | HTTP submit/show/cancel, multi-worker process execution, common RunSpec/RunResult; optional remote native Run Bundle retention | Multi-host workload, task graph/scaffold integration, workspace/trace export |
| Backend/isolation selection | Exact execution class and label matching; host/rootless/container/VM workers; real HTTP-dispatched VM execution on Linux | Multi-host VM/container execution and host-policy failure experiments |
| Reliable control | fsync-before-ack WAL, fencing, cancellation, expiry, drain, idempotent submit/completion; unstarted rejection/requeue; durable terminal-result outbox and restart export/delivery | Recovery of live execution, disk-full faults, multi-host failure tests |
| Scalable scheduling | Bounded ready window, indexed expiration, batched leases, reservations, tenant quotas | Sharding, replicated authority, group commit, admission/load measurements and large-scale benchmarks |
| Independently versioned base/workspace/toolkit layers | Durable immutable template registry; lease-bound revision handles; VM worker composes native lazy-cache layers with private upper and shared live read mounts; real Linux VM composition/upper isolation gate | Distribution deployment and measured startup/density benefit; container composition |
| AgentENV pause/resume | Durable lease-bound desired/observed pause/offload/resume; native controls verified on real Linux VM; CPU reserved before resume | Multi-host VM lifecycle/fault experiments and inference-wait coordination |
| Incremental execution checkpoints, fork and recovery | Ordinary Job executor explicitly rejects full execution capture; virtio-fs can rebind verified, fully owned overlay copies | Connect full VM state capture/restore to Job driver; independent forks, compatible runtime identity, remote storage and recovery tests |
| Dense memory use | VM size derived from task admission budget; host-local shared RAM/cache profile options; offload observations retain RAM charge | Physical resident-memory accounting, controlled reclaim/overcommit and measured density improvement |
| CPU QoS/controlled overcommit | CPU reservations; optional Linux PSI, affinity and visible cgroup v2 CPU/memory admission; native resume gating | BE/LS enforcement, per-attempt physical accounting, pressure-aware overcommit and latency/isolation verification |
| RL preemption/resumption | Lease protocol and per-task evidence | Preserve rollout/scaffold state independently of GPU scheduling; resumable checkpoint coordination |
| Access control and observability | Distinct admin/worker API credentials, explicit task environment, local trace; verified remote native Bundle downloads | Per-tenant/node identities, TLS deployment, centralized workspace/trace artifacts, dynamic task policies/Gateway integration |

Completion requires the whole matrix, not only passing scheduler tests. The
unconnected ordinary-Job checkpoint path is documented in
`crates/pvisor/src/cli/checkpoint.rs::execution_blocker`; VM pause/offload is
not a complete checkpoint, portable migration or a claim of zero resident RAM.

The overlay rebinding prerequisite preserves ordered lower layers, private upper,
work/preimage directories, saved guest file handles and directory cookies. It
also relocates original lower inode identities so later copy-up of an unseen
hard-link alias joins the existing upper inode. Descriptor-ring tests remove the
original tree before restoring and verify a second copy/fork. The coordinator
must verify the complete owned-tree inventory before rebinding, including files
never opened by the guest. External backing roots, missing hard-link origins and
invalid copied state fail without changing the saved server state. This full-copy
primitive does not yet connect ordinary Job checkpoints, shared immutable cache
bindings, CPU/RAM handoff or controller-directed recovery.

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

## Retain and download native execution evidence

Set `"retain_bundle": true` in a task specification to require retention of
the native `run-bundle.json` before successful cluster completion. The default
is false. Only workers advertising the artifact protocol can receive such a
task. This requirement is part of the immutable submission specification.
The checked-in `hello` task example enables retention.

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
limit per file. The current worker exports one native Bundle. The manifest
binds files to the exact task, lease generation, worker and incarnation. Before
accepting completion, the controller verifies every chunk, whole-file digest
and Bundle Run/Attempt identity, terminal state, timestamps and exit code
against the completion result. The worker additionally validates the complete
native Bundle schema. Bulk verification runs outside the scheduler lock.
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

This exports the Bundle itself. Paths to local traces, workspace files and
other artifacts inside it remain local references; this is not complete
workspace export, an execution snapshot, or portable recovery. Object storage
is controller-local and append-only. Storage quotas, orphan GC, remote
replication and live execution recovery remain implementation gates. Restart
export/delivery of already terminated attempts uses the outbox below.

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
durability can be established. Completed receipt/evidence storage still needs GC.

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
# Or use the native S3 backend with host-owned storage credentials.
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
limit. Each attempt gets a separate RAM backing. VM rootfs apply is protected.
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
capacity consistent with deployment limits; physical overcommit, CPU QoS,
overhead accounting and density improvements still require implementation
and measurements.

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
  for unknown old leases to expire. It never adopts those executions and does
  not prove their processes have stopped.
* Drain blocks new reservations and lets existing work finish. Stopping the
  worker with SIGINT/SIGTERM requests native cancellation and sends completion
  evidence before exit. SIGKILL is an unconfirmed loss case.

API routes: admin credentials can publish/read environment templates,
submit/read/cancel/control tasks, download
artifact manifests/objects, list/drain workers and read counts; worker
credentials can register/poll/recover/decline/complete/acknowledge controls and upload
lease-bound artifact objects. Health
exposes the protocol version. Both roles are trusted deployment services.
TLS termination and node/tenant credential issuance are deployment work still
to implement. The HTTP request limit is 4 MiB; the single shard retains at most
one million task records by default.

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
These tests do not exercise filesystem ENOSPC or restore a killed live VM.

Next gates, in dependency order:

1. Extend VM controls to multi-host fault tests, connect inference-wait
   coordination, and add per-attempt physical memory/overhead accounting.
   Never release memory merely because a desired state says idle or a single
   backing-file residency sample is zero.
2. Connect full execution snapshot/restore and immutable checkpoint lineage
   to ordinary Job attempts, then fork, restore and cross-node recovery.
3. Validate multi-host environment distribution and extend composition to
   containers; extend native Bundle retention to workspace/trace artifacts,
   and connect Gateway/scaffold state and checkpoint-based worker/node recovery.
4. Add enforced CPU QoS and controlled memory/CPU overcommit using node reports;
   shard/replicate authority and group durable operations.
5. Measure completed useful tasks/sec, p50/p95/p99 step and lifecycle latency,
   resident RAM per live/waiting sandbox, CPU time, cache/network bytes and
   failure recovery under identical workloads and baselines. Density or speed
   improvements require measured VM/task experiments, not scheduling counters.
