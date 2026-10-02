# Job, Operation, Attempt and Event

A **Job** is a persistent CLI unit of work. `pvisor run` creates it; `status`, `kill`, `inspect`, `fork`, `apply` and `drop` operate on it directly. Each current Job has one internal Run record. Disk `run-*` IDs, `run.json` and Run Bundle names remain unchanged for compatibility.

## Operation: the object being processed

Job describes the user's work; Operation describes what pVisor processes. The current production operation is `run.execute`, containing the program, arguments and working directory along with effective policy decisions and Placement. pvisor owns admission, actual rewrites, scheduling and execution; core defines the records.

Events are externally observable facts; Trace records them. They describe requests, actual rewrites, placement and results. They are not the operation itself and do not guarantee replay of external effects from logs alone. See [Operation and Event](operations-events.md) for fields and causality.

## Run: the internal Job record

Run identifies a command, configuration and result independently of the OS PID. After the process exits, local records let `status`, `apply` and `drop` refer to the same work. Use `status --review` to inspect the Run Bundle and staged changes.

## Attempt: one execution

Attempt identifies one execution by an executor. Each current `PVisor::run` creates one Attempt. Session in pvisor owns preparation, cancellation, cleanup and terminal publication.

Fork creates a new Run with lineage from a logical checkpoint; it does not restore the original process.

## Effect: consequences of execution

For staged files, the current workflow is:

```text
run → stop → review → apply selected files → review remaining changes → apply or drop
```

Successful process exit and successful file application are separate results. Files from a failed command remain reviewable; later drop does not undo files already applied.

Network requests and external service changes are effects too. File staging neither delays them until approval nor rolls them back.

## Checkpoint: filesystem snapshot

The `fork` command snapshots the upper layer of a stopped staged Run. The embedded API also supports cooperative AgentCtl quiescence. Checkpoints preserve staged files, the conflict baseline captured at first modification and lineage. They do not save process memory or external service state, or freeze all lower layers. The manifest is published only after snapshot contents are synced to disk. Embedded callers use `restore_logical_checkpoint(checkpoint, destination_upper, destination_preimages)` to restore files and conflict baselines together.

See [Review and apply](../guides/review-apply.md) for steps and [Capabilities and evidence](../concepts/capabilities-and-evidence.md) for guarantees.
