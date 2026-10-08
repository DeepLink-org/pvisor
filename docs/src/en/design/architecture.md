# Core architecture

pVisor processes Operations: it accepts requests, decides how to handle and place them according to policy, calls the actual execution mechanism, then describes what happened through Events.

There are two responsibilities: **core provides definitions; pvisor provides implementation.** External callers submit execution requests, control execution through a run handle and observe progress and results through Events.

![Technical architecture centered on pvisor-vm: CPUs, memory, devices, consistent freeze and KVM/HVF adapters](assets/pvisor-architecture.svg)

## Definitions and implementation

| Owner | Responsibility |
| --- | --- |
| `pvisor-core` | Operation, policy decisions, Placement, Outcome, Event and shared interaction contracts; pure validation and policy evaluation |
| `pvisor` | Request parsing, capability admission, actual policy rewrites, placement selection, scheduling, execution and Attempt lifecycle |
| `pvisor-journal` | Event commit, receipts, deduplication, causal reference validation and recovery |
| `pvisor-overlay-core` | File authorization integration, copy-on-write, preimages, review/apply/recovery/drop |
| `pvisor-overlayfs` | Host FUSE mounts and file operation integration |
| `pvisor-overlaynet` | Network parsing, proxy forwarding and VM networking; enforcement of core policy definitions |
| `pvisor-guest` | VM PID 1 and command launch contract |
| `pvisor-gateway` | Optional model protocol routing, conversion and call observation |
| `pvisor-daemon` | Separate single-node sandbox admission, durable ownership, native VM supervisor lifecycle and endpoint proxy |
| `pvisor-cli` | CLI, TUI, cache/replay frontends and Host Job application adapters; consumes the runtime |
| `pvisor-vm` | VMs, devices, freezing, snapshots and RAM mappings; uniform api and private platform implementations |
| `pvisor-replay` | Agent trajectory replay mechanisms; consumes Core/Journal contracts, with frontends in CLI |

The [daemon](daemon/index.md) uses VM-only NativeRuntime: detached supervisors embed `PVisor::run` and retain RunHandles across daemon restart. The executable constructs NativeRuntime with native CLI settings; stage/apply, checkpoint/fork and Gateway APIs are absent, and node sharing is not automatically acquired. See [responsibility convergence](daemon/responsibility-convergence.md). External orchestration owns host choice and workflows.

Core neither owns the execution loop nor starts processes or opens control sockets. pvisor implements AgentCtl clients/servers and approval sockets. Drivers implement file, network and isolation boundaries. The default core does not depend on Gateway, TUI or replay; the `gateway` feature enables capture.

## API boundaries and migration status {#api-boundaries}

Logical crate responsibilities and Rust visibility require separate checks. Only `pvisor-vm`, `pvisor-overlayfs` and `pvisor-journal` currently use a sole `api` entry point. Other crates retain existing interfaces; layering in an architecture diagram does not establish a completed workspace migration.

| Crate | Current public boundary | Implementation and resource ownership |
| --- | --- | --- |
| `pvisor-vm` | `pvisor_vm::api`; trait-based use of `VmBuilder` and `VmmHandle` | Private backend, VMM, devices, RAM and platform dispatch |
| `pvisor-overlayfs` | `pvisor_overlayfs::api`; configuration, mount, session and metrics contracts | Private `fs`, `mount` and `observation`; owns neither apply nor Run lifecycle |
| `pvisor-journal` | `pvisor_journal::api`; `JournalStore`, `TraceProducer`, `DurableFiles` | Private `journal`, `trace`, `persistence`; Core owns Event/Receipt |
| `pvisor-core` | Domain modules and root re-exports; not migrated | Shared identities, protocols, pure validation and policy definitions; no execution resources |
| `pvisor-overlay-core`, `pvisor-overlaynet` | Existing public modules and root interfaces; not migrated | File semantics, dual-entry file service, proxies and egress data plane |
| `pvisor`, Gateway, Replay, Daemon and others | Retain existing entries; no uniform sole-`api` model yet | Continue evolving within actual lifecycle and domain boundaries |

Migrated `api` modules declare public data, fields and trait methods. Opaque owners may be re-exported from private implementations while retaining private state. Method bodies, validation, platform dispatch and resource management live in implementations; public traits have no default bodies. Callers import contracts from the owning crate's `api`. Declarations keep the same shape across supported platforms/features; capability queries and explicit unsupported errors describe actual support.

Migration proceeds one crate at a time, updating its consumers, README, public documentation and boundary checks together while preserving external contract coverage. Unmigrated crates should not gain blanket re-export layers or duplicate DTOs merely to appear uniform, nor expose internals to bypass compilation boundaries. Hardware/private-state checks remain inside the crate; callers rely on ownership, lifecycle, post-failure state and synchronization contracts.

This Rust boundary does not automatically establish stable wire protocols or old-record compatibility. CLI `JobCommand`/tickets still require exact build matching, and stored formats have independent readers. See [Records and version matrix](records-and-versions.md) and the VM rationale in [ADR 0005](decisions/0005-rust-vm-api.md). Status follows static inspection of `lib.rs`, VM `runtime_modules.rs`, `api.rs` and directory READMEs; it does not claim fresh contract-test or platform validation.

## Host and Guest AgentCtl {#host-agentctl}

Host AgentCtl is the authority path for built-in Job CLI operations. Guest
AgentCtl is the per-Attempt cooperation path for workload `Hello`/`Sync`, client
state, directives and checkpoint quiescence. They have separate schemas,
credentials and endpoints: a guest cooperation token never authorizes host Job,
VM, staged-file or daemon-supervisor controls. The Bundle's `agentctl` snapshot
still describes Guest cooperation, not a Host authorization receipt.

### Listener and request ownership {#host-job-service}

The CLI parses a typed `JobCommand` and connects to an on-demand persistent
listener. `JobCommand` embeds CLI DTOs: it is an internal exact-schema/build
contract, not a stable public API. Core's shared Host envelopes and supervisor
contracts remain pure definitions and validation, without CLI DTOs or transport. `cli/host_service.rs` serializes startup with a private lock and
publishes a generation/capability manifest plus a generation-named Unix socket
under canonical `/tmp/pvisor-host-<effective-UID>`. The root is same-UID,
non-symlink and exactly `0700`; sockets and private manifest files are `0600`.
Invalid pre-existing entries fail closed rather than being repaired with chmod.
The listener is independent of daemon sandbox/pool ownership and the standalone cache. Node resource protocols are runtime facilities with no daemon acquire/release adapter.

After kernel same-UID peer authentication and build compatibility negotiation,
the frontend transfers stdin/stdout/stderr with `SCM_RIGHTS` and sends its typed
command, cwd, environment, terminal context and pinned target. The listener
returns a correlated ticket with stdio and a private worker channel. The
frontend launches the checked executable as a child in its originating terminal
session and a separate process group. The listener verifies registration and
worker readiness before granting admission; the worker does not reinterpret
shell argv. Cwd, environment, output and exit status belong to the request,
not the persistent listener. Embedded `PVisor` calls still enter the runtime
directly rather than requiring this CLI frontend.

Persisted-Job selectors resolve without endpoint arguments. Selected records
are pinned to Job, Attempt and execution generation where present and rechecked
before effects; stale selection is not silently retargeted. Host live controls
and stage discovery links also use private endpoints under the shared authority
root. Executors exclude the authority root from guest exposure; the CLI rejects
guest filesystem sources that expose it. Same-UID is a host trust boundary,
not isolation from every process belonging to that user.

### Wire contract and upgrades {#host-wire}

Core defines `AgentCtlHostRequest<C>` (`version`, `request_id`, optional `target`,
`command`) and `AgentCtlHostResponse<R>` (`version`, `request_id`, `result`).
Version is currently **1**; targets contain `job_id`, optional `attempt_id` and
optional `generation`. Endpoint owners validate authorization and target scope.
Live Attempt endpoints require the exact Job/Attempt and reject an independent
generation. Envelopes reject unknown fields; identities are nonempty, at most
256 bytes, and contain no control characters. The Job service and its internal
workers use `runtime/host_transport.rs` for newline-delimited JSON, with the
same async/sync framing rules and a 1 MiB JSON limit excluding the newline.
Readers consume only through that delimiter, preserving following frames or FD
markers. `SCM_RIGHTS` marker bytes are separate transport records, not JSON;
`cli/host_fds.rs` owns their descriptor handling. Typed error codes are
`invalid_request`, `unauthorized`, `version_mismatch`, `conflict`, `unsupported`,
`internal` and `unavailable`.

Core's `host_protocol` also defines live VM `HostVmCommand` (`Pause`, `Resume`,
`Offload`, `Status`) and `HostVmResult` (`status`, `value`). `pvisor` exports
`host_vm_exchange` for typed Host request/response exchange. `--vm-load`
selects `Resume`, mapped to
`RunResume` of the same live Attempt, not a `Load` wire operation.

The internal Job handshake checks Host version **1**, the Job ticket schema,
Cargo package version and BLAKE3 executable content digest
before descriptor or command admission. A package version alone does not
establish compatibility; in-place rebuilds can also be incompatible. Executable
path, ownership, permissions, device/inode and file content are checked before
worker launch.

Linux hashes `/proc/self/exe`. On macOS, `cli/host_image.rs` checks the UUID of
dyld's loaded main image against the on-disk Mach-O `LC_UUID` for the matching
CPU slice before hashing that same open file. Missing, malformed, ambiguous or
mismatched UUID metadata fails closed; admission requires a matching source
Mach-O UUID. This checks executable identity across the first pathname
replacement. UUID matching is not byte-for-byte
attestation of loaded memory or kernel-pinned exec authority. The macOS platform
path has not been compiled or tested; parser checks do not validate dyld access,
platform linking or actual executable replacement behavior.

Daemon native supervisors use the same version-1 newline Host envelopes
with private owner/token credentials and Job/Attempt/generation targeting.
They do not use Guest `Hello`/`Sync` or the CLI worker ticket mechanism. This wire
format is incompatible with old supervisors. Drain sandboxes using the old
binary before upgrade; similarly drain active CLI requests and stop the old
Job listener before replacing it. A new client rejects an incompatible live
listener before submitting descriptors or commands. There is no legacy fallback
or transparent takeover of an old supervisor.

### Cancellation and validation limits {#host-limits}

The frontend latches SIGINT/SIGTERM/SIGHUP before startup and admission, sends
correlated cancellation, and owns terminal restoration and worker reaping. The
listener retains request cleanup ownership through worker completion and can
escalate cleanup. Linux implements subreaper adoption, `/proc` descendant
tracking and pidfd-based signalling, with the listener excluded from request
cleanup. macOS tracks birth-identified descendants and known workload groups
across ordinary process-group changes. Cleanup freezes the root and discovered
forkers, rescans until the tracked set stabilizes, then sends individual
birth-checked signals. Cleanup covers tracked descendants beyond the worker
process group.
Already-reparented orphans missed by discovery are not guaranteed to be owned;
libproc identity checks followed by numeric-PID signals are not atomic pidfd
operations. This is not Linux-equivalent containment. The macOS cleanup path has
not been compiled or tested.

The persistent listener accepts requests without persisting a request queue.
`request_id` correlates responses, errors, tickets and cancellation; universal
deduplication and exactly-once behavior are outside its contract. Some durable
Job operations retain their own scoped receipts, but that does not cover every Host command. Disconnects, timeouts and
cancellation can follow effects already performed; the frontend reports
ambiguity and does not automatically retry. Reconcile Job state and artifacts
before deciding what to submit next.

The Host AgentCtl path lacks real-VM TUI end-to-end validation. Existing transport,
process or mock checks do not cover guest correctness, production durability or
full platform behavior; see above for macOS identity and cleanup limits.

## A production execution path

![Preparation, launch-fact commit, dispatch, cleanup and result ordering](assets/execution-sequence.svg)

Driver preparation creates real file and network resources; executor dispatch starts the workload. Separating these steps lets a failed required launch-fact commit prevent dispatch while the Session reclaims prepared resources. Completion similarly converges resources and observations before terminal publication; cancelling a caller wait does not establish that those actions finished.

`RunSpec` is execution configuration supplied by the caller; `Operation` is the structured operation description. The only current production operation is `run.execute`, with program, arguments and working directory. Executors consume the effective RunSpec and prepared driver attachments. `PVisor::resolve_operation` uses the same admission path for pre-launch review; it cannot replace execution or evidence that controls were installed.

Admission preserves snapshots of requested and effective operations. Actual policy changes are recorded as Rewritten; selected VM/Overlay placement as Placed. Interception happens at driver boundaries: file operations enter OverlayFS/OverlayCore and traffic enters OverlayNet. They share policy definitions, but individual file and network operations are not currently promoted to separate public Operations.

For example, the network driver narrows requested Ambient capability to Deny. Requested preserves original permissions, Rewritten stores before/after snapshots, Placed describes final placement, and the executor runs the effective configuration. These snapshots record actual policy handling; a general rule interpreter is outside the current implementation scope.

## One lifecycle owner

Each current `PVisor::run` creates one Attempt owned and managed by pvisor's `Session`. See [Execution model](execution-model.md) for Job, Run and Attempt identities.

Session owns driver preparation, the Guest AgentCtl server, cancellation/timeouts, cleanup, observation checks, Bundle storage and terminal publication. Executors return `ExecutorOutput`; they neither assign Job/Attempt identities nor publish terminal state. `RunHandle` exposes status, cancellation, checkpoints and event subscriptions. Requesting cancellation does not mean execution has stopped.

Process/VM executors clean up their managed process groups; containers use the runtime termination interface. See [Isolation design](isolation.md) for descendants outside the group and platform gaps. Guest AgentCtl provides workload cooperation and checkpoint quiescence, not mandatory enforcement; Host AgentCtl is the separate host-authority path described above.

## Event is the observation interface

External callers observe Events without depending on Session's internal fields. See [Operation and Event](operations-events.md) for chains, identities, causal references and recording boundaries.

The implementation prepares drivers, commits startup facts, then calls the executor. Failure to commit required startup facts prevents dispatch and triggers prepared-resource cleanup. Preparation can still have file or socket effects.

## Policy, controls and evidence

Policy, admission plans and completion observations are separate layers. See [Capabilities and evidence](../concepts/capabilities-and-evidence.md) for `ExecutorPlan`/`ExecutorObservations` levels and evidence rules.

Core shares file/network policy evaluation. User, workspace, session and executor baseline policies jointly constrain permissions; a later allow cannot override an earlier explicit deny. pvisor and drivers implement actual authorization, interception and control installation.

## Records and file application

| Record | Question answered |
| --- | --- |
| `run.json` | What are the Job's identity, state, executor and local resources? |
| Run Bundle | What are the result, control observations, artifacts and file/network summaries? |
| Event Journal/Trace | Which facts were published and how are they related? |
| Overlay diff/preimages | Which changes await review and what original state must apply check? |

Each record has its own scope. Events reconstruct observed operation history, not all external state. Native agent trajectory replay is also not deterministic replay of arbitrary effects.

Review staged files before applying them. OverlayCore validates targets, persists apply intent, updates files and recovers. A batch is not an atomic filesystem transaction. See [Capabilities and evidence](../concepts/capabilities-and-evidence.md) for `apply`/`drop`/checkpoint recovery and irreversible effects; see [Review and apply](../guides/review-apply.md) for the workflow.
