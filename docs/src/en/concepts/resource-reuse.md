# State preservation and branch reuse

<span id="state-reuse-and-resource-optimization"></span>

After an Agent has completed part of a task, it may need to preserve progress, try another approach or continue later. pVisor uses checkpoints to save a starting point and branches to isolate subsequent changes, allowing attempts to proceed independently, be reviewed separately and retain their provenance.

## Workspace checkpoints: preserve file progress {#state-scope}

A workspace checkpoint saves staged file changes, conflict baselines and provenance. A workspace branch starts a new command from that file state, writes subsequent changes into its own upper layer and leaves the original candidates available.

For example, an Agent has produced a code change but tests still fail. Save the current file state, let another branch try a different repair, then review both approaches.

This checkpoint does not preserve process memory or model conversations. Base files may still reference external paths, so it is not an immutable backup of the entire environment. Ordinary workspace branching requires the source Job to have stopped; capture during execution requires a cooperative quiescence point. See [Logical checkpoints and forks](../guides/fork-checkpoint.md) for operations and saved state.

## VM execution checkpoints: preserve execution progress {#execution-checkpoints}

Under supported configurations, a VM execution checkpoint consistently captures CPU, RAM, device and filesystem state. Restoration rebuilds an execution instance and continues from the saved position; execution branching can create an independent Job from the same checkpoint.

For example, save execution state after environment initialization or loading data into memory, then use it for restoration or branching. Whereas workspace branching starts a new command, execution branching continues the saved program and does not accept a replacement command.

Creating an execution checkpoint can leave the original VM running. Suspending saves the checkpoint and waits for that VM to exit. Restoring a suspended task preserves Job identity and creates a new Attempt; execution branching creates an independent Job. [Live Resume](lifecycle.md#checkpoint-boundary) continues the original live instance and the same Attempt.

## Create independent branches from the same starting point {#shared-bases}

Both branch types record their source checkpoint and write subsequent file changes into separate upper layers. Supported VM execution branches also keep RAM writes private. Branches can execute and have their results inspected separately; the Agent or framework still chooses strategies and compares outcomes.

Isolation covers the state managed by the branch. It cannot roll back remote APIs, databases or messages already sent. For recorded model interactions and tool calls, see [Replay recorded trajectories](../guides/replay.md).

## Supported scope {#lifecycle-and-cost}

| Capability | What to check before use |
| --- | --- |
| Workspace checkpoints and branches | Keep supported staged candidates when creating a checkpoint. Branching from an existing checkpoint requires that checkpoint and its referenced base files. |
| VM execution checkpoints and branches | Use a native-save-capable Linux x86_64 or macOS ARM64 VM configuration with networking off and private RAM. Restoration requires the same host boot, pVisor binary and firmware. |

VM execution restoration excludes host root `/` and cannot be arbitrarily combined with shared memory pools, writable RAM backing or cold-page compression. It is not cross-host migration or cross-version restoration. See the [CLI reference](../reference/cli.md#full-vm-execution-checkpoints) for full configuration restrictions and capability queries, and [Environment snapshots](../design/environment-snapshot.md) for consistency mechanisms.

<span id="resource-mechanisms"></span>

**State reuse preserves task progress; resource reuse reduces duplicate storage of underlying content.** File and RAM modifications must remain independent across branches; whether immutable content can be shared depends on the path. The checkpoint configuration's private-RAM requirement above still applies, and creating a branch does not automatically enable shared memory pools or compression. See [Image and memory optimization](resource-optimization.md#resource-mechanisms) for the mechanisms.
