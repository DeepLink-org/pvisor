---
status: todo
search:
  exclude: true
---

# Cluster execution (L3)

!!! warning "Planned"
    No results are available yet. This page records the requirements; contributions are welcome.

## Question

How do scheduling, centralized evidence, and delegation divide responsibility with Kubernetes/Ray?

## Requirements

- Metrics: scheduling throughput, audit cost, cluster concurrency.
- Controls: native Kubernetes/Ray scheduling.
- Workload: batch agents on multiple nodes.
- Environment: pinned scheduler versions.

## Acceptance criteria

- pVisor defines execution semantics without replacing schedulers.
- Gaps and phases.
- Align with density measurements.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [Trust ladder](../../why/trust-ladder.md), [parallel agents](../../guides/parallel-agents.md)
