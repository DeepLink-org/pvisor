# PolicyVisor (pVisor)

**Scaling autonomous agent execution.**

PolicyVisor (pVisor) manages execution for Agent CLIs, scripts, and automation
commands. The **p** stands for **Policy**: connect requested capabilities,
effective runtime controls, and reviewable results.

Owns one Job, its internal Run record and Attempts, capability admission,
filesystem Effects, execution placement, and the host CLI (`pvisor`). It can
place Jobs on host, native OCI container, and `pvisor-vm` executors while preserving one
Run contract.
It is not an Agent framework, an OCI runtime, or an operating system.

OverlayFS, OverlayNet, Gateway, and AgentCtl are pVisor runtime drivers.
`pvisor-core` defines Operations, Events and cross-component contracts. This crate
owns Session lifecycle, scheduling, policy adaptation and execution. Job lifecycle
commands are built into `pvisor`; local node lifecycle, cache and memory-pool
tools are grouped under `pvisor service`, while TUI and replay are optional Job
frontends found beside it. Cross-node placement and distributed scheduling
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
Without the option, native image preparation and container execution are unchanged.

## Host Job service and AgentCtl

All built-in Job CLI operations (`run`, `status`, `kill`, `suspend`, `resume`,
`fork`, `checkpoint`, `inspect`, `review`, `apply`, `drop`) submit typed
`cli/host.rs::JobCommand` requests through the on-demand persistent listener in
`cli/host_service.rs`. Ordinary persisted Jobs need no endpoint options or manual
service startup. Bare `pvisor` displays help; default execution via
`pvisor -- COMMAND` enters the same service. This listener is separate from
the node/cache/pool deployment service below; embedded `PVisor` remains a direct API.

Host authority lives under canonical `/tmp/pvisor-host-<effective-UID>`
(usually `/private/tmp` on macOS): a same-user, non-symlink `0700` root with
`0600` sockets and generation/capability state. Kernel same-UID credentials,
frontend PID, generation and worker registration are checked before admission.
The frontend retains the terminal and spawns a listener-authorized request
worker; `SCM_RIGHTS` transfers stdio and its private channel. Cwd, environment,
stdio, cancellation and exit status remain request-local. Executors exclude the
shared authority root from guest exposure; Guest AgentCtl `Hello`/`Sync` tokens
cannot authorize Host Job, VM or daemon-supervisor operations.

Core's version-1 Host envelopes carry `request_id`, optional Job/Attempt/generation
`target`, typed commands and correlated results/errors. Core's envelope and
supervisor contracts are pure shared definitions/validation. `JobCommand`
embeds internal CLI DTOs tied to an exact schema/build, not a stable public API.
The Job listener, internal workers and daemon native supervisors use shared
`runtime/host_transport.rs` async/sync newline JSON framing and same-effective-UID
peer authentication: 1 MiB of JSON excluding the newline, consuming
only through the delimiter. `SCM_RIGHTS` FD markers are separate transport
records, not JSON. The internal version-1 handshake checks the Job ticket schema,
Cargo package version and BLAKE3 executable content digest before descriptor/
command transfer; package version alone is insufficient. Worker executable
ownership, permissions, device/inode and content are checked.

Linux hashes `/proc/self/exe`. On macOS, `cli/host_image.rs` compares dyld's loaded
main-image UUID with on-disk Mach-O `LC_UUID` for the matching CPU slice before
hashing the same open file. Admission requires a matching source Mach-O UUID;
missing, malformed, ambiguous or mismatched metadata fails closed. UUID
matching detects pathname replacement but does not attest loaded memory
byte-for-byte or provide kernel-pinned exec authority. The macOS platform path
remains uncompiled and untested; parser checks do not validate dyld access,
platform linking or real replacement behavior.

SIGINT/SIGTERM/SIGHUP are latched before admission; frontend and listener retain
worker/cleanup ownership and terminal restoration. Linux implements subreaper,
`/proc` descendant tracking and pidfd signalling, excluding the listener from
cleanup. macOS tracks birth-identified descendants and known workload groups
across ordinary process-group changes, freezes the root and discovered forkers,
and rescans before individually birth-checked cleanup signals. It is not limited
to the worker process group, but it cannot guarantee ownership of already-
reparented orphans missed by discovery. libproc checks followed by numeric-PID
signals are not atomic pidfd operations or Linux-equivalent containment. The
macOS cleanup path remains uncompiled and untested.

Host transport and process checks do not establish guest correctness or full
platform validation. Real-VM TUI end-to-end validation remains unavailable;
macOS identity and cleanup paths retain the platform-specific limits above.

The listener is persistent, not a durable request queue. Request IDs correlate
responses and cancellation; they do not deduplicate every operation or promise
exactly-once execution. Some durable Job operations have scoped receipts only.
Lost responses, timeouts and cancellation can follow effects; the CLI reports
ambiguity and does not automatically retry. Reconcile state before resubmitting.
Drain active requests and stop old listeners with the old binary before upgrade.
Daemon native supervisors use newline-delimited version-1 Host envelopes;
the wire is incompatible with old supervisors, so drain their sandboxes using
the old binary before upgrade too. There is no legacy fallback. See
[Host and Guest AgentCtl](../../docs/src/en/design/architecture.md#host-agentctl)
for contract bounds, ownership and limitations.

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
than silently downgraded. Existing workspace forks relying on those configurations
must not be treated as supported by this change.

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
Native control authority remains available through the handle/service. The daemon
uses this setting to avoid publishing a second control endpoint for the same VM. `pvisor run` prints `--vm-socket PATH
--vm-job-id ID --vm-attempt-id ID` to stderr; copy those exact values into normal
commands in another host terminal. The example root assumes Linux effective UID
`1000`; replace it and the identities with the printed values:

```bash
pvisor run --executor vm -- /bin/sleep 600
pvisor status --vm-socket /tmp/pvisor-host-1000/vm-EXAMPLE.sock --vm-job-id run-EXAMPLE --vm-attempt-id attempt-EXAMPLE
pvisor suspend run-EXAMPLE --vm-socket /tmp/pvisor-host-1000/vm-EXAMPLE.sock --vm-job-id run-EXAMPLE --vm-attempt-id attempt-EXAMPLE --vm-pause
pvisor resume run-EXAMPLE --vm-socket /tmp/pvisor-host-1000/vm-EXAMPLE.sock --vm-job-id run-EXAMPLE --vm-attempt-id attempt-EXAMPLE --vm-load
pvisor suspend run-EXAMPLE --vm-socket /tmp/pvisor-host-1000/vm-EXAMPLE.sock --vm-job-id run-EXAMPLE --vm-attempt-id attempt-EXAMPLE --vm-offload --vm-ram-file /private/vm-ram/offloaded.ram
pvisor resume run-EXAMPLE --vm-socket /tmp/pvisor-host-1000/vm-EXAMPLE.sock --vm-job-id run-EXAMPLE --vm-attempt-id attempt-EXAMPLE --vm-load
```

All three global addressing options are required, including
for live `status`; suspend/resume's positional selector must match `--vm-job-id`.
Only `status`, `suspend --vm-pause` / `--vm-offload` and `resume --vm-load` support
live mode. Pause and offload are mutually exclusive. `--vm-ram-file` requires
`--vm-offload`; omit it to use the current backing, or provide a new
guest-inaccessible path on the same filesystem. `--vm-load` selects
`HostVmCommand::Resume`, mapped to `RunResume` of the same live Attempt; there is
no `Load` wire operation. It is not a persistent snapshot restart or eager RAM
prefault. Without live options, persisted-Job suspend/resume retain their
checkpoint capture/restoration behavior. Pause stops vCPUs; offload uses the
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
| `--vm-control-socket PATH` | `control_socket` | Unset: CLI automatic `/tmp/pvisor-host-<effective-UID>/vm-<UUID>.sock` (canonical `/tmp`) |
| `--vm-ram-backing FILE` | `ram_backing` | Unset: attempt-local file in ordinary file-backed mode |
| `--vm-ram-compression[=BOOL]` | `ram_compression` | `false`; FUSE/macFUSE backing |
| `--vm-cold-ram-compression[=BOOL]` | `cold_ram_compression` | `false`; Linux x86_64 local cold pager |
| `--vm-ram-dedup[=BOOL]` | `ram_dedup` | `false`; best-effort host advice |
| `--vm-memory-pool SOCKET` | `memory_pool` | Unset; experimental Apple Silicon pool, unsupported on Linux |
| `--vm-node-socket SOCKET` | `node_socket` | Unset; same-host resource service |
| `--vm-snapshot-filesystem-pool DIR` | `snapshot_filesystem_pool` | Unset; Linux x86_64 no-network native checkpoint lower pool |

Boolean flags accept bare=true or `=true` / `=false`; omission preserves config.
True flags and explicit path options infer VM when `--executor` is omitted;
`=false` alone does not. Explicit executors are not silently replaced. Path
options override only their corresponding field. A custom CLI control path must
be directly under the canonical private Host service root. The root must be
non-symlink, same-effective-UID and exactly `0700`; arbitrary private parents
are rejected by CLI workers. Embedded callers retain private custom-parent validation and
automatic private `vm-*` directories under the authority root. Existing paths
are never overwritten. The `0600` socket accepts same-UID peers and is removed
at Attempt termination. It must never be guest-accessible, including in
host-rootfs VMs; the host parent provides executor exclusions. It is separate
from staged Job control and is not exported as a guest discovery file.
`--vm-control-socket` selects the creation path; `--vm-socket` addresses a live
endpoint. The global live addressing/action options are not `[vm]` fields.

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

## Local service boundary

`pvisor service run --config service.toml` supervises local resource owners;
`status`, `restart ROLE` and `stop [--role ROLE]` manage their lifecycle. The
`node` role shares immutable image mounts and snapshot RAM while retaining
same-user authorization, compatibility checks and active-pin restart/stop
fences. The optional `pool` role serves the experimental Apple Silicon cold-page
pool. `service cache` and `service memory-pool` dispatch their installed tools.

Linux x86_64 also has default-off experimental instance-local live cold
compression: `VmSettings.cold_ram_compression` / `[vm].cold_ram_compression` or
`pvisor run --vm-cold-ram-compression -- COMMAND`. The flag selects VM execution;
the runner automatically starts the runtime-owned userfaultfd pager over private
anonymous ordinary RAM using `LocalColdRamStore`, without a live backing file,
FUSE or a service. This is separate from `vm.ram_compression` (FUSE backing).
Kernel-fault syscall or `/dev/userfaultfd` authority is required; compiled support
is not permission, and missing authority fails startup without fallback or global
sysctl changes. Linux external `memory_pool` is deliberately unsupported.

The store rejects raw/poorly compressed blocks, caps encoded payload at half
configured RAM and bounds object count by the configured 64 KiB block count.
Two quiescence windows capture/recheck bounded batches before discard; validated
`UFFD_COPY` restores refaults. Guest execution continues between windows without
guest application participation; this is experimental eviction/refault probing,
not ordinary pause or a read-access heat detector. Admission rejects dedup,
file/FUSE backing, snapshot capture/restore and whole-VM offload combinations.
See [local compression](../../docs/src/en/design/memory-optimization/compression-local.md)
for user-specific device ACLs, restricted mappings/build features and ownership.
This delivers an experimental mechanism, not a production-density claim; sealed
`memfd` pooling remains proposed.

A minimal configuration is:

```toml
state = '.pvisor/services'

[node]
cache_backend = 'filesystem'
cache_location = 'cache'
snapshot_roots = ['snapshots']
```

Paths resolve relative to the configuration file; node state/socket paths resolve
under service state. Linux deployments can set `cgroup_root = ':self:'` **before
`[node]`**, using a real delegated unified cgroup v2 hierarchy. Service limits
remain installed before child execution, with positive per-role `[limits.node]`
(and optional `[limits.pool]`) budgets. Without delegation, process ownership is
not evidence of kernel-enforced limits. Resource owners are not transparently
recoverable after a crash; drain pins before restart/stop. Node pin checks do not
prove the pool has no direct VM clients. Drain those clients separately: SIGINT
puts the pool into draining, and a stop timeout can leave it rejecting new clients
while retaining existing data owners.

The service configuration accepts local resource roles, not distributed
scheduler roles; `controller`, `worker` and `workers` keys are rejected even
when their sections/lists are empty.
`pvisor service daemon ...` passes arguments unchanged to a trusted, separately
installed `pvisor-daemon` beside `pvisor`; it does not add a daemon role to this
configuration or link a daemon dependency. The daemon's VM-only `NativeRuntime`
embeds this crate in detached supervisor subprocesses. The daemon CLI is wired
to that runtime; companion dispatch does not automatically acquire node sharing
resources. The daemon API has no stage/apply or checkpoint/fork implementation.

### Build and validation boundary

The root `service-build` selects local binaries and builds the daemon separately.
Ignored local gates require explicit selection and the environment described in
their test annotations. Install `pvisor-daemon` beside `pvisor` for companion
dispatch. The daemon is a separate executable linking `pvisor` and `pvisor-core`;
synchronous internal VM dispatch runs before Tokio. Packaging includes both
executables, but does not supply or validate the prepared-image bootstrap,
SDK conformance or density.
See the [daemon boundary](../pvisor-daemon/README.md).

## Develop

```bash
just build release          # release build + macOS Hypervisor signing
just build    # debug build + macOS signing
just test pvisor
just examples
```

On macOS, source builds that use HVF must be signed. `just build release` does this;
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
`rustup target add x86_64-unknown-linux-musl`. Use `just build` so target
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
