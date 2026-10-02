---
status: todo
search:
  exclude: true
---

# Replay fidelity

!!! warning "Planned"
    No data yet meets [methodology](methodology.md). [Historical Qwen3.6 samples](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/replay/qwen3.6-results.md) cover three tasks and five agents, without complete dates, commits, or repeat counts. They do not establish compatibility or determinism.

## Question

How similar is the next action after a replay boundary, and how does continuation success compare with starting fresh?

## Requirements

- At least 20 tasks with repeats per Claude Code, Codex, OpenCode, OpenHands, mini-swe-agent, and Pi adapter.
- Metrics: exact next-tool agreement, visible-text similarity, continued versus original reward.
- Record date, commit, model/agent, sampling settings, hardware.
- Classify context reconstruction failures, model nondeterminism, and environment differences.

## Acceptance criteria

- One-command reproduction with pvisor-benchmark/v1.
- Link results from replay design.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [Replay](../guides/replay.md)
