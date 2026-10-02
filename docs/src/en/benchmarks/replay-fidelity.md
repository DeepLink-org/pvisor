---
status: todo
search:
  exclude: true
---

# Replay fidelity

!!! warning "Planned"
    No data yet meets [benchmark methodology](methodology.md). Existing historical samples are in the [Qwen3.6 experiment log](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/replay/qwen3.6-results.md): 3 tasks and 5 agents, with run dates, commit IDs, and repeat counts not fully recorded, so they are not a compatibility or determinism guarantee.

## Question

Replaying forward from step N, how closely does the agent's next action match the original trajectory? How does the continued task success rate compare with running from scratch?

## Requirements

- At least 20 tasks per adapter (Claude Code, Codex, OpenCode, OpenHands, mini-swe-agent, Pi agent), with multiple repeats each.
- Metric: exact next-action tool agreement rate, visible-text similarity, and the gap between continued and original reward.
- Record the run date, pVisor commit, model and agent versions, sampling parameters, and hardware.
- List failed samples separately and classify them (context reconstruction failure, model nondeterminism, environment difference).

## Acceptance criteria

- Reproduce in one command, reporting through the `pvisor-benchmark/v1` schema.
- [Replay design (planned)](../design/replay.md) cites these metrics.

## Tracking

- Tracking issue: TODO
- Owner: TODO
- Related: [Replay](../guides/replay.md)
