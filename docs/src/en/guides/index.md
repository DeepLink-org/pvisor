# Local agent workflows

With file staging enabled, let an agent complete a project task, then review candidate changes and choose the paths to apply. A refactor, fix, or scripted task follows “prepare → execute → review → accept or discard.”

## Prepare a task you can check {#prepare}

Complete [installation and platform setup](../start/installation.md), then validate staging and review with the offline task in [your first run](../start/first-run.md). The demonstration needs no model account; afterward, you should be able to distinguish the host workspace from candidate files in the Job.

For a real agent, use the [agent integration guides](agents/index.md) to check its installation location, credentials, and required network destinations. Once these are ready, replace the offline command with the agent or your script.

Give the task an explicit goal and acceptance criteria, such as “fix this test; allow source and test changes; preserve other files.” Record the starting repository state so review has a baseline to compare against.

Start native Jobs with `pvisor spawn`. The `pvisor run` command starts standalone containers and is separate from the native Job staging and review entry point. See the [CLI reference](../reference/cli.md) for usage.

## Choose execution boundaries {#boundaries}

Select an [executor](executors/index.md) for the tools you need, and configure [file access](policies/files.md), [network egress](policies/network.md), and [credentials](policies/credentials.md) separately. Inspect installed controls; staged files alone do not establish network or read isolation.

Explicitly select staging when file changes need acceptance afterward: `--safe` requests preset controls and stages the workspace; `--stage PATH` selects its storage location. Ordinary host execution writes through by default. See [CLI staging and storage](../reference/cli.md#暂存与存储) for writes and cleanup at each location.

For a custom stage, choose a fresh directory outside the project for each task. Address subsequent commands through that path or an explicit Job identity. [Jobs and storage](../concepts/jobs.md) defines the lookup scope of `last`.

## Decide after execution {#review}

After the task ends, check the outcome, exit code, and installed controls before reviewing net file changes. Command success does not establish that the changes satisfy the task; a nonzero exit can still leave useful candidates.

| Outcome | Next action | What to retain |
| --- | --- | --- |
| Changes meet the requirements | Apply selected paths; handle any remaining candidates | Execution evidence and the final accepted result |
| Some changes are useful | Accept in batches, or save a file branch before cleanup | Unapplied candidates and conflict baselines |
| Execution failed or changes are unsuitable | Diagnose the failure, then discard candidates or rerun | Failure diagnostics and inputs needed to reproduce it |
| Another operation changed host files | Inspect and explicitly resolve the conflict | Current host files and the original stage |

[Review and apply changes](review-apply.md) covers commands, read-only inspection, and conflict recovery. `drop` handles unaccepted file candidates; [staging semantics](../concepts/staging.md) defines the scope of external calls and already-applied batches.

To keep a file proposal while trying another approach, check [workspace fork](fork-checkpoint.md) prerequisites first. To continue CPU, RAM, and device state, choose an [execution-checkpoint interface](infrastructure/integration.md#capabilities).

## Extend to parallel tasks and CI {#automation}

Give parallel tasks separate workspaces, stages, and Job identities. Collect results with the [parallel-agent workflow](parallel-agents.md#batch), then coordinate applying them to one target tree. You or your review process still determine acceptance as task volume increases.

[CI workflows](ci.md) separate execution outcomes, artifact upload, and accepting changes. Retain failure and timeout evidence so review can decide whether to rerun, repair, or discard.

Use [trajectory capture](capture.md) to track model interactions and [replay](replay.md) to restore supported agent histories. Diagnose failures with [troubleshoot a Run](troubleshooting.md).

When an inference service, evaluator, or trainer starts the tasks, use [agent execution infrastructure](infrastructure/index.md) to assign state and resource ownership. Both workflows share executor and capability documentation.
