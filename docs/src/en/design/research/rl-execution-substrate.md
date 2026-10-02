---
status: todo
search:
  exclude: true
---

# Agentic RL execution substrate

!!! warning "Planned"
    No results are available yet. This page records the requirements; contributions are welcome.

## Question

What is required for large-scale untrusted rollouts and evaluation?

## Requirements

- Metrics: throughput, isolation, reproducibility, fork cost.
- Controls: RL framework sandboxes.
- Workload: fixed rollouts, failure replay, checkpoint forks.
- Environment: pinned model/tools.

## Acceptance criteria

- Integration and scope.
- Tested recording, fork, and prefix replay.
- Separate design from the rollout guide.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [Rollouts](../../guides/rl-rollouts.md), [replay design](../replay.md)
