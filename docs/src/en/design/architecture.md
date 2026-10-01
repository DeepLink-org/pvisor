# Architecture

PolicyVisor (pVisor) combines capability admission, executors, runtime controls, and execution records for Agent CLIs, scripts, and automation commands. Current delivery centers on a local Run and its reviewable results; fleet scheduling remains a design direction.

## Component ownership

| Crate | Responsibility |
| --- | --- |
| `persisting-pvisor` | CLI, admission, Attempt lifecycle, executors, Run Bundle, review/apply/checkpoint |
| `persisting-control` | Run/Overlay contracts, capability policy, control messages/client, and shared event records |
| `persisting-overlay-core` | Shared copy-on-write semantics and first-touch file fingerprints |
| `persisting-overlayfs` | Host FUSE adapter |
| `persisting-overlaynet` | Network authorization, resolution, proxy forwarding and VM network attachment |
| `persisting-gateway` | Model routing, protocol conversion and capture |
| `persisting-replay` | Agent-native trajectory replay and continuation adapters |

## Execution path

```text
CLI/config → RunSpec → capability admission → RunPlan IR → prepare runtime drivers
  → executor starts the command → exit/cancel/deadline → cleanup
  → local Run record + Run Bundle + terminal result
  → later review / apply / drop
```

The current run path creates one Attempt. Host execution drains bounded output and cleans its process group after the leader exits, including deadline and cancellation paths. A descendant that leaves the process group is outside process-group cleanup; a bounded output drain prevents its inherited pipe from blocking Run completion. Stronger descendant containment depends on the selected platform mechanism.

## Run IR and observations

`RunStatus.attempt.executor` describes the admission-time control plan; it is not
proof that setup has completed. `PVisor::capabilities()` reports host mechanisms,
not a particular Run's installed controls. Final Bundle safety fields account
for sandbox setup failures and runtime observations.

Admission resolves the executor, application policy and network configuration before compiling an immutable `RunPlan` from the effective `RunSpec`. Its `run.execute` IR request and ordered VM/Overlay context rewrites describe placement. Each filesystem, network and environment rule has a stable ID within the Run, a target, an action and the actual enforcement evidence for its dimension. Embedders can call `PVisor::resolve_run_plan` to inspect the same plan without starting an Attempt.

Run events carry IR request, rewrite and completion facts. `run.json` and the Run Bundle retain the plan. IR is the internal framework for rewrites and effect accounting; it adds no CLI entry point. The FUSE filesystem view counts hits, successes, denials, other failures, successful mutating operations, failed mutating operations with uncertain effects, and read/write bytes by mount-relative path and operation. Matched deny/warn rules and the stage rule receive counters too. The path table retains at most 8192 distinct paths and counts further hits in `overflow_hits`. These are operations reaching FUSE, not unique files or the final filesystem diff; OverlayFS diff remains the source for final changes.

The bundle also records aggregate OverlayNet allow, deny, failure and byte counters. Filesystem grants outside FUSE and network traffic outside an interceptor have no reliable per-rule counters, so their fields are `null`: unknown is not zero. A selective host proxy counts only traffic that reaches it and cannot prove that no connection bypassed it.

## Filesystem application

`persisting-control::overlay` owns the review/apply records, first-touch state schema, and local Run inspection messages. OverlayCore computes fingerprints and stores journals; pVisor handles requests and executes apply/discard.

OverlayCore records the target's original state on first mutation. Apply closes a selection over required directories and hard-link siblings, validates affected preimages, writes a durable intent, updates the target, and then consumes applied upper entries.

Recursive directory deletion or replacement validates recorded descendants as well as the directory. A partial apply retains updated directory baselines needed by pending children. Recovery distinguishes `Prepared`, `TargetApplied`, and `Committed` so a partly pruned upper is not reinterpreted as a new change.

Individual file replacements use temporary files and rename. A whole batch is not one atomic filesystem transaction: an interrupted batch can be partially visible and needs recovery. The target lock serializes pVisor apply operations; it does not lock out editors or unrelated writers. Stop concurrent writers while applying.

## Records and capture

`run.json` is the local Run record. `run-bundle.json` summarizes the result, controls, artifacts and filesystem/network evidence. These are distinct from optional Trace Event journals containing lifecycle and Gateway events.

Gateway uses bounded apply queues and the shared fact Journal. Queue acceptance is not a durable commit; committed events rebuild projections after restart. Capture and protocol conversion must not be described as proof that all network traffic was intercepted.

## Extension points and limits

Executor and sink interfaces support embedding. They do not imply automatic retry scheduling, exactly-once external actions, distributed transactions, or cryptographic node attestation. Future placement ideas belong in [local to fleet](local-to-fleet.md); platform enforcement belongs in [isolation](isolation.md).
