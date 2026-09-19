# Architecture

PolicyVisor (pVisor) combines capability admission, executors, runtime controls, and execution records for Agent CLIs, scripts, and automation commands. Current delivery centers on a local Run and its reviewable results; fleet scheduling remains a design direction.

## Component ownership

| Crate | Responsibility |
| --- | --- |
| `persisting-pvisor` | CLI, admission, Attempt lifecycle, executors, Run Bundle, review/apply/checkpoint |
| `persisting-agentctl` | Control contracts, capability policy, cooperative AgentCtl protocol and client |
| `persisting-overlay-core` | Shared copy-on-write semantics and first-touch file fingerprints |
| `persisting-overlayfs` | Host FUSE adapter and optional Jujutsu backend |
| `persisting-overlaynet` | Network authorization, resolution, proxy forwarding and VM network attachment |
| `persisting-gateway` | Model routing, protocol conversion and capture |
| `persisting-events` | Shared event envelope and identities |
| `persisting-replay` | Agent-native trajectory replay and continuation adapters |

## Execution path

```text
CLI/config → RunSpec → capability admission → prepare runtime drivers
  → executor starts the command → exit/cancel/deadline → cleanup
  → local Run record + Run Bundle + terminal result
  → later review / apply / drop
```

The current run path creates one Attempt. Host execution drains bounded output and cleans its process group after the leader exits, including deadline and cancellation paths. A descendant that leaves the process group is outside process-group cleanup; a bounded output drain prevents its inherited pipe from blocking Run completion. Stronger descendant containment depends on the selected platform mechanism.

## Filesystem application

OverlayCore records the target's original state on first mutation. Apply closes a selection over required directories and hard-link siblings, validates affected preimages, writes a durable intent, updates the target, and then consumes applied upper entries.

Recursive directory deletion or replacement validates recorded descendants as well as the directory. A partial apply retains updated directory baselines needed by pending children. Recovery distinguishes `Prepared`, `TargetApplied`, and `Committed` so a partly pruned upper is not reinterpreted as a new change.

Individual file replacements use temporary files and rename. A whole batch is not one atomic filesystem transaction: an interrupted batch can be partially visible and needs recovery. The target lock serializes pVisor apply operations; it does not lock out editors or unrelated writers. Stop concurrent writers while applying.

## Records and capture

`run.json` is the local Run record. `run-bundle.json` summarizes the result, controls, artifacts and filesystem/network evidence. These are distinct from optional EventRecord JSONL containing lifecycle and Gateway events.

Gateway uses bounded apply queues and a best-effort asynchronous WAL. Queue acceptance is not a synchronous durable commit. Capture and protocol conversion must not be described as proof that all network traffic was intercepted.

## Extension points and limits

Executor and sink interfaces support embedding. They do not imply automatic retry scheduling, exactly-once external actions, distributed transactions, or cryptographic node attestation. Future placement ideas belong in [local to fleet](local-to-fleet.md); platform enforcement belongs in [isolation](isolation.md).
