---
status: todo
search:
  exclude: true
---

# Comparison: agent RL rollout infrastructure

!!! warning "Planned"
    No data yet. Below are the requirements; contributions are welcome.

## Question

How does this compare with existing RL rollout environments?

## Requirements

- Metric: isolation, trajectory recording, forking and replay, concurrency density, and how it integrates with the training framework.
- Control group: the OpenHands runtime; SWE-Gym-like environments; RL frameworks' sandbox components.
- Workload: batch rollouts with failure replay and checkpoint forking.
- Environment: pinned model and tool versions.

## Acceptance criteria

- State the integration method and boundaries clearly.
- Cite benchmarks/density and replay-fidelity data.
- Provide a correction route.

## Tracking

- Tracking issue: TODO
- Owner: TODO
- Related: [comparisons](../why/comparisons.md), [methodology](methodology.md)
