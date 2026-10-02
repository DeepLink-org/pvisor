# Logical checkpoints and forks

Before applying all or dropping, fork the filesystem state into a new Job:

```bash
pvisor fork last -- codex
```

## Behavior

- Source must be stopped. Fork checkpoints staged files and starts a child Job.
- Child records lineage; source changes remain independently reviewable/applicable/discardable.
- `--checkpoint ID` selects an existing snapshot.
- Without a new command, the child reuses the source command.

## Checkpoint contents

| Saved | Not saved |
| --- | --- |
| Staged workspace upper | Process memory/running state |
| First-touch conflict preimages | External services and prior network effects |
| Source lineage | Immutable copies of every host lower file |

Fork restarts from file changes rather than restoring a process. Snapshot content is synced before manifest publication.

## Embedded API

RunHandle::checkpoint uses AgentCtl quiesce directives and matching quiesced Session reports, snapshots upper, then publishes continue. Cooperative clients can freeze file state at a safe point; arbitrary subprocesses do not become restorable. restore_logical_checkpoint restores upper and preimages together. See [execution model](../design/execution-model.md).
