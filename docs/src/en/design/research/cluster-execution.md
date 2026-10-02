---
status: todo
search:
  exclude: true
---

# Cluster execution (L3)

!!! warning "Planned"
    No results are available yet. The requirements are open for contributions.

## Question

Where are the boundaries of cross-node scheduling, centralized evidence and batch delegation, and how does pVisor divide responsibility with Kubernetes/Ray?

## Requirements

- Metrics: cross-node scheduling throughput, centralized-evidence audit cost, single-cluster concurrency limit.
- Controls: native Kubernetes and Ray scheduling.
- Workload: batch agent execution across multiple nodes.
- Environment: multi-machine cluster; pinned scheduler versions.

## Acceptance criteria

- pVisor defines execution semantics without replacing schedulers.
- Gaps and phases.
- Align with density measurements.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [Trust ladder](../../why/trust-ladder.md), [parallel agents](../../guides/parallel-agents.md)
