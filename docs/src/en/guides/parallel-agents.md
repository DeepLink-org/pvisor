---
status: todo
search:
  exclude: true
---

# Run many agents on one host and review them in batches

!!! warning "Planned"
    No results are available yet. This page records the requirements; contributions are welcome.

## Question

When several agents run on one machine at once, how does isolation work, how do you review them in batches, and what is the host limit?

## Requirements

- Metrics: number of concurrent Jobs, CPU/memory cost per Job, tail latency.
- Control: sequential runs; Docker at the same density.
- Workload: a fixed task set at 8/32/128 concurrent Jobs.
- Environment: one set each for Linux (KVM/FUSE) and macOS (HVF/macFUSE).

## Acceptance criteria

- A host concurrency limit and resource model.
- A reproducible batch-review flow (aggregate by workspace, apply in batches).
- Alignment with benchmarks/density.

## Tracking

- Tracking issue: TODO
- Owner: TODO
- Related pages: [Use cases](../why/use-cases.md), [density](../benchmarks/density.md)

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

Validate with two tasks first, then record processes, FUSE mounts, VM memory, disk, model-service quota, and proxy-port use. Give each Job a non-conflicting port; do not publish 8/32/128 capacity claims without data. Read the Bundle for the actual effect of resource limits; see [concurrency density (planned)](../benchmarks/density.md).
