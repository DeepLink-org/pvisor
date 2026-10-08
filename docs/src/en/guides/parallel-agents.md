# Run many agents on one host and review them in batches

Give every parallel agent its own workspace and Stage. Each result then has a separate baseline, log, and change set, making it easy to review without mixing outputs from different agents.

Prepare two workspaces from the Git repository root:

```bash
git worktree add --detach ../pvisor-task-a HEAD
git worktree add --detach ../pvisor-task-b HEAD
```

Enter each directory separately and launch the commands below. Keep each Stage outside its workspace. Start with two concurrent tasks, measure resources using the [density experiment](../benchmarks/density.md), then increase concurrency.

## A manual workflow the current tools can do

Run one Job in each of two independent checkouts/worktrees, with a separate stage outside the project and an explicit selector for each task. The lower layer then does not change when the other task applies, so you can review the results separately; networking and credentials are still decided by each Job's policy.

```bash
# Terminal A / workspace A
pvisor run --safe --stage ../stage-a -- codex
# Terminal B / workspace B
pvisor run --safe --stage ../stage-b -- claude

pvisor status --review ../stage-a
pvisor status --review ../stage-b
```

This is not a batch scheduling interface. Once you pick a result, merge it per workspace and integrate through your existing Git flow; do not copy and merge the upper directories of several tasks, and do not apply concurrently into the same target tree.

## Shared-workspace and fork limits

Forking from the same stopped Job preserves a shared staged starting point, but the checkpoint does not freeze every lower. After the first branch applies, the other branch and the host can conflict; that refusal is a protection mechanism and must not be bypassed by deleting preimages. To reproduce the same input, pin the workspace baseline, tool versions, and image digests.

## What decides how many can run

Validate with two tasks first, then record processes, FUSE mounts, VM memory, disk, model-service quota, and proxy-port use. Give each Job a non-conflicting port; do not publish 8/32/128 capacity claims without data. Read the Bundle for the actual effect of resource limits; see [concurrency density](../benchmarks/density.md).

## Validate the batch workflow with two offline tasks {#batch}

Start with shell tasks to verify independent workspaces and records. Run this Bash script from the original repository directory using the two worktrees created above. Each task creates its own file; the host must support safe tasks.

```bash
(
  cd ../pvisor-task-a
  pvisor run --safe --overlaynet-deny-all --stdio capture \
    --stage ../stage-batch-a -- /bin/sh -c 'printf "A\n" > result-a.txt'
) &
pid_a=$!
(
  cd ../pvisor-task-b
  pvisor run --safe --overlaynet-deny-all --stdio capture \
    --stage ../stage-batch-b -- /bin/sh -c 'printf "B\n" > result-b.txt'
) &
pid_b=$!
code_a=0
code_b=0
wait "$pid_a" || code_a=$?
wait "$pid_b" || code_b=$?
printf 'A=%s B=%s\n' "$code_a" "$code_b"

pvisor status --review ../stage-batch-a
pvisor status --review ../stage-batch-b
pvisor inspect ../stage-batch-a -- cat result-a.txt
pvisor inspect ../stage-batch-b -- cat result-b.txt
```

Expect two zero exit codes, staged files containing A and B, and no new files in either worktree yet. After review, accept A and discard B:

```bash
pvisor apply ../stage-batch-a --path result-a.txt
pvisor drop ../stage-batch-b
git -C ../pvisor-task-a diff --stat
git -C ../pvisor-task-a status --short
```

A's file is written back to its own worktree. A new file appears in `git status`; ordinary `git diff` does not show it before it is added to the index. Test and commit in that worktree, then integrate through your project's merge process.

When substituting agents, keep a separate Stage for every task and configure credentials and networking using [Codex](agents/codex.md) or [Claude Code](agents/claude-code.md). Map each result to its workspace before reviewing and applying it; your pipeline controls merge ordering for a shared target tree.
