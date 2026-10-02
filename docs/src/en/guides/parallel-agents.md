---
status: todo
search:
  exclude: true
---

# Local parallel agents and batch review

!!! warning "Planned"
    No results are available yet. This page records the requirements; contributions are welcome.

## Question

How do isolation, batch review, and host capacity work for concurrent agents?

## Requirements

- Metrics: concurrency, per-Job CPU/memory, tail latency.
- Controls: sequential runs and Docker at matching density.
- Workload: fixed tasks at 8/32/128 Jobs.
- Environment: Linux KVM/FUSE and macOS HVF/macFUSE.

## Acceptance criteria

- Host limits and resource model.
- Reproducible workspace aggregation and batch apply review.
- Align with density benchmarks.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [Use cases](../why/use-cases.md), [density](../benchmarks/density.md)

## Manual workflow with existing tools

Run one Job in each of two independent checkouts/worktrees, with separate outside-project stages and explicit selectors. Applying one does not mutate the other's lower layer. Each Job still needs its own network/credential policy.

```bash
# Terminal A / workspace A
pvisor run --safe --stage ../stage-a -- codex
# Terminal B / workspace B
pvisor run --safe --stage ../stage-b -- claude

pvisor status --review ../stage-a
pvisor status --review ../stage-b
```

This is not a batch scheduler. Apply to each matching workspace and integrate through normal Git. Do not merge upper directories or apply concurrently into one target tree.

## Shared workspace and fork limits

Fork preserves a common staged starting point but does not freeze all lowers. Applying one branch can conflict with another; refusal protects edits and must not be bypassed by deleting preimages. Pin workspace baseline, tools, and image digests for repeatable inputs.

## Capacity

Start with two tasks, measure processes, mounts, VM memory, disk, model-service quotas, and proxy ports. Allocate nonconflicting ports per Job. Do not claim 8/32/128 capacity without data. Check effective resource limits in the Bundle; see [density (planned)](../benchmarks/density.md).
