# Logical checkpoints and forks

Workspace fork keeps a file proposal so you can try another approach with a new command. The parent must be a stopped host Job with reconstructable launch policy, parent and child stages must share a filesystem, and you must fork before a full apply or drop cleans up staging data. VM/container Jobs and Jobs with Gateway routes do not support this workspace fork path.

Once these conditions hold, fork into a new Job so the agent can continue a different way:

```bash
pvisor fork last -- codex
```

`last` searches default storage only. If the parent Job uses an explicit stage, replace it with that stage path or Job ID; see [Jobs and storage](../concepts/jobs.md) for selection rules.

## Behavior

- `fork` requires the source Job to be stopped. It first creates a logical checkpoint of the staged filesystem, then starts a child Job.
- The child records its lineage from the source Job. The source's staged changes are untouched and can still be reviewed, applied, or dropped on their own.
- `--checkpoint ID` reuses an existing logical checkpoint.
- Without a command after `--`, the child reuses the source Job's command.

## What a checkpoint saves

| Saved | Not saved |
| --- | --- |
| The staged workspace upper | Process memory and running state |
| The conflict preimages recorded at first modification | External service state and network calls that already happened |
| Lineage from the source Job | Immutable copies of every host lower file |

The child starts a new command from these file changes; it does not continue the original process memory. Snapshot content is flushed to disk before the manifest is published. To continue VM execution state, use an [execution checkpoint](../reference/cli.md#full-vm-execution-checkpoints) and check its supported profile instead.

## Embedded API

A host that embeds pVisor can call `RunHandle::checkpoint`: pVisor publishes a quiesce directive through AgentCtl, requires every Session included in the checkpoint to report a matching quiesced state, snapshots the upper, then publishes `continue`. This lets cooperating clients freeze file state at a safe point, but it does not turn arbitrary subprocesses into resumable processes. Restore with `restore_logical_checkpoint(checkpoint, destination_upper, destination_preimages)`, which restores files and conflict preimages together. See the [execution model](../design/execution-model.md) for the mechanism.
