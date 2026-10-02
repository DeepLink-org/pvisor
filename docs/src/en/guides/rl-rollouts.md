---
status: todo
search:
  exclude: true
---

# An execution layer for agentic RL rollouts and evaluation

!!! warning "Planned"
    No results are available yet. This page records the requirements; contributions are welcome.

## Question

Can pVisor serve as a substrate for large-scale untrusted execution in agentic RL rollouts and evaluation?

## Requirements

- Metrics: rollout throughput, isolation effectiveness, trajectory reproducibility, fork cost.
- Control: the sandbox components of existing RL frameworks; OpenHands-runtime-style approaches.
- Workload: batch rollouts of fixed tasks, including failure replay and forking from a checkpoint.
- Environment: cluster or multi-host environments; pinned model and tool versions.

## Acceptance criteria

- The integration with training frameworks and its boundaries.
- Tested behavior for trajectory recording, forking, and tool-prefix replay.
- An explicit relation to design/research/rl-execution-substrate.

## Tracking

- Tracking issue: TODO
- Owner: TODO
- Related pages: [RL execution substrate](../design/research/rl-execution-substrate.md), [replay](replay.md)

## How to assemble one rollout today

An evaluation sample today can be one command execution, its model traffic capture, and the native agent trajectory. Prepare a clean workspace for the sample, then run the command and keep the Job ID, Run Bundle, Event Journal, and native agent trajectory. Reward computation, the task queue, model training, and cross-node scheduling stay with your existing framework.

| Artifact | Purpose | Does not replace |
| --- | --- | --- |
| Run Bundle | Outcome, controls, and file changes | Training-framework reward and dataset metadata |
| Gateway Journal | Model calls through the Gateway | Uncaptured traffic and native agent sessions |
| Native agent trajectory | Tool-prefix replay for the matching adapter | Process memory snapshots |
| Logical checkpoint | Forking staged file state | Full environment and external service state |

## Pin the sample identity

Record the task ID, pVisor commit, agent/model versions, initial repository commit, image digest, sampling parameters, policy, and executor for every rollout. Track task failure, isolation refusal, recording failure, and continuation quality separately; never record an infrastructure failure as "the model was incapable".

Replay reruns tools in a new environment and repeats their side effects. Use test APIs/databases and a separate output directory; validate the format with `--prepare-only`, validate the tool prefix with `--replay-only`, and then start a real continuation. See [replay](replay.md) for the pinned adapter versions and boundary semantics. Cluster throughput and hostile multi-tenancy are still unproven.
