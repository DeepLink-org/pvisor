# Core architecture

pVisor processes Operations: it accepts requests, decides how to handle and place them according to policy, calls the actual execution mechanism, then describes what happened through Events.

There are two responsibilities: **core provides definitions; pvisor provides implementation.** External callers submit execution requests, control execution through a run handle and observe progress and results through Events.

For the overall diagram and an end-to-end task walkthrough see [Design and internals](index.md). The contracts below cover crate ownership, API boundaries and the host authority path.

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
| `pvisor-shim` | containerd Runtime v2 shim (`io.containerd.pvisor.v2`); task lifecycle, exec and FIFO/PTY IO for the host process path |
| `pvisor-vm` | VMs, devices, freezing, snapshots and RAM mappings; uniform api and private platform implementations |
| `pvisor-replay` | Agent trajectory replay mechanisms; consumes Core/Journal contracts, with frontends in CLI |

The [daemon](daemon/index.md) uses VM-only NativeRuntime: detached supervisors embed `PVisor::run` and retain RunHandles across daemon restart. The executable constructs NativeRuntime with native CLI settings; stage/apply, checkpoint/fork and Gateway APIs are absent, and node sharing is not automatically acquired. External orchestration owns host choice and workflows.

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

This Rust boundary does not automatically establish stable wire protocols or old-record compatibility. The frontend verifies the same binary when self-executing its owner; later CLIs use instance-protocol compatibility without requiring matching binary digests. Stored formats have independent readers. See [Records and version matrix](records-and-versions.md). Status follows static inspection of `lib.rs`, VM `runtime_modules.rs`, `api.rs` and directory READMEs; it does not claim fresh contract-test or platform validation.

## Host and Guest AgentCtl {#host-agentctl}

Host AgentCtl is the authority path for built-in Job CLI operations. Guest
AgentCtl is the per-Attempt cooperation path for workload `Hello`/`Sync`, client
state, directives and checkpoint quiescence. They have separate schemas,
credentials and endpoints: a guest cooperation token never authorizes host Job,
VM, staged-file or daemon-supervisor controls. The Bundle's `agentctl` snapshot
still describes Guest cooperation, not a Host authorization receipt.

### Job owner and request ownership {#host-job-service}

`pvisor run` keeps a terminal frontend and one self-executed owner. They inherit
stdio and exchange cancellation/exit information over a private socketpair. The
owner stays in the original session in a separate process group. Interactive
execution uses the original controlling terminal, with foreground ownership
restored at exit; terminal Ctrl-C may reach the workload directly. Linux makes
the owner the subreaper and retains pidfd-based descendant cleanup.

Before executor entry, the owner binds a private socket and writes `live_control`
to the host-only Job record. The canonical address is:

```text
/tmp/pvisor-host-<UID>/jobs/<job-id>/<attempt-id>/control.sock
```

Directories are same-UID, non-symlink and `0700`; the socket is `0600`. The record
binds its path to device/inode, protocol, owner PID and starting binary identity.
Clients authenticate peer UID and check the socket identity before and after
connecting. Executors exclude the whole authority root from guest views. Terminal
publication clears discovery; the owner removes only its own socket inode.
Embedded runs publish the same record when storage is retained.

Live status, kill, native VM transitions, execution capture and running inspect
execute in the starting owner binary. Stopped status, review, apply, drop and
inspect run directly in the calling process. Sealed checkpoint restoration/fork
starts a new owner. Apply/drop refuse a valid owner still holding stage. Durable
operations use exclusive `flock` on the Job root's `job.lock`, validated as an
owned `0600` file with one hard link. Its diagnostic record contains the lock
protocol, PID and operation. Unknown Job schemas refuse mutation. Residual owner
metadata can be cleared only after its process dies and no valid control endpoint
remains. Read-only inspect uses stage-declared lower mutability, defaulting to
Mutable for missing or rebound declarations; freshness is not guaranteed.

### Existing internal channels and cleanup order {#owner-internal-control}

The runtime still retains these internal channels inside the Job's process. They
add no user-wide persistent process.

| Channel | Current callers and implementation | Planned consolidation |
| --- | --- | --- |
| Overlay control | `registry.rs::RunControlServer` discovers its private endpoint through the stage's `control.sock` link; owner status queries itself, the TUI still reads observations, and the old inspect mount table duplicates owner inspect | Migrate TUI observations and SDK callers before removing old inspect branches, the listener and link discovery |
| Native execution capture | `job_execution.rs::Server` retains `execution.sock`; after receiving capture, the owner still calls itself through this runtime endpoint | Call directly inside the owner while preserving request receipts, operation locks, freezing and suspended-head terminal confirmation |
| Explicit VM debugging | `--vm-socket` and embedded `host_vm_exchange` use a separate asynchronous exchange; record discovery uses `LiveControlRecord::exchange_checked` | Share bounded, cancellable transport while retaining the public SDK adapter |

Cleanup first unifies debug transport, then narrows owner bootstrap messages to
run, fork and resume. `JobCommand` currently serializes all commands although
only those three cross the socketpair; other commands' serialization, owner
terminal branches and round-trip tests can then be removed. Production callers
always pass zero for `OwnedTree.excluded`; `Completion.signal` has no reader,
and restoration's `vm_control_socket(None)` always returns None. These cleanups
are pending. Channel consolidation must migrate callers and contract tests
together.

### Wire contract and upgrades {#host-wire}

The instance contract is named `pvisor-instance-control-1`. Core's version-1 Host
envelopes carry correlation, exact Job/Attempt targets and typed results/errors.
`runtime/host_transport.rs` retains newline JSON framing with a 1 MiB frame limit.
The owner admits at most eight connections and uses existing operation locks for
mutations. The public socket receives no terminal descriptors or guest tokens.
Internal CLI DTOs bootstrap only the self-executed owner.

Caller and owner binaries may differ when the instance protocol matches. An
incompatible Job protocol identifies that owner's recorded binary; it does not
block other Jobs. Before upgrading, drain old Jobs and stop the old
`--pvisor-internal-host` with the old binary. New CLIs never connect to old
`<generation>.sock` endpoints. Records without live discovery expose disk state
only, and do not discover old running VMs. The daemon supervisor's listener,
private credentials, framing and admission remain unchanged.

### Cancellation and validation limits {#host-limits}

Signal handlers are installed before the owner unblocks SIGINT/SIGTERM/SIGHUP.
The frontend reaps its owner child; the owner handles cancellation, disconnection
and descendants. Linux uses subreaper adoption, `/proc` discovery and pidfds.
macOS retains birth-identity checks and polling: missed reparented orphans are not
guaranteed contained, and numeric signals are not atomic pidfd operations. Loaded
Mach-O UUID verification is not byte-for-byte memory attestation. Moving the
listener does not establish additional macOS isolation guarantees.

Lost responses, timeouts and cancellation can follow accepted effects. The
record-discovery path reports uncertainty and never retries automatically.
The current `--vm-socket` debug path still awaits the old asynchronous exchange:
Ctrl-C does not interrupt its response wait, and underlying disconnect errors
may omit the uncertainty warning. This implementation gap awaits consolidation.
Reconcile state after a disconnect on either path. Scoped checkpoint receipts
do not provide universal exactly-once delivery.
Transport/process tests do not establish guest correctness or real-VM TUI coverage.

## A production execution path

For the full preparation, launch-fact commit, dispatch, cleanup and terminal-publication sequence see the walkthrough in [Design and internals](index.md#execution). The three ownership-level contracts follow.

`RunSpec` is execution configuration supplied by the caller; `Operation` is the structured operation description. The only current production operation is `run.execute`, with program, arguments and working directory. Executors consume the effective RunSpec and prepared driver attachments. `PVisor::resolve_operation` uses the same admission path for pre-launch review; it cannot replace execution or evidence that controls were installed.

Admission preserves snapshots of requested and effective operations. Actual policy changes are recorded as Rewritten; selected VM/Overlay placement as Placed. Interception happens at driver boundaries: file operations enter OverlayFS/OverlayCore and traffic enters OverlayNet. They share policy definitions, but individual file and network operations are not currently promoted to separate public Operations.

For example, the network driver narrows requested Ambient capability to Deny. Requested preserves original permissions, Rewritten stores before/after snapshots, Placed describes final placement, and the executor runs the effective configuration. These snapshots record actual policy handling; a general rule interpreter is outside the current implementation scope.

## One lifecycle owner

Each current `PVisor::run` creates one Attempt owned and managed by pvisor's `Session`. See [Execution model](execution-model.md) for Job, Run and Attempt identities.

Session owns driver preparation, the Guest AgentCtl server, cancellation/timeouts, cleanup, observation checks, Bundle storage and terminal publication. Executors return `ExecutorOutput`; they neither assign Job/Attempt identities nor publish terminal state. `RunHandle` exposes status, cancellation, checkpoints and event subscriptions. Requesting cancellation does not mean execution has stopped.

Process/VM executors clean up their managed process groups; containers use the runtime termination interface. See [Isolation design](isolation.md) for descendants outside the group and platform gaps. Guest AgentCtl provides workload cooperation and checkpoint quiescence, not mandatory enforcement; Host AgentCtl is the separate host-authority path described above.

## Event is the observation interface

External callers observe Events without depending on Session's internal fields. See [Operation and Event](operations-events.md) for chains, identities, causal references and recording boundaries.

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
