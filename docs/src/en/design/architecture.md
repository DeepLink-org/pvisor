# Core architecture

pVisor processes Operations: it accepts requests, decides how to handle and place them according to policy, calls the actual execution mechanism, then describes what happened through Events.

There are two responsibilities: **core provides definitions; pvisor provides implementation.** External callers submit execution requests, control execution through a run handle and observe progress and results through Events.

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
| `pvisor-tui`, `pvisor-replay` | Terminal frontend and agent trajectory replay tools depending on pvisor |

Core neither owns the execution loop nor starts processes or opens control sockets. pvisor implements AgentCtl clients/servers and approval sockets. Drivers implement file, network and isolation boundaries. The default core does not depend on Gateway, TUI or replay; the `gateway` feature enables capture.

## A production execution path

```text
CLI / embedded caller
  → RunSpec
  → pvisor: admission, policy rewrite, Placement → effective Operation
  → Session: prepare drivers and run resources
  → commit startup facts
  → RunExecutor::execute
  → clean up, check control observations, save results, publish terminal state
```

`RunSpec` is execution configuration supplied by the caller; `Operation` is the structured operation description. The only current production operation is `run.execute`, with program, arguments and working directory. Executors consume the effective RunSpec and prepared driver attachments. `PVisor::resolve_operation` uses the same admission path for pre-launch review; it cannot replace execution or evidence that controls were installed.

Admission preserves snapshots of requested and effective operations. Actual policy changes are recorded as Rewritten; selected VM/Overlay placement as Placed. Interception happens at driver boundaries: file operations enter OverlayFS/OverlayCore and traffic enters OverlayNet. They share policy definitions, but individual file and network operations are not currently promoted to separate public Operations.

For example, the network driver narrows requested Ambient capability to Deny. Requested preserves original permissions, Rewritten stores before/after snapshots, Placed describes final placement, and the executor runs the effective configuration. This records actual policy handling; it is not a general rule interpreter.

## One lifecycle owner

Each current `PVisor::run` creates one Attempt owned and managed by pvisor's `Session`. See [Execution model](execution-model.md) for Job, Run and Attempt identities.

Session owns driver preparation, the AgentCtl server, cancellation/timeouts, cleanup, observation checks, Bundle storage and terminal publication. Executors return `ExecutorOutput`; they neither assign Job/Attempt identities nor publish terminal state. `RunHandle` exposes status, cancellation, checkpoints and event subscriptions. Requesting cancellation does not mean execution has stopped.

Process/VM executors clean up their managed process groups; containers use the runtime termination interface. See [Isolation design](isolation.md) for descendants outside the group and platform gaps. AgentCtl provides workload cooperation and checkpoint quiescence, not mandatory enforcement.

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

Review staged files before applying them. OverlayCore validates targets, persists apply intent, updates files and recovers. A batch is not an atomic filesystem transaction. See [Capabilities and evidence](../concepts/capabilities-and-evidence.md) for apply/drop/checkpoint recovery and irreversible effects; see [Review and apply](../guides/review-apply.md) for the workflow.
