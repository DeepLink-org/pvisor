# Run, Attempt, and Effect

## Run: one managed invocation

A Run identifies the command, configuration, and execution result. Its ID is independent of the operating-system PID. The local record lets `status`, `review`, `apply`, and `drop` refer to the same work after the process exits.

## Attempt: one execution

An Attempt identifies execution by a selected provider. The current `PVisor::run` path creates one Attempt per invocation. The contracts carry attempt and lease identities, but pVisor does not automatically schedule distributed retries.

A fork creates a new Run with lineage to a logical checkpoint; it does not resume the original process.

## Effect: a consequence of execution

For staged files, the supported decision loop is:

```text
run → stop → review → apply selected files → review the remainder → apply or drop
```

A successful process exit and a successful file application are separate results. You can review files produced by a failed command. Applied files are no longer undone by dropping the remaining stage.

Network requests and external service mutations are also consequences, but filesystem staging does not hold them for later approval or roll them back.

## Checkpoint: a filesystem snapshot

The CLI snapshots the upper layer of a stopped staged Run. The embedded API also supports cooperative AgentCtl quiescence. A checkpoint preserves staged files and lineage, not process memory, external services, or an immutable copy of every lower layer.

Read [review and apply](../guides/review-apply.md) for the operational workflow and [capabilities and evidence](capabilities-and-evidence.md) for execution guarantees.
