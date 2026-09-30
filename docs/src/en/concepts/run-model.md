# Job, Run, Attempt, and Effect

A **Job** is the CLI's durable unit of work. `pvisor run` starts one;
`status`, `kill`, `inspect`, `fork`, `apply`, and `drop` act on it without an
extra command layer. Each Job currently corresponds to one internal Run record.
The existing `run-*` ID, `run.json`, and Run Bundle names remain on disk for
compatibility.

## Run: the internal record of a Job

A Run identifies the command, configuration, and execution result. Its ID is independent of the operating-system PID. The local record lets `status`, `apply`, and `drop` refer to the same work after the process exits. Use `status --review` for the Run Bundle and staged changes.

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

The `fork` command snapshots the upper layer of a stopped staged Run. The embedded API also supports cooperative AgentCtl quiescence. A checkpoint preserves staged files, first-touch conflict baselines, and lineage, not process memory, external services, or an immutable copy of every lower layer. Snapshot contents are synced before the manifest is published. Embedded callers restore both directories with `restore_logical_checkpoint(checkpoint, destination_upper, destination_preimages)`.

Read [review and apply](../guides/review-apply.md) for the operational workflow and [capabilities and evidence](capabilities-and-evidence.md) for execution guarantees.
