---
status: todo
search:
  exclude: true
---

# Agentic RL execution substrate

!!! warning "Planned"
    No results are available yet. The requirements are open for contributions.

## Question

What does pVisor need to serve as a substrate for large-scale, untrusted Agentic RL rollouts and evaluation?

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
