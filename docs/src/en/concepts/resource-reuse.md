# State preservation and branch reuse

<span id="state-reuse-and-resource-optimization"></span>

When an Agent tries an alternative from existing work, it needs a defined scope of saved state, a point to continue from, and isolated writes for each attempt. pVisor uses checkpoints to preserve a specified scope of state and branches to create independent execution paths.

## Decide which state to preserve {#state-scope}

| State | Content preserved and reused | Main boundary |
| --- | --- | --- |
| Workspace checkpoint | Staged file changes, conflict baselines, and provenance | Does not preserve process memory; base files may remain external references |
| VM execution checkpoint | Consistent CPU, RAM, device, and filesystem state under supported configurations | Depends on platform, configuration, and compatibility; does not roll back external services |
| Agent trajectory | Recorded model interactions and tool calls for replay workflows | Does not represent complete machine state; tool execution may still cause side effects |

A workspace branch starts a new command from saved candidate file changes. An execution branch continues machine execution from a full execution checkpoint. Both require a defined source state and independent writes. See the [CLI reference](../reference/cli.md#full-vm-execution-checkpoints) for current commands and supported configurations, and [Replay recorded trajectories](../guides/replay.md) for trajectory use.

## Keep branches independent {#shared-bases}

A branch retains its source checkpoint relationship and writes subsequent file changes into its own upper layer. Supported execution-restore configurations also keep RAM writes private. This allows alternatives to proceed from the same starting point and their results to be reviewed separately. Workspace checkpoint base files may still reference external paths, so saving the upper layer does not preserve an immutable copy of the entire environment.

Execution checkpoints must capture consistent CPU, RAM, device, and file state and meet restore compatibility requirements. They do not roll back remote APIs or databases with local state. See the [CLI reference](../reference/cli.md#full-vm-execution-checkpoints) for supported configurations and [Environment snapshots](../design/environment-snapshot.md) for consistency mechanisms.

## Preserve state and continue execution {#lifecycle-and-cost}

Under supported native VM configurations, execution checkpoint creation can leave the VM running. `suspend` saves a checkpoint and waits for the original VM to exit; `resume` restores the current suspended state, while an execution branch uses an independent Job. Pausing and offloading a live VM follow a separate runtime lifecycle; see [Sandbox lifecycle management](lifecycle.md).

<span id="resource-mechanisms"></span>

See [Image and memory optimization](resource-optimization.md#resource-mechanisms) for demand loading, sharing, and compression of saved content. Restore capability and resource savings need to be checked separately.
