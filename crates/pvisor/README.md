# PolicyVisor (pVisor)

**Scaling autonomous agent execution.**

PolicyVisor (pVisor) manages execution for Agent CLIs, scripts, and automation
commands. The **p** stands for **Policy**: connect requested capabilities,
effective runtime controls, and reviewable results.

Owns one Job, its internal Run record and Attempts, capability admission,
filesystem Effects, and execution placement. It is an embeddable library, not
an application frontend. The host CLI (`pvisor`) lives in
[`pvisor-cli`](../pvisor-cli/README.md). The runtime can
place Jobs on host, native OCI container, and `pvisor-vm` executors while preserving one
Run contract.
It is not an Agent framework, an OCI runtime, or an operating system.

OverlayFS, OverlayNet, Gateway, and AgentCtl are pVisor runtime drivers.
`pvisor-core` defines Operations, Events and cross-component contracts. This crate
owns Session lifecycle, scheduling, policy adaptation and execution. The
`pvisor-cli` application owns Job lifecycle commands, the per-Job owner
bootstrap, terminal/rendering code and companion discovery.
Cache storage and node resource protocols remain runtime components; cache
argument parsing lives under `pvisor cache` in the app; `pvisor-daemon serve --cache`
owns the server entry point.
Job commands use `pvisor`; `pvisor-daemon` owns cache and sandbox services and its optional
pool. TUI and replay are
Job frontends, not runtime dependencies. Cross-node placement and distributed scheduling
belong to external orchestrators, not this crate.
Guest injection uses the core `pvisor` execution runtime.

```mermaid
flowchart TD
    Entry[CLI / PVisor API] --> Session[Session: one Attempt]
    Core[pvisor-core contracts and policies] -.-> Session
    Session --> Executor[Host / OCI container / pvisor-vm]
    Session --> Drivers[OverlayFS / OverlayNet / optional Gateway]
    Session --> Records[Run record / Run Bundle / optional Event Journal]
    Records --> Review[status / inspect / apply / drop]
```

| Product area | Current responsibility |
| --- | --- |
| Run lifecycle | One logical `Run`, currently one `Attempt` per execution, cancellation, deadlines, terminal publication, and parent lineage |
| Agent control | Host AgentCtl v1 for typed Job CLI authority; isolated optional Guest AgentCtl for client state, directives and cooperative quiescence |
| Capabilities | Models, tools, filesystem read/write, network, secrets, subprocess, and resources, with evidence recorded per dimension |
| Filesystem effects | Copy-on-write staging, classified review, logical checkpoint/fork, repeated selective apply, terminal apply/drop, and an apply ledger |
| Network and model access | Gateway capture plus OverlayNet policy; enforcement strength depends on executor and is never inferred from a product label |
| Execution placement | Host process, native OCI container executor, or `pvisor-vm` (Linux KVM / macOS HVF) using an OCI image, prepared rootfs, or Linux host rootfs |
| Evidence | Run Bundle, lifecycle events, capability enforcement, filesystem changes, network counters, AgentCtl observations, output, and artifact references |

With `--stage PATH`, the product loop is `RunSpec → admission → Attempt →
RunResult + private Run Bundle + staged Effects → review/apply/drop`. Ordinary
host Jobs without staging write through to the workspace. `--safe` and `--ask`
retain the workspace stage in Job storage by default; `--stage PATH` selects
another location. HOME and VM rootfs writes have separate lifetimes; see
[staging and storage](../../docs/src/en/reference/cli.md#staging-and-storage).
Capture is a Gateway capability, not a second product.

New Attempts use Run record schema 2 and completion protocol 1. Older readers
reject schema 2 instead of bypassing its completion check. Session durably prepares the Bundle,
publishes its terminal events, writes the terminal Run projection, then atomically
publishes `completion.json` binding the typed result to the Bundle's BLAKE3 digest.
Only that receipt commits completion. Readers reject missing/changed Bundles and
foreign Attempt receipts. An offline Attempt without a receipt is `Unknown`,
with its stage retained and workspace mutation refused. `status` reconciles stale
Run/Job projections under their metadata and Attempt leases; it never takes a live
Attempt's lease or infers success from owner death. Legacy records without the
protocol marker retain their original read rules. This is local crash consistency,
not authentication against another process with unrestricted access as the host UID.
Job metadata lock waits are bounded to five seconds and preserve uncertain outcomes.
Completion and Bundle reads reject symlinks/nonregular files and are capped at
64 MiB each. Very large captured results or file inventories can exceed this bound
and fail finalization while retaining the stage.

Local sandbox evidence comes from individual control installation receipts on a
parent-retained inode. The launcher unlinks its pathname and closes the descriptor
before workload execution. A ready marker or admission mechanism list cannot
substitute for these receipts.

`RunSpec.runtime.profile` is the shared `RunProfile::{Default, Safe}` request.
Safe requires sandbox admission before workload start; its selective host network
proxy may still be cooperative. `PolicyMode::Enforce` independently requires
non-bypassable controls. Bundle `safe_profile_requested` comes from this typed
request. `RunConfig.run.profile = "safe"` selects the CLI preset.

Apply requests require explicit `all = true` or a nonempty path/include/exclude
selection. Empty selection is rejected before target/stage mutation. The low-level
OverlayCore selection contract remains unchanged for its existing callers.

Admission carries the final network configuration into every Attempt driver;
preparation does not re-resolve policy from mutable Run metadata or configuration.
Guest workspace overlays require `RunExecutor::supports_guest_workspace_overlay`
(default false), independently of VM network attachment support. Executor names
remain persisted descriptions, not runtime capability checks.

Containers use the host architecture. `--container-platform` (configuration:
`container.platform = "linux-amd64"` or `"linux-arm64"`) optionally asserts that
native platform; an explicit non-native selection is rejected before execution,
including with a prepared rootfs or custom injected pVisor. It does not enable
cross-platform emulation or artifact auto-discovery. A configured platform also
requires the container executor rather than being ignored by host/VM execution.
Without the option, image preparation and container execution use the native platform.
The OCI executor injects the `pvisor` executable and delegates the Job through
`spawn --executor host --spec ... --result-file ...` inside the container.
Its RunSpec is mounted read-only, and its writable output directory excludes the
OCI bundle/state. Result reads reject symlinks, nonregular files, foreign Run
identities and payloads over 16 MiB. Boundary-writable JSON supplies transcript
bytes only: the host-observed OCI runtime exit determines completion, and delegated
AgentCtl directives, metrics, artifacts and enforcement claims are not imported.

## Explicit runtime experiments

Runtime feature names and metadata live in `src/features.rs::REGISTRY`, with
`name`, `stage`, `default` and `description`. This registry does not imply
upstream Codex behavior or compatibility. Runtime features are not Cargo build
features.

```bash
pvisor feature                       # name / stage / default / enabled / description
pvisor feature list --json           # same list as a JSON array
pvisor --feature workload-aware-memory-offloading feature --json
pvisor --feature workload-aware-memory-offloading run --executor vm -- /bin/sleep 10
pvisor spawn --executor vm --feature workload-aware-memory-offloading -- /bin/sleep 10
pvisor --feature workload-aware-memory-offloading --executor vm -- /bin/sleep 10
```

`--feature NAME` is global, repeatable, accepts comma-separated names and rejects
unknown names. Arguments after `--` belong to the guest and are never feature
options. Feature queries run locally without starting a Job or contacting its
owner. Their `enabled` column means registry defaults plus this invocation's
CLI enables, not a live VM status or a scan of personal/project config files.
Global enables are supported by `run` and feature queries, not other Job actions
or extensions; extension arguments and help requests are forwarded to
companions. Companion help also supports leading feature options, without
forwarding those options as runtime enables; use `pvisor help COMMAND` or
`pvisor COMMAND --help`.

Run TOML supports strict bool keys in a centralized table:

```toml
[run]
executor = "vm"
command = ["/bin/sleep", "10"]

[features]
workload-aware-memory-offloading = false
```

Run configuration and personal Agent defaults are loaded during Run resolution;
omitting `--feature` preserves their value, while CLI enables override `false`.
There is no CLI disable switch yet. VM-only features require the resolved VM
executor (`--executor vm`, `--vm`, or existing VM inference/config); otherwise
execution is refused, never silently ignored or switched from host/container.
Host-only delegated JSON `--spec` cannot enable this feature.

The registered name `workload-aware-memory-offloading` (workload aware memory
offloading) registers **EXP-001 M0**, stage `experimental`, default `false`.
The former CLI/config name is rejected; there is no compatibility alias.
It travels as serialized `RunArgs.features` in the internal typed
Host request, resolves into `RunConfig.features`, and is explicitly passed through
`VmExecutor::with_features()` into `RunnerSpec.features`. Resume/execution-fork
retain the stored Run feature settings. Embedded callers use public
`features::Feature::WorkloadAwareMemoryOffloading`,
`features::FeatureSettings::workload_aware_memory_offloading` and
`VmExecutor::with_features()`; a stored RunConfig alone does not configure an
independently constructed executor.

The native runner calls `pvisor_vm::api::VcpuObservationControl::
set_vcpu_observation(true)` on the built VM handle. Failure aborts VM startup;
default-off runs do not call the observer control. Supported builds are Linux
x86_64/KVM and Apple Silicon macOS/HVF; other builds reject enabling it. This
only turns on the runtime's wait observation: it neither samples/exports
observations through CLI status nor decides guest idleness, wake deadlines, cold
reclamation, or automatic offload. Embedded callers query observations through
`vcpu_observation()`; there is no Host VM observation operation. Cold/memory
flags are independent and are not implicitly enabled. Parser/config/control unit tests are not
real-guest, macOS platform, energy/density, or upstream validation evidence.

## Cooperative native model waits (EXP-003 M0)

With the `gateway` Cargo feature, embedded callers can enable
`GatewayDriverConfig::native_model_wait(NativeModelWait::Pause)` or
`NativeModelWait::Offload`. The default is off. Session creates a fresh adapter
bound to the actual Run/Attempt before starting Gateway. Linux x86_64 VM execution
and an enabled Gateway are required; a custom `model_wait` lifecycle conflicts.
There is no CLI/config-file toggle for this prototype.

The trusted workload declares whole-guest quiescence with
`x-pvisor-inference-idle: true`. It must be the guest's only runnable work and
have no outstanding filesystem/device operations. Ordinary model calls retain
normal forwarding. This declaration does not discover other runnable processes
or transfer dirty guest pages. Pause stops vCPUs; Offload also uses the existing
shared file-backed RAM/device barrier. Private COW/pager RAM remains unsupported
for offload. A failed device drain fails the Attempt instead of probing idleness.

At most one declared wait is reserved per Attempt, including accepted controls
and cancellation cleanup. Foreign/missing Run or Attempt identities and parallel
waits are rejected before upstream dispatch. Entry waits for native ACK; delivery
waits for resume ACK. Buffered bodies and supplier failures use the existing
Gateway barrier; SSE resumes on its first nonempty network chunk, which may be a
heartbeat. Full inference spooling and the guest submit/wait/cancel/ack protocol
remain unimplemented.

Manual pause, resume and offload share the native transition lock and revoke the
automatic pause epoch when admitted, even if the later native operation fails.
Delivery after manual pause waits for an explicit resume or Attempt cancellation.
Dropped handlers enqueue bounded cleanup of only their own pause; uncertain
native ACKs or failed cleanup cancel the Attempt. Execution capture/suspend is
rejected while a reservation exists. The existing runtime retains resource
admission and its storage lease; this prototype does not release/reacquire CPU
reservations or introduce a scheduler lease. Control observations include
`model_wait_epoch` (null for manual operations).

`native_model_wait_case` drives a real guest through Gateway and a gated local
model supplier, with an 8 MiB guest-memory digest checked after delivery. Build
with `cargo build --locked -p pvisor --features gateway --example
native_model_wait_case`; pass `running`, `pause`, `offload`, `manual` or `cancel`
and a fresh output directory. Dynamic builds require
`PVISOR_CASE_VM_LIBRARY_DIR`; optional `PVISOR_CASE_ROOTFS` and
`PVISOR_CASE_VM_PYTHON` select guest inputs (defaults `/` and `/usr/bin/python3`).
Each run writes `result.json` with final status and control observations. These
are correctness checks, not memory-saving, throughput or density measurements.

## Job owners and AgentCtl

`pvisor spawn` starts a terminal frontend and one owner over an inherited private
socketpair. The owner stays in the original session, in its own process group;
Linux makes it the subreaper and retains pidfd-based descendant cleanup. For an
interactive Job the frontend gives the original controlling terminal to the
owner/workload, so terminal Ctrl-C can reach the workload, and restores the
foreground group at exit. Pipes inherit their descriptors directly. The owner
installs signal handlers before unblocking SIGINT/SIGTERM/SIGHUP. Frontend
cancellation and disconnection are handled by the owner. No user-wide Job
listener or per-command worker is required. Once the Attempt has published
terminal status, owner escalation allows the runtime's bounded RAM/macFUSE
resource teardown to finish before forced exit and Job completion publication.

Each running Job publishes `live_control` in its host-only Run record before
executor entry. It contains the canonical path, device/inode, protocol
`pvisor-instance-control-1`, owner PID, executable path and BLAKE3 digest:

```text
/tmp/pvisor-host-<UID>/jobs/<job-id>/<attempt-id>/control.sock
```

The root and every directory are owned by the effective UID, non-symlink and
`0700`; sockets are `0600`. Long Job/Attempt paths are accessed through a checked
directory descriptor rather than truncating the canonical address. Clients check
socket identity before and after connecting and authenticate the peer UID.
Executors exclude the entire private authority root from guest projections.
Guest AgentCtl tokens do not authorize Host operations. Terminal publication
clears the record and the owner removes its own socket. Embedded runs with
retained storage publish the same discovery record; runs without it are visible
to the CLI only through durable state.

Live status, kill, VM pause/resume/offload, execution checkpoint/suspend and live
inspect connect to the recorded owner. Operations execute in the binary that
started the Job. Compatible protocol versions allow a different caller binary;
an incompatible Job protocol reports that owner's binary without blocking other
Jobs. The socket accepts no terminal descriptors. It uses the existing correlated
Host envelopes and newline JSON codec, with a 1 MiB frame limit and at most eight
accepted connections. Existing native operation locks serialize transitions.
The process-local endpoint table is only an owner cache.

The runtime still retains `RunControlServer` for Overlay status/observations and
legacy inspect, plus `execution.sock` for native capture. Owner requests can still
call these internal sockets in their own process; the TUI uses Overlay
observations. Consolidating them requires migrating callers and preserving
capture receipts/locks. See [existing channels and cleanup order](../../docs/src/en/design/architecture.md#owner-internal-control).

Stopped status, review, apply, drop and inspect execute in the calling process.
Checkpoint restoration/fork starts a new owner directly. Durable operations take
exclusive `flock` on the Job root's `job.lock`: same UID, `0600`, one hard link,
no symlink. Its diagnostic record names the lock protocol, PID and operation;
unknown disk schemas refuse mutation. Different compatible binaries share this
lock. Residual lock metadata is cleared only after its process has died and no
valid live owner remains. Apply/drop refuse a valid owner that still holds stage;
the Attempt lease remains an additional fence. Read-only inspect uses the stage's
ordered lower-mutability declaration; absent or rebound declarations use Mutable.
Read-only sharing does not guarantee freshness.

The frontend verifies its loaded image before self-executing the owner. Linux
hashes `/proc/self/exe`; macOS checks the loaded Mach-O UUID against the opened
executable before hashing. This is not byte-for-byte loaded-memory attestation.
macOS retains its existing birth-identity/polling limitations: numeric signals
are not atomic pidfd operations, and already-reparented orphans missed by
discovery are not guaranteed contained. Moving the listener does not establish
new macOS isolation guarantees.

Lost responses, timeouts and cancellation may follow accepted effects.
Record-based CLI control reports uncertainty and does not automatically retry.
The explicit `--vm-socket` debug path still uses the older asynchronous exchange:
Ctrl-C does not interrupt its response wait, and disconnect errors may omit that
warning. Unifying this path is pending. Reconcile state before
resubmitting; scoped checkpoint receipts are not universal exactly-once delivery.

For upgrade, drain Jobs and stop the old `--pvisor-internal-host` with the old
binary. New CLIs never connect to `<generation>.sock`. Old records without
`live_control` expose disk state only; old running VMs are not automatically
discovered. The daemon supervisor listener, framing and admission are unchanged.

### Shared runtime implementation

`runtime/job_service.rs::RuntimeJobService` provides typed persisted-Job status,
review, apply/drop, workspace checkpoint mutations, native capture and execution
resume/fork operations without CLI argument or rendering dependencies. Request-local
`ServiceContext` hooks retain cancellation and exact Job/Attempt/generation fences;
mutations recheck admission under their leases. Capture rechecks before sending,
not after an effectful request has been sent. Scoped receipts and lost-response
ambiguity remain; this is not a universal exactly-once dispatcher.

Ordinary Job execution uses `RuntimeJobService::start_managed()` and
`ManagedJobRun`. The runtime retains the actual Attempt completion task, installs
the durable execution Job server for VM Attempts and publishes its completion
after native teardown. The server binds to the runtime's prepared record, not a
frontend selector or caller-supplied stage metadata. Non-VM Attempts retain their
Session-owned record/Bundle publication without creating an execution checkpoint
ledger. Managed VM starts require durable Run storage. The supplied `RunConfig`
is retained for execution restoration; it does not configure or replace the
caller's already-built `PVisor`.

Restored execution uses `RestoredAttempt::start_with()` to project the captured
environment and restore metadata, then enters the same managed completion path.
`ManagedRestoredRun` remains a compatibility alias for `ManagedJobRun`. Frontends
supply runtime configuration, terminal/cancellation adapters and rendering; they
do not install or finish the execution Job server. Failed handoff cancels and
drains the accepted Attempt; partially published Job state is retained for
reconciliation rather than forged into a completed receipt. Frontend wait errors
and early success cannot bypass native teardown or hide publication failures.
Dropping the managed run or its wait future requests cancellation; the completion
task retains publication ownership while its Tokio runtime remains alive. This
is not crash recovery or persistence after process/runtime shutdown. Resume/fork
do not report `Finished` solely because a launcher callback returned success.

Managed starts with durable storage also retain a private, versioned
`workspace-launch-policy.json` derived from runtime-resolved policy, not the
caller's restoration `RunConfig`. Workspace fork reconstructs supported network,
filesystem, resource and environment-projection policy without loading current
user/workspace defaults. It does not retain environment values or Gateway
credentials, and it is not an enforcement attestation. The reconstruction path
currently supports standard host execution with an OverlayNet proxy; legacy Jobs
without this snapshot and unsupported VM/container/custom executor, Gateway or
unrepresentable controls are refused before checkpoint/stage mutation rather
than silently downgraded.

Bundle validation checks identity and safety-summary consistency against recorded
executor observations; it does not authenticate the producer or prove artifact
contents. Wall-clock timestamps can move backwards and are not a monotonic audit
clock. Capture completion and replay tool receipts have narrower scopes than a
complete-run capture or an exactly-once external side-effect guarantee.

The lower-level `PVisor::run()` and `RuntimeJobService::start()` still return a bare
`RunHandle`. Embedded Attempt callers and daemon detached supervisors retain their
own lifecycle and resource ownership; they are not implicitly enrolled in the
managed Job/checkpoint server.

`AttemptService` is the shared in-process dispatcher for live status, termination
and native controls. Host VM and daemon supervisor endpoints adapt their own
command/authentication contracts to it. Termination requests cancellation, not
reaping, cgroup absence or resource-release proof. CLI `JobCommand` and daemon
Sandbox registry/lifecycle remain separate; Guest AgentCtl remains isolated.
The daemon does not acquire checkpoint/fork or apply/drop API support merely by
linking these services.

## Per-instance VM controls and memory CLI

By default every native VM Attempt gets a host-only Unix control endpoint, even
for embedded runs without retained Job storage. Embedded owners with their own
authenticated control endpoint may use `PVisorBuilder::instance_control(false)`;
a custom `control_socket` combined with this setting is rejected at admission.
Native control authority remains available through the handle/service. The daemon disables the extra instance endpoint because it already owns a
supervisor endpoint. Ordinary CLI controls discover the owner from the Job:

```bash
pvisor spawn --executor vm -- /bin/sleep 600
pvisor status last --json
pvisor suspend last --vm-pause
pvisor resume last
pvisor suspend last --vm-offload --vm-ram-file /private/vm-ram/offloaded.ram
pvisor resume last --vm-load
```

The live VM flags are command-local: `status` accepts only `--vm-socket`,
`--vm-job-id` and `--vm-attempt-id`; `suspend` accepts those identity flags plus
`--vm-pause`, `--vm-offload` and `--vm-ram-file`; `resume` accepts the identity
flags plus `--vm-load`. Place them after the command. Root-prefixed live VM
flags and live VM flags on other commands are rejected.

`--vm-socket` is a debugging override and requires explicit `--vm-job-id` and
`--vm-attempt-id`; a positional selector must match that explicit Job ID. Its
path must remain inside the private authority root. Normal commands need none
of these addressing options. Pause/offload are mutually exclusive;
`--vm-ram-file` requires offload. Live resume continues the same Attempt. When
there is no valid live owner, ordinary resume restores a sealed execution
checkpoint. Ordinary suspend captures an execution checkpoint and stops the
Attempt; `--vm-pause` pauses without capturing.
Pause stops vCPUs; offload uses the
stronger CPU/device quiescence boundary. Successful live replies are JSON;
typed Host errors and transport/parsing failures exit nonzero but need not
produce JSON. Non-VM controls are unsupported, not signal emulation.

Core's `host_protocol` defines `HostVmCommand` and `HostVmResult`; this crate
exports them with the Host envelopes and `host_vm_exchange` for embedded callers.
The exchange takes an `AgentCtlHostRequest<HostVmCommand>` and returns an
`AgentCtlHostResponse<HostVmResult>`. Successful CLI output contains the result's
`status` and `value` fields.

| Run CLI | `[vm]` field | Default |
| --- | --- | --- |
| `--vm-control-socket PATH` | `control_socket` | Unset: owner uses `/tmp/pvisor-host-<effective-UID>/jobs/<job-id>/<attempt-id>/control.sock` (canonical `/tmp`) |
| `--vm-ram-backing FILE` | `ram_backing` | Unset: attempt-local file in ordinary file-backed mode |
| `--vm-ram-compression[=BOOL]` | `ram_compression` | `false`; FUSE/macFUSE backing |
| `--vm-cold-ram-compression[=BOOL]` | `cold_ram_compression` | `false`; Linux x86_64 local cold pager |
| `--vm-ram-dedup[=BOOL]` | `ram_dedup` | `false`; best-effort host advice |
| `--vm-memory-pool SOCKET` | `memory_pool` | Unset; experimental Linux physical/sealed-compressed or Apple Silicon legacy pool |
| `--vm-node-socket SOCKET` | `node_socket` | Unset; same-host resource service |
| `--vm-snapshot-filesystem-pool DIR` | `snapshot_filesystem_pool` | Unset; Linux x86_64 no-network native checkpoint lower pool |

Boolean flags accept bare=true or `=true` / `=false`; omission preserves config.
True flags and explicit path options infer VM when `--executor` is omitted;
`=false` alone does not. Explicit executors are not silently replaced. Path
options override only their corresponding field. Custom CLI and embedded control
paths must remain under the canonical private Host authority root, with checked
`0700` ancestors. Existing sockets are never overwritten. The socket is removed
at termination and is never exported as guest discovery metadata.
`--vm-control-socket` selects the creation path; `--vm-socket` overrides discovery.
The command-local live addressing/action options are not `[vm]` fields.

The local cold pager has one combined reclaim/compression toggle; it conflicts
with dedup, file/FUSE backing, external pools, snapshot capture/restore, snapshot
filesystem pools and whole-VM offload. Dedup conflicts with both compression
modes and external pools. The snapshot filesystem pool must be host-owned,
outside VM-writable roots and snapshot stores, and on the Job's volume. Conflicts
apply after config/CLI merging. Existing storage/control tests and compressed
artifacts provide limited evidence, not end-to-end guest correctness or memory
savings; compressed exit still does not commit writes after the last resume.
See the [CLI commands and limits](../../docs/src/en/reference/cli.md#vm-instance-control)
and [configuration example](../../docs/src/en/reference/config.md#vm-control-memory).

## Cooperative model waits (opt-in Rust API)

With the `gateway` Cargo feature, an embedded native VM owner can install an
Attempt-local wait coordinator. It is disabled by default and is separate from
the `workload-aware-memory-offloading` observation experiment. The first version
optimizes one outstanding Host-observed model call per Attempt and requires a
one/two-vCPU guest-kernel idle handoff. Agent/client code needs no declaration.
Two vCPUs support pause/resume only, retain normal PSI/TX-watchdog behavior,
and need matching ABI-v3 firmware. Automatic offload remains single-vCPU.
Overlapping calls keep forwarding but disable automatic transitions for the
remainder of that Attempt. There is no new CLI/TOML switch, daemon integration,
or scheduler-budget release. `CooperativeModelWait` is the historical Rust name;
cooperation is between pVisor's VMM and guest kernel, not the Agent application.

```rust,ignore
use std::time::Duration;
use pvisor::{CooperativeModelWait, GatewayDriverConfig, ModelStreamBuffer, ModelWaitRounds, PVisor};

// `proxy` is the existing Gateway ProxyConfig; `vm` is a fresh single-vCPU VM.
let vm = vm.with_guest_idle_handoff()?;
let gateway = GatewayDriverConfig::new(proxy).cooperative_model_wait(CooperativeModelWait {
    require_guest_idle: true,
    downloads: false,        // opt into authorized proxy GET / CONNECT read waits
    filesystem_reads: false, // opt into Host-owned READ / read-only OPEN waits
    pause_after: Duration::from_millis(20),
    offload_after: Some(Duration::from_millis(50)),
    stream_buffer: Some(ModelStreamBuffer {
        max_bytes: 256 * 1024,
        max_wait: Duration::from_millis(250),
    }),
    rounds: Some(ModelWaitRounds {
        recheck_after: Duration::from_millis(100),
        offload_cooldown: Duration::from_secs(5),
    }),
});
let runtime = PVisor::builder()
    .gateway(gateway)
    .executors(vec![std::sync::Arc::new(vm)])
    .build();
```

These are example timers, not measured break-even thresholds. Both deadlines
start at each idle round; offload must not precede pause and timers must be
nonzero for pause and at most one day. `CooperativeModelWait::default()` uses a
100 ms pause grace, mandatory guest idle evidence, no offload, no stream buffering,
and repeated rounds with a 100 ms recheck interval and five-second offload cooldown. Setting
`require_guest_idle: false` is rejected; an Agent promise is not an alternative.
Enabled policy requires a VM and an enabled Gateway; a simultaneous custom
`model_wait` lifecycle or legacy `native_model_wait` selection is rejected before startup.

The Host enables observation per Attempt. Gateway recognizes authorized model
requests and reports their lifecycle automatically. No client header or explicit
rollout wait/resume call is required; this mode ignores and strips the legacy
`x-pvisor-inference-idle` header. The separate `native_model_wait` adapter keeps
its old explicit-declaration behavior; it cannot be combined with this mode. Model JSON/SSE is unchanged. Model waiting alone cannot
authorize pause: the patched guest kernel must hand off an idle vCPU and timer.
On overlap the old wait is woken and native cleanup finishes before responses
are delivered; new calls are not rejected for being concurrent. Automatic
optimization stays disabled for the remainder of the Attempt, even after the
calls become serial again. Manual pause still holds all delivery barriers.

Requests must traverse pVisor's model-aware Gateway, already bound by Host to
the target Attempt. Opaque TLS traffic or requests bypassing that Gateway do not
provide model lifecycle evidence. An external rollout's model request cannot be
mapped to an unrelated VM from timing or caller-supplied session headers. A
pVisor-owned per-VM route/binding is required; a general external-rollout binding
adapter is not implemented here. Training algorithms/losses and client inference
protocols need no change, but network routing and VM association remain prerequisites.

Entry only registers a timer: host upstream dispatch is not delayed by RAM
writeback. Fast replies cancel the timer. Longer waits pause vCPUs and optionally
use the existing `RunOffload { file: None }`; native executor setup permits
automatic offload only for shared file-backed RAM, excluding private/restored
RAM and cold/external pagers. Other configurations retain pause-only behavior;
native device checks remain authoritative. Offload completion includes host
backing publication, not just the runner's reclaim report.

For non-streaming responses, the Gateway barrier wakes at body completion or
failure. With `stream_buffer: None`, SSE keeps the first-nonempty-chunk boundary,
including heartbeat/reasoning deltas, EOF or failure. With `Some(ModelStreamBuffer)`,
the Host retains each raw batch until the first of: byte limit, fixed wait
limit, EOF, upstream error, or the coordinator's latched wake signal. Timer,
network/device activity, manual controls, overlap and cancellation still take
precedence over buffering. No header, model-client wrapper or guest application
change is needed. The maximum wait starts when successful streaming headers
arrive for the first batch, then when collection of each new batch begins after
its predecessor drains. It includes silence before the first byte and never
resets on a heartbeat.
It does not cover upstream connection/header latency or extend the guest lease.

`max_bytes` accepts 1 byte through 16 MiB; `max_wait` must be >0 and <=60 seconds.
These are Host limits, not measured optimal thresholds. The copy buffer contains
at most `max_bytes` payload bytes. HTTP/TLS transport memory, capture storage and
one already-received chunk's unread suffix are additional: an oversized chunk
ends buffering immediately without another upstream read. The suffix is retained
and replayed once after the copied prefix. This is not a total process RSS or
node-wide budget. Only the automatic wait receives this policy; passive overlap
fallbacks do not acquire additional prefix buffers.

Response headers/data wait for acknowledged resume. An already-started pause or
offload is allowed to finish (including Host backing publication) before resume;
therefore `max_wait` bounds collection, not end-to-end delivery latency. Each
batch crosses a resume barrier and uses the existing translation, capture and
backpressure path. While a batch is queued, a delivery hold prevents rearming.
An internal marker acknowledges consumption of all prior output queue items by
the HTTP body writer; it is not a TCP ACK or proof of guest application consumption.
After that marker, collection restarts and the coordinator may recheck guest idle.
The request guard stays alive through EOF and final batch drain. Stream drop,
shutdown or errors cancel it. With buffering disabled, the first chunk resumes
the VM and the delivery hold stays through EOF (no pauses between raw chunks). Buffered bytes precede a subsequent stream error;
errors before any bytes retain normal pre-body failure handling. No bytes or SSE
semantic events are discarded, but transport chunk boundaries may change.
Heartbeats, reasoning, visible output and tool-call deltas are all delayed alike;
there is no semantic filtering. Rebatching requires `rounds: Some(...)`.
EOF ends buffering; an SSE `[DONE]` marker alone does not bypass the size/time bound.
A real guest timer or external event may wake earlier than `max_wait`.
Wall-time deadlines and guest clock semantics remain unchanged. Reclaim reports
are best-effort residency observations, not proof of node capacity or density.

With repeated rounds, timer/device wake retires only the current round. If the
same request remains eligible and no delivery hold is active, the coordinator
registers the next listener and uses atomic resume-and-arm: VMM retires the old
claim, arms fresh acquisition and only then releases devices/vCPU. The fresh
kernel offer still gates pause; no old permission or timer deadline is reused.
This path does not wait `recheck_after`/`pause_after` before detecting the next
idle entry. Initial acquisition, failed acquisition and post-delivery rechecks
retain those delays; `offload_after`, the real-deadline minimum and offload
cooldown remain unchanged. Old listener retirement completes before the next
transition. Delivery racing that transition cancels the published new round and
waits for its cleanup; native transitions are never dropped halfway. The one-second
limit now bounds acquisition only. Accepted handoffs use the real guest timer
or explicit no-timer state; there is no fixed periodic resume. `recheck_after` must
be 10 ms..60 s. `offload_cooldown` must be >= that interval and <= one day. It is
measured from completion of an automatic offload attempt (including rejection)
and persists across model requests in the Attempt. During cooldown rounds may
pause, but skip offload; they reconsider it in a later round. These defaults are
policy choices, not measured performance thresholds. `rounds: None` retains
single-round behavior for comparison. Manual control, overlap and request
cancellation stop automatic rounds, rather than permitting an automatic retry.

Explicit controls through Run handles, AttemptService and the Host VM endpoint
share the coordinator. Human pause/offload holds delivery until explicit resume;
resume invalidates the old wait timers. Checkpoint/save-and-stop conflicts with
an active wait or a manual pause. Invalid primitives are rejected before effects.
Control failures after admission conservatively cancel the Attempt because a
generic error can follow a native effect or failed event publication. Request
drop/shutdown cleanup is latched and runs on the Session runtime, independently
of the Gateway runtime. Attempt cancellation/termination never triggers resume;
this is live-process coordination, not restart or crash recovery.

Coordinator tests cover timer/ownership/cancellation races and real local HTTP
through Gateway with a simulated VM backend. They do not establish real-guest
RAM correctness, reclaim amount, density gains, or performance thresholds.

### Other Host wait sources (experimental)

`downloads: true` observes authorized proxy GET waits for upstream headers and
body chunks, plus established CONNECT downstream reads (including opaque TLS
bytes). Each ready/error/EOF crosses the same resume barrier. CONNECT has a
16 KiB read buffer; HTTP uses transport chunks and backpressure. This does not
transfer guest TLS, decompression, hashing, package installation or arbitrary
background computation to Host. Direct socket downloads bypassing the proxy
are not covered. No new HTTP header or Agent integration is required.

`filesystem_reads: true` observes VMM Host-owned READ/read-only OPEN operations,
including lazy image retrieval and read-only baseline fingerprinting. Host
finishes the backend fetch while the guest may be offloaded, then signals wake
before touching guest RAM. This covers a running VM's lazy reads, not initial
image preparation before a VM exists. Cached/short work normally ends before
pause grace. Mutations/copy-up retain their existing RAM ownership.

All three hints feed one Attempt-local coordinator and the same kernel idle
handoff, epoch fences, native pause/offload/resume, cooldown, manual ownership
and termination paths. A hint alone never proves idleness. Filesystem observation
skips reservation while another wait owns the coordinator; overlapping explicit
network/model waits conservatively disable automatic transitions. Policies are
opt-in Rust API fields, default false for these new sources; there are no new
CLI/daemon controls or per-source timing thresholds in this revision.

## Resource ownership and local cold compression

`pvisor-daemon serve --sandbox --memory-pool` enables the daemon-owned experimental pool;
the daemon starts or reuses its detached `pvisor-daemon memory-pool --directory DIR`
component. Linux `--memory-pool-mode compressed` selects shared sealed compressed
objects instead of default physical-page sharing. Compressed clients retain
validated handles and local recovery after pool loss, while new publication
stops. Physical/legacy pool loss still fails dependent VMs. API restart does not
provide pool-process or host-reboot recovery. Reserve pool/host overhead outside
sandbox admission limits. See the [daemon boundary](../pvisor-daemon/README.md).

Physical pool publication computes the complete page identity once per incoming
page. Existing slots are compared byte-for-byte through the owner's read-only
mapping, under the pool mutex, instead of being read and hashed again. Only the
owner can write its size-sealed backing; referenced slots cannot be reused.
Client publication validation, live-RAM revalidation and checksum-verified
restore remain required. This removes redundant server work without treating
a matching digest as proof of equal contents.

Pool sessions also maintain bounded per-object local reference counts as PUT and
RELEASE complete. STATS reads these counts through weak object observers, without
rebuilding an index of every token. Local aliases are not counted as cross-session
sharing; weak observers do not keep payloads alive or change pidfd pinning.
The index is bounded by the existing per-connection reference limit and is removed
with the last local reference. Live cross-session counts remain approximate under
concurrent activity in other connections, as before.

The `pool_memory_case` engineering helper runs the same static guest byte oracle
as the Firecracker memory control through `pvisor-vm::api`. Its parent owns the
four-VM resource budget and sampling. It selects off/KSM/physical/sealed/local
mechanisms explicitly; it does not use a periodic filesystem heartbeat. The
optional `--libraries` attachment uses the existing caller-owned immutable DAX
contract, with `--buffered-libraries` selecting the same window and ordinary
guest reads. Library mode requires off and a Python rootfs/script; it does not
automatically promote mutable rootfs paths or enable DAX for normal VM launches.
The separate `--rootfs-library` mode loads `/libraries/libpayload.so` from the
ordinary overlay rootfs and accepts the usual memory modes. It needs the same
Python script/rootfs, conflicts with `--libraries` and does not attach a DAX
window. This exercises the physical pager's successful-file-READ hints through
the normal rootfs path, with full payload and private-write validation.

`pvisor cache` provides `prepare`, `publish`, `list`, `stat` and `read`;
`pvisor-daemon serve --cache` serves the cache. Node runtime protocols own immutable mounts and snapshot
RAM with same-user authorization, compatibility checks and connection pins.
The daemon has no node acquire/release adapter. Embedded callers retain explicit
resource ownership and must release consumers before backing owners.

Linux x86_64 also has default-off experimental instance-local live cold
compression: `VmSettings.cold_ram_compression` / `[vm].cold_ram_compression` or
`pvisor spawn --vm-cold-ram-compression -- COMMAND`. The flag selects VM execution;
the runner automatically starts the runtime-owned userfaultfd pager over private
anonymous ordinary RAM using `LocalColdRamStore`, without a live backing file,
FUSE or a service. This is separate from `vm.ram_compression` (FUSE backing).
Kernel-fault syscall or `/dev/userfaultfd` authority is required; compiled support
is not permission, and missing authority fails startup without fallback or global
sysctl changes. Linux external `memory_pool` negotiates physical-page sharing or
sealed compression; only the latter uses the 64 KiB userfaultfd pager.

The store rejects raw/poorly compressed blocks, caps encoded payload at half
configured RAM and bounds object count by the configured 64 KiB block count.
Two quiescence windows capture/recheck bounded batches before discard; validated
`UFFD_COPY` restores refaults. Guest execution continues between windows without
guest application participation; this is experimental eviction/refault probing,
not ordinary pause or a read-access heat detector. Admission rejects dedup,
file/FUSE backing, snapshot capture/restore and whole-VM offload combinations.
See [local compression](../../docs/src/en/design/memory-optimization/compression-local.md)
for user-specific device ACLs, restricted mappings/build features and ownership.
This delivers an experimental mechanism, not a production-density claim. Sealed
pool handoff and service-exit recovery have process-level tests; guest UFFD
recovery and production latency/density need separate validation.

The daemon's VM-only `NativeRuntime` embeds this crate in detached supervisor
subprocesses. Its CLI uses explicit flags.
The daemon API has no stage/apply or checkpoint/fork implementation. The daemon
is a separate executable linking `pvisor` and `pvisor-core`; synchronous internal
VM dispatch runs before Tokio. Packaging does not supply or validate the
prepared-image bootstrap, SDK conformance or density.

## Independent container VM lifecycle

On Apple Silicon, a daemon started with `--memory-pool-mode physical-pages`
provides native 16 KiB physical slots through the same pool socket. The runner
negotiates this protocol and starts the VM-owned physical pager instead of cold
compression. Publication rechecks complete live bytes under the CPU/device
barrier; reads retain sharing and writes COW. A kqueue watch keeps disconnected
slots pinned until peer exit. This is a separate fixed pool mode, with the same
pool-loss termination contract. It uses an unlinked file/cache, not memfd seals;
only the trusted owner may mutate free slots. Whole-VM offload and snapshots
remain incompatible. See the daemon README for limits and native four-VM gates.

`docker::Create.shared_kernel_base` selects a private Apple Silicon boot-kernel
cache through `pvisor_vm::api::VmConfiguration`. Equal initial kernel bytes map
one read-only inode with private COW writes; other RAM keeps its normal backing.
Creation canonicalizes and persists the directory, excluding guest rootfs and
mount overlaps. The selector works independently of `memory_pool`; when both are
used, immutable kernel RAM is excluded from both cold compression and dynamic
physical-page merging. Baseline mappings
retain their descriptor after daemon exit/path removal. This does not merge
application RAM, enable snapshots/offload, or provide automatic cache GC.

`docker::Create.memory_pool` optionally selects the existing same-user daemon
pool; creation canonicalizes its private Unix socket and persists the selector
for later starts. Pool authority must be outside guest rootfs/mount trees.
Startup authenticates the peer before installing the shared platform pager.
Apple Silicon uses a fresh unlinked file-backed RAM mapping and the existing
HVF compressed pool; Linux retains anonymous RAM and negotiates physical or
sealed-compressed storage. No environment mutation or new VM pager is introduced.
Omitting the selector retains legacy container defaults. An unavailable pool
fails startup. macOS depends on the live pool for restore and fails closed on
pool loss; it does not inherit Linux sealed-local recovery. See the daemon README
for standalone startup, manager restart, budgets and experimental limitations.

The experimental `docker` module owns the user-private image catalog, pinned
container configuration, writable uppers, managed volumes and launch generation
records used by the top-level container commands. The CLI owns Docker argument parsing, rendering
and terminal adapters. This profile does not convert ordinary Jobs or alter
stage/apply, capture or existing daemon APIs.
`Store::pull_with_progress` and `create_with_progress` emit synchronous OCI
preparation observations for frontend rendering; existing `pull`/`create` callers
remain silent. Download counts use received compressed bytes, and layer readiness
follows verification and extraction rather than merely reaching an expected size.
`pull_for_platform_with_progress` selects `linux/amd64` or `linux/arm64`
(`linux/arm64/v8` is an alias), independently of the host. Image records retain
their canonical platform; legacy records default to native because previous
versions only pulled native images. A reference holds one platform, replaced
atomically on successful pull; existing instances keep their pinned image.
`Create.platform` optionally asserts the native platform. Creation and start
reject foreign images before publication or VM launch; no CPU emulator is supplied.

New Docker pulls and completed builds default to the local immutable cache pool
under `PVISOR_DOCKER_HOME/images/pool-v1` (beneath the default user-local
`pvisor/docker` directory when unset). They reuse the cache's V1 file/metadata
index, authenticated fixed revision, conditional publication and direct VM
backend. They do not retain a complete extracted rootfs or OCI layer archives.
Original OCI configuration bytes are retained separately under `images/configs`
so subsequent builds preserve configuration such as labels and SHELL. A repeated
pull still resolves the registry manifest/config, but a matching catalogued pool
revision skips layer download and extraction. Pulls of the same reference/platform
serialize without holding the catalog lock across network I/O.

`Image.cache` binds the pool location and immutable handle; its legacy `rootfs`
field is empty. Embedded consumers call `Store::open_image`, retain the returned
`ImageRoot` through all OverlayCore/VM access, and use its `rootfs()` as the lower.
That path is a temporary metadata/sparse-file projection: native host reads must
not bypass the filesystem backend. Ordinary VM reads fetch only needed content
blocks, with bounded decoded memory caches and no persistent raw block mirror.
Copy-up may materialize a whole file in the projection before copying into upper;
normal projection teardown removes it. Legacy records without `cache` continue
to use their existing directories, including already created containers. No
existing image directories are automatically deleted or migrated.

Pool data is deduplicated by raw content SHA-256 across files, paths and images.
Each existing V1 content chunk (at most 1 MiB) is independently encoded using
Zstd level 3 when it saves more than 64 bytes, otherwise raw, in a versioned
13-byte envelope containing magic, codec and decoded length. Reads bound input
and decoded size, then verify the raw digest before returning bytes. Metadata
remains uncompressed for indexed range reads. `pool-format.json` and the internal
`pool+file://` binding identify this separate physical encoding. Its physical
`format.json` also rejects ordinary raw V1 readers/publishers; the pool adapter
supplies the unchanged logical index format. Ordinary
filesystem/S3 V1 prefixes remain raw and are not converted in place. This is not
the proposed small-file-pack V2 format. Current index entry/object limits remain.

Guest mmap uses the existing ordinary virtio-fs/page-cache path; this change does
not enable DAX or make the mutable physical projection Immutable. A cold small
read can decode an entire content chunk; warm-cache latency and mmap performance
need separate workload measurements. Pool compression/deduplication tests are
not Docker disk-use comparisons. Local pool imports explicitly reject arbitrary
xattrs other than the existing ownership/mode override rather than dropping
required attributes. On macOS, the exact host-only `com.apple.provenance`
attribute is ignored during import without removing or changing it on the source;
other attributes, including other `com.apple.*` names, remain rejected.
File ownership, modes, timestamps, symlinks and hardlinks
retain the cache contracts.

Import extraction and intermediate build stages are temporary and are removed
on ordinary success/failure. Abrupt process death can leave temporary directories.
Pool data/revisions remain after `rmi` because stopped containers may reference
them; automatic GC and disk-cache quotas are not implemented. Failed publication
does not replace existing catalog tags, but can leave unreferenced pool objects.

Each launch self-executes a detached owner and a separate native VMM. Embedding
applications using `docker::Store::start` must dispatch
`docker::internal_if_requested` at process entry, before threads or normal CLI
parsing. Lifecycle mutations use the catalog lock, and both owner and VMM hold
independent lifetime leases. A private inherited pipe terminates the VMM when
its owner dies. Stop escalation goes through the current owner and generation;
lookup and force-stop publication share the removal lock, with no lock held
while waiting for termination. This prevents `--rm` teardown from racing a
force-stop record into a deleted instance. No command signals a host PID saved
in metadata. VM setup errors and workload
exit codes remain visible in completion records and instance output.
Concurrent starts join the same starting generation and wait for its bounded
readiness or failure; they do not spawn a second owner or reject a healthy
in-progress launch. Waiting never retains the catalog lock.

Published TCP relays cancel the guest tunnel on client reset or failed input
writes, releasing the bounded connection slot without waiting for the guest
application to time out. Orderly client write-half close still forwards EOF and
allows the complete guest response. Completion cancels blocked companion pumps
before joining them; cloned descriptors must not retain finished tunnels.

The host translates Docker defaults into generic guest process/mount options,
including the Docker capability bounding set and `TERM=xterm` for TTYs unless
an image or explicit value/unset wins. The guest has one launcher and
PID-1 reaper shared by Jobs, the initial workload and exec. An optional host-CID
vsock adapter adds same-VM exec, stdin, signal delivery and TCP publication;
it owns no separate process lifecycle or host filesystem/lifecycle authority.
Ordinary Job configurations retain inherited identity/session/stdio defaults.

The Docker network resolver lower keeps its host directories private (`0700`)
while carrying the image's guest ownership and permissions for `/` and `/etc`.
An absent image `/etc` uses guest root ownership and `0755`; the injected resolver
file is guest-root-owned `0644`. This prevents DNS setup from blocking non-root
`run -u` and `exec -u`, without widening image directory permissions or host access.

`docker::build` constructs native OCI images without a build daemon. Each RUN
self-executes the existing container VM path under a private build store, with no
user mounts, and is killed/reaped before host-side COPY or export. An inherited
pipe terminates the VMM if the frontend dies. Context capture uses directory
file descriptors and no-follow opens; destination links resolve against the
guest root. Completed stages temporarily export the merged OverlayCore view as a flat OCI
layer and reimport it. Final publication commits the shared local content pool
before replacing catalog tags; temporary layouts/rootfs trees are then released. This reuses logical guest ownership/mode without installing host
setuid bits. Context capture and each COPY preserve regular-file hardlinks,
including links spanning directories or separately selected sources, without
linking the private snapshot back to host source inodes.
Rootless Linux exports private host-user-owned symlinks as guest root in OCI
output, without chowning host files. Non-root symlink ownership is rejected on
hosts that cannot represent it; this is not full guest symlink-owner support.
Failure never publishes a partial stage. Unsupported special files
and file capabilities fail explicitly. See the CLI README for the Dockerfile
subset, lack of instruction caching and platform validation limits.

The supported CLI profile and its explicit differences from Docker are in the
[application README](../pvisor-cli/README.md#docker-command-profile-experimental).
A passing catalog or protocol test does not establish Linux/KVM acceptance,
Docker SDK conformance, database durability or performance.

## Source and application boundary

Runtime APIs are exported explicitly from `src/lib.rs`: `PVisor`, Session/Attempt
handles, configuration and feature settings, `job_service`, durable Job/checkpoint
records, transport, and filesystem review primitives. The implementation module
`runtime` remains private. The runtime library has no Clap dependency and does
not depend on `pvisor-cli`; application enums are parsed by app-local Clap
adapters. Clap is retained only as a development dependency for standalone
runtime demonstration/measurement examples.

Journal storage and trace production use `pvisor_journal::api::{JournalStore,
TraceProducer}` alongside the opaque `Journal` and `Trace` owners. The existing
`trace` module explicitly re-exports these contracts; trace identity is immutable
and supplied through `Trace::with_id` when it must match a Run. Durable filesystem
barriers call `Persistence` through `DurableFiles` directly, while runtime JSON
publication and Run persistence diagnostics retain their own helpers.

Application sources live under `../pvisor-cli/`: `src/cli/`, `src/companions.rs`,
`src/tui/`, and three entries in `src/bin/`: `pvisor`, `pvisor-tui`
and `pvisor-replay`. Daemon pool lifecycle belongs in
`../pvisor-daemon/src/memory_pool.rs`; node protocols remain in `src/node.rs`
and `src/node/`. Feature listing and cache argument
parsing also live in the application. Executable-dependent integration tests
live in its `tests/`; runtime-only tests remain here. Tests combining command
execution with runtime APIs link both crates from the application test suite.
The detailed command, platform, storage and memory limits above remain relevant
to embedded callers where they describe runtime behavior, and to the installed
application where they describe frontend behavior.

## Lazy image V2

Lazy images keep the existing immutable published-image format and handles.
V2 optimizes the private host reader; it does not change OCI contents, guest
commands, copy-up/export ownership, or the physical-lower stability declaration.

- Capable TCP/Unix services advertise optional `metadata_pages` in Prepared.
  The client reads the existing 64 KiB binary B+tree and file records locally,
  including permissions, ownership and symlink metadata, instead of issuing
  Stat/List RPCs. The pinned COMMIT authenticates the checksum catalog and pages;
  only eight immutable metadata basenames are readable, with bounded ranges.
  Legacy JSON response receipts are neither trusted nor written in this mode.
  Invalid metadata propagates an error without silently falling back to RPC;
  invalid local binary cache entries may be refetched and authenticated.
  Authenticated raw pages and decoded B+tree nodes share a 256-entry LRU.
  Nodes are validated once per residency rather than decoded on every lookup;
  raw/node allocations remain budget-charged through their last active reference.
  Persistent disk caches have no aggregate eviction quota. File-content reads remain unchanged. Initial immutable-object,
  header and root-page reads are synchronous: runner lower reconstruction happens
  before Linux user-namespace setup, which requires kernel-level single-threadedness;
  joining temporary reader threads is not a safe substitute.
- Services without the capability, older private bindings and non-socket
  transports retain the RPC path. On the second uncached child lookup in a
  directory, fetch up to two validated directory metadata pages. At most 64 directories are admitted per backend
  lifetime, and only eight pages remain resident. Retained nodes and exact
  persistent positive/negative metadata hits take precedence over prefetch.
  Complete inventories can establish absence; partial, evicted or failed
  inventories fall back to exact stat. Looked-up attributes and proven negative
  results are persisted under the immutable metadata identity for remounts.
  Prefetch never downloads file content or projects speculative guest names.
- Remote list continuations are opaque and separate from local directory/FUSE
  cookies. Resume after page eviction replays from a retained boundary or zero;
  local ordinals are never passed to the portable B-tree as remote cursors.
- TCP/Unix readers negotiate framed protocol 2 with harmless Ping requests and
  reuse up to four connections per client. TCP uses NODELAY on both endpoints.
  Idle client streams expire after four seconds, before the server's five-second
  idle timeout. Authorization is checked on every envelope; bodies remain
  length-bounded and SHA-256 verified. Fully framed application errors preserve
  their error kinds and permit reuse; framing, transport or integrity errors
  discard the stream. Ordinary cache clients do not replay failed actual requests;
    the private bridge's pinned read clients have the bounded exception below.
- Protocol 1 remains one-request-per-connection. A V2 client falls back before
  sending the actual request when an old service rejects the version or closes
  the harmless handshake. Authentication failures, malformed replies and
  handshake timeouts are not downgrade signals. Filesystem/S3 object transports
  do not use the socket pool; metadata prefetch still applies.
- The service admits at most 64 connections, separates their framing/idle waits
  from 16 file workers and two preparation workers, and bounds each pending
  request queue to 16. Saturation returns a busy error or closes an unadmitted
  connection; callers must handle these failures. Idle connections and long
  preparations do not occupy file workers.

The Linux private host-network bridge accepts envelope 1 (one exchange) and
2 (sequential persistent exchanges). It admits at most 32 connection handlers,
separates frame/idle/write waits from four execution workers, and bounds queued
requests to 16. Queue saturation returns a framed busy error; unadmitted
connections close. Each exchange is confined to the pinned immutable handle
and metadata whitelist. Bridge V2 is enabled by default. On both bridge hops,
private clients retry an exact-handle Stat/List/Read or whitelisted Metadata at
most once on EOF, broken/reset/aborted/disconnected transport or connect refusal.
The failed lease is released, and retry uses a fresh stream (negotiated V2, or
already-established V1 fallback). Pool wait, connect, handshake and body I/O
share the original per-hop deadline. Timeout, busy/not-found/auth errors,
malformed frames/digest declarations and verified-body digest mismatches are
not retryable. Prepare/Open and ordinary clients retain no-replay semantics.
Old private bindings without retry context default to no replay.

The allowance is per hop, not a propagated end-to-end budget: failure on both
legs can produce up to four upstream read attempts. A downstream timeout does
not cancel already queued/executing upstream work, whose outcome may be unknown.
Only immutable read operations qualify; no mutation is replayed. Detached bridge
threads live in the dedicated child process, never the pre-namespace VM runner.

For private engineering A/B, `PVISOR_LAZY_BRIDGE_V2=0` retains the previous
single-exchange bridge and socket-worker scheduling independently of upstream
V2. Optional `PVISOR_LAZY_BRIDGE_METRICS_DIR` must name an existing same-user
private directory. Bridge-created shared counter files contain only aggregate
counts/timings, not credentials or image paths; read after process teardown.
Live snapshots are approximate, killed-process gauges may remain nonzero,
and files are not reboot-durable. The benchmark caller owns file cleanup.

V2 is enabled by default. For engineering comparison only, set
`PVISOR_LAZY_IMAGE_V2=0` before constructing the client and VM runner to disable
client index pages, metadata prefetch and connection reuse; other values retain
V2. Set `PVISOR_LAZY_INDEX_PAGES=0` with V2 enabled to retain the previous
RPC/prefetch path and pooling. These are private
rollout/benchmark overrides, not a persisted image-format version or CLI feature.
Both A/B modes retain the shared pagination and correctness fixes. The inherited
host environment carries the setting through private runner reconstruction.
See the [archived same-artifact NumPy A/B](https://github.com/DeepLink-org/pvisor/blob/555e750105e8072e0b85030dfb36226c66dae206/benchmark/pvisor/lazy_image_v2.py); historical
Docker measurements are a different cohort. Contract tests cover real portable
pagination, negative-cache remounts, bounded prefetch, Unix/TCP framing, legacy
fallback and concurrent service progress. Linux KVM performance measurements do
not establish macOS/HVF, live S3 or WAN coverage.

## Physical lower stability (explicit experiment contract)

Embedded callers may set `OverlayHint::lower_mutability` to a per-physical-lower
`Vec<pvisor_overlay_core::LayerMutability>` in `lower_dirs` order. Empty defaults
to all mutable. Attempt preparation validates nonempty length and exact canonical
identity/order against the final normalized stack; promises cannot silently
transfer to inserted targets or frozen copies. Host mounts receive the declarations
through `OverlayMountConfig`; native VM handoff serializes `lower_mutability`
alongside `lowers`, retains it in `OverlayDeviceSpec`, and projects it into
`pvisor_vm::api::OverlayConfig`. Legacy runner metadata omitting the field defaults
to mutable. Restored/rebound copies are not automatically promoted. The enum
lives only in overlay-core; no duplicate runtime contract is introduced.
New low-level host/VM mount APIs default their lowers to Immutable. The runtime
expands unspecified transparent rootfs/live workspace declarations to explicit
Mutable at handoff, preserving external-edit visibility and legacy restoration.
Authenticated prepared bases retain their explicit Immutable declarations.
When VM networking prepends the attempt-owned synthetic resolver lower, it prepends
an immutable declaration to an explicit mutability vector as well. Existing
declarations keep their original physical-layer association. Legacy empty vectors stay empty.
Resolver preparation tests exercise the production preparation function and VM
builder validation, including disabled networking and local write failure, without
booting a guest or requiring firmware.

**No automatic image/frozen promotion in this first version.** Host rootfs and
arbitrary prepared `--rootfs` directories are mutable. OCI extraction uses
content-addressed atomic publication (`image/oci.rs`) but retains writable host
permissions and does not enforce a lifetime lease against external changes.
VM lazy images have immutable remote logical metadata, but their physical
projection (`image/cache/direct.rs::Projection::project`) creates names/links and
updates metadata on demand; that projection is not a stable physical lower.
Host lazy read-only mounting alone is not a lifetime stability proof. Frozen
baseline selects observation semantics, not an immutability guarantee. All
these paths therefore conservatively retain mutable declarations; an owned,
externally stabilized experimental lower can use the explicit contract.

For a sealed root lower, embedded callers can explicitly transfer a
`SnapshotBase` from `SnapshotStore::import_base` into
`VmExecutor::with_rootfs_base`. The configured `VmSettings.rootfs` must name that
exact canonical generation; a different directory or restored executor is
rejected. The executor and its clones retain the base GC lease through native
teardown. Ordinary rootfs paths and `rootfs_immutable` do not opt in. The owned
store must remain private and unmodified by external writers. An attempt-owned
synthetic resolver lower, when needed, is fully written before VM entry and
retains its owner through exit; its declaration preserves the lower stack's
identity and order. Writable uppers retain their normal behavior.

Ordinary VM CLI runs can select a durably prepared physical rootfs independently
of the live workspace:

```bash
pvisor cache prepare-rootfs --rootfs /opt/rootfs-source --base-store /opt/pvisor-bases
# Use the prepared_rootfs.base.id from the JSON result:
pvisor spawn --vm --rootfs-base-store /opt/pvisor-bases --rootfs-base BASE_ID -- /bin/sh
```

The source must be quiescent and independent of the store. Initial preparation
copies and fully verifies its content/native metadata; external hardlinks and
special objects fail. `SnapshotStore::prepare_rootfs` imports and durably pins
the generation; `retain_prepared_rootfs` idempotently pins an existing owner from
the same store. The private catalog atomically publishes a versioned record and
a hardlink to the base reference marker. No removal API is provided: these pins
survive preparer/process exit, GC and stopped Jobs until a future controlled
deletion mechanism. A saved JSON path/id alone does not retain an ordinary base.

`open_prepared_rootfs` checks the bounded record, pin identity and small seal,
then acquires a lease without a full tree audit. External store mutation is
unsupported; use `SnapshotBase::verify` for an explicit audit. CLI Job owners
open before resolving their layout; `VmExecutor::new` also consumes the typed
`VmSettings::prepared_rootfs` selector for SDK callers and retains the owner.
Its optional resolved `rootfs` must match exactly; `image` conflicts. A restored
executor rejects the selector override. This source does not support native
execution checkpoint capture/restore: capabilities report capture unavailable,
Job capture admission rejects it, and no native capture binding is installed.
Ordinary workspace logical checkpoints retain their separate behavior.
Native runners independently re-lease
the base before their sandbox and retain it until teardown.

Configuration uses `[vm.prepared_rootfs]` with `store` and `base = { id = "..." }`.
The two CLI options are required together and conflict with `--rootfs`; host,
container and JSON `--spec` reject them. Store/workspace/stage/extra guest sources
must be independent canonical trees. The store must not be guest-visible or
writable. Only its selected rootfs subtree becomes an Immutable lower; workspace
snapshot/version semantics remain separate. Legacy configurations default to no
selector; host `/`, arbitrary paths, OCI caches and lazy projections retain their
original declarations. Lower-data caching and writeback/DAX are not enabled by
rootfs selection.

On Linux x86_64, a fresh native launch with an authenticated prepared-rootfs
selector, its matching retained `SnapshotBase`, and no capture/restore binding
requests read-only local-FLUSH elision when
`PVISOR_VM_FS_READONLY_FLUSH_ELISION` is unset. This preference is applied only to
that native child, without changing the Host environment or generic VM worker
default. Explicit `0` disables it; explicit `1`, invalid values and non-UTF8
values pass unchanged to the existing strict worker parser. Other platforms,
ordinary rootfs/Immutable declarations, and SDK callers using only
`with_rootfs_base` retain the unset-off behavior and their existing checkpoint
capabilities. SDK callers consuming the authenticated `prepared_rootfs` selector
share its existing capture-unsupported contract and this scoped Linux default.
Actual negotiation still requires the guest capability, guest cache, WB0/DAX0
and compatible lock support; each successful OPEN still checks Core policy and
the actual read-only Immutable regular lower descriptor's native filesystem.
Upper, Mutable, writable and otherwise ineligible handles retain ordinary FLUSH.
Older guests fall back without the capability. Guest write/error barriers and
RELEASE remain unchanged; this default does not enable data caching or repair
unrelated page-mapping or writeback behavior. Launch-environment contract tests
do not establish additional platform or workload performance coverage.
Local CLI preparation rejects both source and destination paths that overlap
the canonical Host authority before any store initialization or copying, even
without an active Job owner. It never copies Host credentials into a base.

The runtime also exposes `SnapshotStore::prepare_workspace`,
`retain_prepared_workspace` and `open_prepared_workspace`, returning the existing
`SnapshotBase` owner. They import a quiescent workspace or retain an already
imported base in a separate `prepared-workspace` catalog. Its versioned,
purpose-bound record and hardlink pin survive preparer exit, GC and stopped
consumers. Rootfs-only catalog entries cannot authenticate as workspace entries,
or vice versa; a base may be explicitly retained in both catalogs. Existing
`prepared-rootfs` directory names and version-1 record bytes remain compatible.
Warm workspace opens authenticate the small record, pin and seal, without a full
tree audit; `SnapshotBase::verify` remains the explicit audit. Keep each acquired
lease until its consumers stop, and keep the private store externally unmodified.
Source and store must be independent canonical trees, and import retains all
existing content/native-metadata, special-object and external-hardlink checks.
These APIs provide storage ownership only: they do not add a CLI selector,
change workspace/stage/apply semantics, or enable VM cache experiments.

Embedded callers can transfer a prepared workspace owner into
`PVisorBuilder::with_workspace_base(base, live_target, Arc<VmExecutor>)`.
The actual native executor must own an authenticated prepared rootfs, and its
store must be independent of the workspace store. The builder selects and binds
that exact executor; replacing it with another executor or a transparent wrapper
is rejected at admission. Host `/`, arbitrary rootfs paths, restored executors,
compose layers, host-mounted merged views and existing workspace stages are
unsupported. Explicit durable storage and a fresh stage are required. Existing
canonical writable/control/Gateway paths must not overlap either private store.

The runtime retains the workspace and rootfs owners through the builder, admitted
Attempt and native teardown. Under the original RunLease, it creates a new compact
Core/stage with the sealed generation as its single Immutable lower and frozen
baseline; `live_target` remains the separate writable apply destination. This is
fresh initialization, not `ExecutionOverlayHint` restoration. Ordinary SDK RunSpec
inputs receive the trusted workspace/rootfs split without private VM metadata.
The actual native handoff checks the lower, baseline, target and declaration
against that owner. The child retains its existing independent seal/lease/content
index authentication; it does not add a second catalog-purpose wire protocol.

Reads stay on the selected generation while external edits to the live target
remain external. Mutations retain normal copy-up/preimage behavior, and applying
selected changes rejects conflicting target edits. This is not whole-read-set
transaction isolation. A private `workspace-baseline.json` records the exact
workspace/rootfs selectors and live target. Stopped-Job apply reauthenticates both
catalogs and their relationship to the record under the existing lease, retaining
both owners through commit; a receipt path alone is no proof. The initial version
rejects apply-target overrides before creating directories. General compose
restrictions remain. Workspace logical checkpoints remain distinct from native
execution capture/restore, which the required prepared-rootfs executor rejects;
workspace checkpoint creation/read-only review remain available, while logical
checkpoint restore/fork is refused before creating child backing. A retained
workspace-catalog owner also identifies this limit if the original stage is no
longer present. Native capture/fork/resume independently reject the actual owned
stage receipt, even if a supplied restoration config omits the selector.
Relaunch must explicitly reconstruct supported ownership, not promote saved
paths; a managed prepared-workspace selector without a matching runtime owner is
refused before starting an Attempt. This entry alone adds no CLI selection, automatic reimport, cache switch
or measured performance benefit.

`PVisorBuilder::on_executor_output(Fn(&ExecutorOutput) + Send + Sync + 'static)`
provides read-only executor observation without a wrapper. It runs synchronously
after an invoked executor returns and before any driver teardown or stage seal;
it does not run for admission failure or cancellation before invocation. The
callback must not block, panic or reenter runtime operations. CLI adapters can
retain their pre-teardown cancellation notifications while keeping the exact
native executor binding.

The `filesystem_owned_inputs` engineering example prepares independently copied,
verified rootfs/workspace generations once with `prepare --rootfs DIR
--workspace DIR --store NEW_DIR --manifest NEW_JSON`. Its `run` subcommand accepts
the offline VM argument subset used by `reference_baselines.py`. Set
`PVISOR_FS_OWNED_INPUTS_MANIFEST` to that manifest and
`PVISOR_FS_OWNED_INPUTS_CONDITION` to `baseline`, `rootfs`, `workspace` or `both`.
All conditions read the same sealed physical rootfs; its baseline declaration
remains mutable. Baseline/rootfs workspace controls retain the ordinary mutable
trial lower and live-baseline read observations. Workspace/both attach a sealed
copy of the same fixture with an independent trial workspace apply destination;
this treatment combines immutable caching and frozen-baseline observation
semantics. Each run owns a fresh upper, authenticates seals without a full
content rescan, and retains both leases until managed Job completion. Explicit
`PVISOR_VM_FS_WRITEBACK=0` is required; the normal VM overlay runner keeps DAX
disabled. The optional
`PVISOR_FS_OWNED_INPUTS_RECEIPT` selects a fresh JSON receipt path; otherwise it
uses `stage/owned-inputs-receipt.json`. The receipt records actual generation
identities, condition and manifest digest after teardown. This entry preserves
PVisor, managed Job completion, compact journals, Bundle generation and self-exec
VM isolation, but omits CLI frontend/owner orchestration. Its Ready timings are
comparable within this four-condition cohort, not to previous CLI Ready measurements.

For same-artifact/same-input A/B keep declarations unchanged and set
`PVISOR_DISABLE_IMMUTABLE_LOWER_CACHE=1` before constructing the serving host
process/VM runner for the no-cache control; unset for caching.
`PVISOR_FS_PROFILE=1` reports `overlay-core` hit/miss/eviction `units`; see
that crate's README for exact names and capacity. This is a metadata/path/parent
cache only: no directory inventory/content cache or kernel TTL change. Native
VM root devices now additionally retain guest data pages on read-only regular
OPENs from explicitly immutable physical lowers; workspace, upper and mutable
lowers keep their original OPEN behavior. This defaults to the `root-data`
profile; `PVISOR_VM_CACHE_EXPERIMENT=off` in the serving runner disables it.
This does not promote any of the mutable sources listed above or use
`VmSettings.rootfs_immutable`'s apply restriction as a stability proof. See
the [VM data-cache contract](../pvisor-vm/README.md#rootfs-data-cache-default).
The conditional default has no independent real-guest performance claim.
Upper/merged state must never be labeled immutable.

## Develop

```bash
mise run build -- release          # release build + macOS Hypervisor signing
mise run build    # debug build + macOS signing
mise run test -- pvisor            # runtime tests
mise run test -- pvisor-cli        # application/executable tests
mise run examples
```

On macOS, source builds that use HVF must be signed. `mise run build -- release` does this;
the equivalent entitlements file is `macos-hypervisor.entitlements`. The embedded
`pvisor-guest` supervisor is built as a static Linux musl ELF
with Rust's bundled linker. On Apple Silicon, install its stdlib once with
`rustup target add aarch64-unknown-linux-musl`. It launches workloads directly,
without a shell helper, and reports their exit codes through the VM runtime's root filesystem ioctl.

The Rust `pvisor-vm` runtime is linked into `pvisor`, with KVM on Linux and
HVF on macOS. Its implementation derives from libkrun components, but no separate
vendored libkrun `rlib`, `libkrun.so` or `libkrun.dylib` is required. The separate guest
kernel is embedded in Linux static musl builds; macOS loads `libkrunfw.5.dylib`
at runtime. Linux source builds require Zig, `cargo-zigbuild`, and
`rustup target add x86_64-unknown-linux-musl`. Use `mise run build` so target
selection and kernel preparation match release builds.

## Links

- [Operation and Event](../../docs/src/zh/design/operations-events.md): operation requests, actual rewrites,
  VM/Overlay placement, execution facts and boundary observations.
- [The PolicyVisor model](../../docs/src/zh/start/what-is-pvisor.md)
- [Get started](../../docs/src/en/start/first-run.md)
- [Isolation architecture](../../docs/src/zh/design/isolation.md)
- [Gateway architecture](../../docs/src/zh/design/gateway.md)
- [OverlayNet architecture](../../docs/src/zh/design/overlaynet.md)
- [pVisor CLI](../../docs/src/en/reference/cli.md)
- [System architecture](../../docs/src/zh/design/architecture.md)
- [`pvisor-overlayfs`](../pvisor-overlayfs/README.md)
- [`pvisor-overlaynet`](../pvisor-overlaynet/README.md)
- [`pvisor-gateway`](../pvisor-gateway/README.md)
- [`pvisor-core`](../pvisor-core/README.md)

### Host idle policy and regression probes

For the opt-in single-vCPU Linux/KVM executor:

```rust,ignore
let vm = VmExecutor::new(settings)?.with_guest_idle_handoff_policy(
    true, // disable proactive compaction; retain on-demand compaction
    std::time::Duration::from_secs(1), // minimum remaining timer budget for offload
)?;
```

Keep the Gateway wait configuration described above. The default
`with_guest_idle_handoff()` preserves compaction defaults and uses the same
one-second offload minimum. The threshold must be >2 ms and <=24 hours.
Short timer budgets keep pause-only mode without consuming offload cooldown.
Admitted backing writeback is non-interruptible and can overrun a deadline.
[Guest maintenance contracts and source tests](../../fw/README.md#experimental-pvisor-idle-handoff)
and [VMM handoff/atomic resume](../pvisor-vm/README.md#single-vcpu-guest-idle-handoff-and-optional-offload-experimental)
describe the safety boundaries.

Build `vm_idle_handoff` with `--features gateway` for single-round regressions
(`rounds: None`): `timer|short|network|race|boundary|busy 10`. It uses the same
rootfs/firmware/fixture environment as the repeated-round probe below and a fresh
`CASE_ROOT`. Both probes use a local provider, not an online model.

### Long-reasoning engineering probe

The streaming fixture supplies a normal per-candidate `finish_reason` before
`[DONE]`. Success requires both guest content/order/checksum checks and exactly
one Gateway `llm.response.stream`, with no `llm.call.cancelled`. Receiving all expected
text alone must not hide an incomplete provider protocol. Fault fixtures retain
separate negative expectations rather than weakening Gateway validation.
`vm_reasoning_rounds` exercises one ordinary model HTTP request inside a real
one/two-vCPU Linux/KVM guest, against a deterministic local provider. It needs
`--features gateway`, the matching idle-handoff firmware and a rootfs with Python.
The guest keeps and checks a 192 MiB payload, checks ordered streaming deltas and
reports timer intervals. A separate TCP channel records delivery ACKs solely for
measurement; it is not an optimization declaration or a production Agent API.

```bash
cargo build --locked --release -p pvisor --features gateway --example vm_reasoning_rounds
CASE_ROOT=/path/to/new-case \
PVISOR_CASE_ROOTFS=/path/to/rootfs \
PVISOR_CASE_VM_LIBRARY_DIR=/path/to/firmware \
PVISOR_CASE_VM_PYTHON=/opt/miniconda3/bin/python \
IDLE_FIXTURE_HOST=10.0.0.2 \
PVISOR_PROBE_RAM_BACKING=/path/to/new-ram-file \
  target/release/examples/vm_reasoning_rounds stream offload 12
```

Scenarios are `long`, `timer` (250 ms), `network`, `stream`, `race` (stream,
network and timer together), `short` (1 ms), `download` (ordinary proxy GET),
`tunnel` (opaque HTTP/1 CONNECT transport) and `image` (Host lazy cache reads).
`busy` (two CPUs) keeps the peer CPU runnable until delivery and requires zero
pauses/offloads. `offload-race` (offload policy) sends real network traffic
10/0/40 ms after reclaim requests whose latest observed lease exceeds
the configured minimum (repeating up to six injections); retained diagnostics determine
whether each wake preceded admission, raced writeback, or followed commit.
Image requires `PVISOR_PROBE_LAZY_SOCKET`, a compatible cache server serving the
fixture manifest and `/payload.bin`; see the engineering result bundle for the
controlled delayed fixture. Policies are `baseline` (no
lifecycle), `pause` and `offload`. Duration is 3..180 seconds. Each case directory
must have no existing workspace; the explicit backing file must not exist. If
omitted, backing is `CASE_ROOT/live.ram`. Choose and record the actual backing
medium; tmpfs cannot establish disk-offload savings. Run through the host's
existing approved pVisor execution profile where user namespaces require it.

The probe writes `events.json`, `delivery.json`, `result.json` and `summary.json`.
`delivery.json` uses a Host monotonic clock for provider enqueue to guest ACK;
that interval includes HTTP, buffering, guest faults and the ACK return path.
It is not isolated resume latency. Compare with baseline and pause-only runs.
Record backing residency and process RSS/PSS separately; the native reclaim
report uses `mincore`, not node capacity. The probe does not establish SWE/model
quality, concurrency throughput, density, or hard-real-time timer guarantees.

The `sched` scenario checks fair-task and timer progress during a bounded
SCHED_FIFO spinner and restores the temporary RT-throttling setting. `psi`,
`psi-late` and `psi-stats` cover pressure/trigger wake and timestamped statistics.
They use ordinary guest workloads without an Agent optimization declaration.

Host-only probe settings:

- `PVISOR_PROBE_CPUS=1|2` (default 1). Two CPUs keep normal PSI/watchdog;
  the two pinned CPUs independently verify the resident payload after delivery.
- `PVISOR_PROBE_DISABLE_COMPACTION=0|1` (default 1).
- `PVISOR_PROBE_OFFLOAD_MINIMUM_MS` (default 1000).
- `PVISOR_PROBE_BUFFER_WAIT_MS=1..60000` (default 250).
- `PVISOR_PROBE_STREAM_INITIAL_DELAY_MS=0..60000` (default 0).

The probe records the guest sysctl/boot command line, deadline skips and actual
pause/offload operations; short timers may correctly produce zero offloads.
Optional `PVISOR_DIAG_OFFLOAD=1` records admission, cooldown and backing phases;
`PVISOR_DIAG_IDLE=1` records epoch/offer-linked handoff, wake and resume transitions.
Watch notifications may coalesce. Ordered diagnostic stderr can perturb timing;
compare equally instrumented runs and retain uninstrumented controls.

### Idle control failure diagnostics

The private Host/runner idle channel retains its 3-second receive watchdog,
1-second write timeout and fail-closed runner exit. Failure diagnostics identify
side, phase, PID, timestamp and errno; runner status-write failures also retain
sequence, epoch, claimed state and elapsed write time. Host cancellation occurs
before asynchronous diagnostic publication. A blocked runner stderr pipe can
delay its fatal diagnostic by at most 20 ms before exit. This does not change
idle handoff, real timer/event wakeups, offload thresholds or cooldowns.

### Dual-vCPU idle handoff validation

`PVISOR_PROBE_CPUS=2` selects the dual-CPU payload in `vm_idle_handoff`.
Use `busy`, `ipi`, `timer`, `network`, `race`, `ingress`, and `boundary` modes with a fresh
CASE_ROOT per run and ABI-v3 firmware. `busy` pins the worker opposite the model
caller and keeps it runnable until the response; it requires zero pauses.
`ipi` exchanges messages between pinned CPUs and records RES/CAL interrupt
counts and wake latency. `timer` arms different deadlines on both CPUs;
network/race checks use the existing real NIC fixture. `ingress` sends real NIC
traffic at 0/10/19/20/21/25/50 ms without waiting for pause, exercising
arrival before and during acquisition as well as after publication. The guest asserts PSI is
present and single-CPU PSI/watchdog deferral flags are absent. All modes retain
payload hashing, complete responses, balanced transitions and no-offload checks.
These are engineering correctness probes, not live SWE or latency guarantees.

Build stages mark their private upper root with the base root's logical metadata,
so COPY ownership is honored during RUN with `--network=none` too. The physical
upper directory remains private (0700).

Managed volume directories retain guest-selected modes across reopen, including
image copy-up permissions. They must remain owned directories (never symlinks);
the enclosing store and volumes catalog remain private 0700 directories.

## Explicit same-host VM peer networks

Native VM Jobs and container VMs can join the same user-private network without
a central forwarding process. For Jobs, add this to each `spawn --config FILE`
TOML configuration, selecting a different address for every live instance:

```toml
[run]
executor = "vm"

[overlaynet]
mode = "auto"
policy = "public"

[overlaynet.peers]
network = "development"
address = "192.0.2.11"
```

A second member uses the same `network` with, for example, `192.0.2.12` and can
connect directly to a service listening on `192.0.2.11` or `0.0.0.0`. The Rust
API uses `NetworkDriverConfig::peer_network(Some(PeerNetworkSettings { network,
address, gateway, aliases }))`. `gateway` defaults to `192.0.2.1` in TOML;
`aliases` defaults empty. These explicit low-level groups are independent of the
Docker-style network catalog described below.

The low-level group name is 1–24 ASCII letters, digits, underscores or hyphens.
Explicit groups are scoped to the effective host UID; container network names
instead resolve through their store-scoped catalog to ID-based groups.
Directories live under canonical `/tmp/pvisor-host-<UID>/networks/<network>/`;
each address has a numeric member directory containing `net.sock`, `peer.json`
and a persistent address lease. This uses the existing host authority root,
which native executors exclude from guest projections. Discovery metadata is
not guest authority. The lease refuses duplicate live addresses; fresh packet
generations fence restarts, and an owner can reclaim stale files after a crash.
No periodic central registry service is required.

Membership explicitly grants bidirectional IPv4 TCP, UDP and ICMP to every
member of that network. It requires `auto` native networking, unrestricted
resolved network policy, no bandwidth budgets and the default controller.
Deny/allowlist/scoped policies, custom controllers, host/OCI executors and off/
proxy networking are refused rather than bypassed. Ordinary external egress
retains its existing TCP/DNS path. Peer ARP is targeted; IPv6, fragmentation,
general broadcast/multicast are unsupported. Explicit `aliases` publish network-local
DNS names; an empty list disables peer DNS on this member. Native addresses remain
explicit; the container catalog supplies automatic address allocation.
MTU is 1500. Slow/missing peers cause bounded packet drops, not a global wait.
The Run interception profile records `peer_ipv4`; peer packet counts are separate
from host-egress/proxy flow counters. No network performance claim follows from
packet contract tests. See the [transport contract](../pvisor-overlaynet/README.md#same-host-peer-packet-networks).


Container networks use `docker::Store::{create_network, networks, inspect_network,
network_instances, remove_network}` and persistent versioned `networks.json`
under that store. `Create.network_name` selects a definition and
`Create.ipv4_address` optionally requests a static IP. The CLI selects shared
`bridge` by default; omitted SDK selectors and build RUN retain their isolated
link for compatibility. None networking has no NIC. A user-defined network is
created explicitly, and its name/ID resolves to a private runtime group keyed by
the network ID. Config names may contain dots and up to 128 bytes; they never
become filesystem path components. Subnets are RFC1918 or 192.0.2.0/24, /24 only.
Catalog serialization reserves unique addresses across concurrent creations and
stops until container removal; network removal refuses any remaining attachments.
User-defined networks publish live container names in DNS with zero TTL; missing
bare member names return NXDOMAIN and never become host egress grants. Default
bridge permits IP connectivity without member-name DNS. Different stores have
separate default bridges and network catalogs. The virtual gateway, resolver,
DHCP and packet classification use the selected subnet/gateway. See the
[Docker-compatible CLI subset](../pvisor-cli/README.md#same-host-peer-networks).

### Explicit guest control publication

`VmExecutor::with_vsock_ports` explicitly publishing reserved control port 1025
also enables `GuestConfig.control` and a session-owned workload process group.
This selects the shared host-only guest process/file engine used by the native
daemon. Other published ports and ordinary Jobs do not enable it. Guest control
requests stay inside the VM; no host process or filesystem fallback is provided.

## 启动阶段诊断

`PVISOR_STARTUP_PROFILE=1` 启用独立 `pvisor-startup-profile` JSON 行，默认关闭。
私有共享 profiler 覆盖 Session 准备、网络启动、checkpoint 兼容性/内核与可执行文件哈希，
以及 runner 的 lazy lower 重建、baseline lease 和沙箱安装。日志使用现有 frontend 诊断路由，
不进入 workload stdout。成功明确登记；错误、取消或提前退出保留 `outcome=unknown`。
关闭时不读取阶段时钟、不生成记录；写入失败不替换原操作结果。
`PVISOR_STARTUP_TIMING=0` 单独关闭原有 checkpoint/持久化日志，不关闭 profile。

普通 host 执行另外标记 `process.spawn_begin` / `process.spawn_ready`，供分析器截断启动窗口。
这些标记说明宿主已创建进程，不证明应用或 VM guest 就绪。不同进程、不同 profiler 的相对
时钟不共用 epoch，重叠 span 不能相加。采集和离线汇总见
[启动诊断](../../benchmark/startup/README.md#启动瓶颈诊断b-startup-diag)。

### Early runner descriptor capacity

Embedded native VM owners may call `VmExecutor::prepare_process()` before starting
Tokio or other worker threads. On Linux it transiently duplicates `/dev/null`
above all reserved runner child descriptors, then releases both handles. This
prepares capacity in the current process without changing resource limits or
existing descriptors. It is repeatable and optional; failure leaves ordinary
launch checks in force. Other platforms use a no-op. It does not prepare a child
after exec, so call in the actual owner. Later descriptor pressure can still
require table growth. High runner source FDs and their `dup2` collision protection
remain unchanged.

### Parallel checkpoint compatibility checks

The owner retains full executable/firmware SHA-256 validation on the launch path,
and overlaps the independent executable and firmware hashes with one scoped worker,
and joins it on success or failure before returning a complete compatibility record.
If worker creation is unavailable, it falls back to serial hashing. No worker or
unvalidated compatibility record escapes the call. The digest fields, selected
firmware path, boot identity and restore checks stay unchanged. This is
not a metadata-only cache or deferred validation. Existing snapshot compatibility
records and file-selection/error contracts remain unchanged.
