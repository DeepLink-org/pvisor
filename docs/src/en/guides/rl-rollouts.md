---
status: todo
search:
  exclude: true
---

# Execution for agentic RL rollouts and evaluation

!!! warning "Planned"
    No results are available yet. This page records the requirements; contributions are welcome.

## Question

Can pVisor support large-scale untrusted agent execution for RL and evaluation?

## Requirements

- Metrics: throughput, isolation, reproducibility, fork cost.
- Controls: RL framework sandboxes and OpenHands-like runtimes.
- Workload: fixed batch rollouts, failure replay, checkpoint forks.
- Environment: clusters or multiple hosts; pinned model/tools.

## Acceptance criteria

- Integration and boundaries.
- Tested recording, fork, and tool-prefix replay.
- Clarify relation to the research design.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [RL substrate](../design/research/rl-execution-substrate.md), [replay](replay.md)

## Assemble one rollout today

One command execution, model capture, and native agent trajectory can form an evaluation sample. Prepare a clean workspace and retain Job ID, Bundle, Journal, and native trajectory. Existing frameworks own reward computation, queues, training, and cross-node scheduling.

| Artifact | Purpose | Does not replace |
| --- | --- | --- |
| Bundle | Outcome, controls, file changes | Reward/dataset metadata |
| Gateway Journal | Model calls reaching Gateway | Uncaptured traffic or native sessions |
| Native trajectory | Adapter-specific prefix replay | Process memory snapshot |
| Logical checkpoint | Fork staged files | Entire environment/external state |

## Sample identity

Record task ID, pVisor commit, agent/model versions, initial repository commit, image digest, sampling, policy, and executor. Distinguish task failure, isolation refusal, recording failure, and continuation quality; infrastructure failure is not model incapability.

Replay executes tools again and can repeat side effects. Use test APIs/databases and separate outputs. Validate with `--prepare-only`, then `--replay-only`, before live continuation. See [replay](replay.md) for pinned versions/boundaries. Cluster throughput and hostile multi-tenant guarantees remain unestablished.
