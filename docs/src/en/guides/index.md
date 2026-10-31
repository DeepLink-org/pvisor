# Agent style stagebox: from experiment to accepted results

Use the [offline first run](../start/first-run.md#agent-style) to learn staging. For an existing project, give the agent a task you can check and verify the accepted files in the target environment.

<div class="pv-guide-steps" markdown="1">

1. ### Prepare a task you can check {#prepare}

    Complete [installation and platform setup](../start/installation.md), then validate staging and review with the offline task in [your first run](../start/first-run.md). The demonstration needs no model account and lets you compare the host workspace with candidate files in the Job.

    For a real agent, use the [agent integration guides](agents/index.md) to check its installation location, credentials, and required network destinations. Once these are ready, replace the offline command with the agent or your script.

    Give the task an explicit goal and acceptance criteria, such as “fix this test; allow source and test changes; preserve other files.” Record the starting repository state so review has a baseline to compare against.

    Start native Jobs with `pvisor spawn`. The `pvisor run` command starts standalone containers and is separate from the native Job staging and review entry point. See the [CLI reference](../reference/cli.md) for usage.

1. ### Choose execution boundaries {#boundaries}

    Select an [executor](executors/index.md) for the tools you need, and configure [file access](policies/files.md), [network egress](policies/network.md), and [credentials](policies/credentials.md) separately. Inspect installed controls; staged files alone do not establish network or read isolation.

    Explicitly select staging when file changes need acceptance afterward: `--safe` requests preset controls and stages the workspace; `--stage PATH` selects its storage location. Ordinary host execution writes through by default. See [CLI staging and storage](../reference/cli.md#暂存与存储) for writes and cleanup at each location.

    For a custom stage, choose a fresh directory outside the project for each task. Address subsequent commands through that path or an explicit Job identity. [Jobs and storage](../concepts/jobs.md) defines the lookup scope of `last`.

1. ### Run a task with acceptance criteria {#run-task}

    Open a terminal at the target project root and record existing edits:

    ```bash title="Shell"
    git status --short
    ```

    Keep those edits as your baseline. Complete authentication and network setup in [agent integration](agents/index.md), then launch an agent, for example:

    ```bash title="Shell"
    pvisor spawn --safe --pass-env ANTHROPIC_API_KEY -- claude
    ```

    Give the agent a checkable task, such as “fix the specified failing test, run that test, and modify only related source and tests.” Wait for the task to finish and exit the agent before review. For authentication failure, nonzero tool exits, or cancellation, inspect `pvisor status last` and `pvisor review last --diff` first. Incomplete records do not establish that no changes occurred; follow [troubleshooting](troubleshooting.md) before proceeding.

1. ### Decide after execution {#review}

    After the task ends, check the outcome, exit code, and installed controls before reviewing net file changes. Command success does not establish that the changes satisfy the task; a nonzero exit can still leave useful candidates.

    | Outcome | Next action | What to retain |
    | --- | --- | --- |
    | Changes meet the requirements | Apply selected paths; handle any remaining candidates | Execution evidence and the final accepted result |
    | Some changes are useful | Accept in batches, or save a file branch before cleanup | Unapplied candidates and conflict baselines |
    | Execution failed or changes are unsuitable | Diagnose the failure, then discard candidates or rerun | Failure diagnostics and inputs needed to reproduce it |
    | Another operation changed host files | Inspect and explicitly resolve the conflict | Current host files and the original stage |

    [Review and apply changes](review-apply.md) covers commands, read-only inspection, and conflict recovery. `drop` handles unaccepted file candidates; [staging semantics](../concepts/staging.md) defines the scope of external calls and already-applied batches.

    To keep a file proposal while trying another approach, check [workspace fork](fork-checkpoint.md) prerequisites first. To continue CPU, RAM, and device state, choose an [execution-checkpoint interface](infrastructure/integration.md#capabilities).

1. ### Validate and hand off results {#handoff}

    Review file differences, then inspect files in the read-only task view:

    ```bash title="Shell"
    pvisor review last --diff
    pvisor inspect last -- git diff --check
    ```

    The Git check applies to Git projects and checks formatting, not functionality. `inspect` is read-only. Run tests that write caches or artifacts during task execution, then validate again in the target environment after acceptance.

    Check which changes belong in the project and which are experiment residue, including dependencies between source, tests, and configuration. Review displays file changes; you decide how to classify them. It does not certify files as safe. Accept paths that exist in your project, for example when both source and tests are needed:

    ```bash title="Shell"
    pvisor apply last --path src --path tests
    git diff --stat
    ```

    Run your project's test command in the original project afterward. Partial acceptance can omit dependencies or configuration. If target-environment validation fails, inspect the unaccepted changes and retain the stage while deciding. Apply does not create a Git commit; review existing workspace edits as well.

    Run `pvisor drop last` only once you know the remaining candidates are unnecessary. If all changes were accepted, staging is already complete and no drop is needed. Drop does not undo accepted files. Use a separate stage for a new attempt; to preserve a file proposal while branching, follow [fork prerequisites](fork-checkpoint.md). Workspace branching does not restore VM execution state.

</div>

## Trajectory capture and replay {#trajectories}

For local tasks, use [trajectory capture](capture.md) to track model interactions and [replay](replay.md) to rebuild supported agent histories and continue execution. The same guides support [context preservation](infrastructure/index.md#inference) in inference and evaluation; see [RL rollouts](rl-rollouts.md) when delivering execution results to a training workflow.

## Extend to parallel tasks and CI {#automation}

Give parallel tasks separate workspaces, stages, and Job identities. Collect results with the [parallel-agent workflow](parallel-agents.md#batch), then coordinate applying them to one target tree. You or your review process still determine acceptance as task volume increases.

[CI workflows](ci.md) separate execution outcomes, artifact upload, and accepting changes. Retain failure and timeout evidence so review can decide whether to rerun, repair, or discard.

Diagnose failures with [troubleshoot a Run](troubleshooting.md).

When an inference service, evaluator, or trainer starts the tasks, use [agent execution infrastructure](infrastructure/index.md) to assign state and resource ownership. Both workflows share executor and capability documentation.
