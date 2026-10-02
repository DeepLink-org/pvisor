# Use cases: value across scales

| Scale | Reader | Level | Status |
| --- | --- | --- | --- |
| One person, one agent | Developer | L1 | Available today |
| One person, multiple agents | Developer | L2 | Next |
| Teams and platforms | CI, platform engineering | L2–L3 | Direction |
| Research and training | MLSys, agentic RL | L3 | Direction |

## One person, one agent (today)

**Scenario:** Claude Code or Codex refactors several files.

**Pain today:** approve every command while watching a long task, or run automatically and inspect the entire diff while worrying about deletions, private keys, and network destinations.

**With pVisor:**

```bash
pvisor run --safe -- codex
pvisor status --review last
pvisor apply last --path src
```

The agent finishes unattended; changes remain staged and the project stays unchanged until apply. Review changes and denied accesses, apply `src`, and discard the rest. Conflicting edits you made meanwhile are not overwritten.

See [first run](../start/first-run.md), [agent integration](../guides/agents/index.md), and [review/apply](../guides/review-apply.md).

## One person, multiple agents (next)

**Scenario:** try several solutions and keep the best.

**Pain today:** shared workspaces clash; separate worktrees isolate files but do not control networking/credentials or produce comparable evidence.

**With pVisor:** each agent has a Job, stage, and evidence. You can already [fork a checkpoint](../guides/fork-checkpoint.md). Batch review and concurrency guidance remain [planned](../guides/parallel-agents.md).

## Teams and platforms (direction)

**Scenario:** fix failing tests in CI or run agents for several teams.

**Pain today:** unattended execution is difficult to audit and its changes difficult to recover selectively.

**With pVisor:** policy bounds execution, Run Bundles support audit, and the direction is human intervention for exceptions. See [CI](../guides/ci.md) and the [trust ladder](trust-ladder.md).

## Research and training (direction)

**Scenario:** agentic RL rollouts, evaluation, and trajectory collection.

**Pain today:** many untrusted executions need isolation, reproducibility, records, and branches from intermediate states.

**With pVisor:** the intended substrate gives each rollout bounded, recoverable, checkable execution. Gateway records model interaction, checkpoints fork filesystem states, and [replay](../guides/replay.md) reconstructs tool prefixes before live continuation. Batch integration is [planned](../guides/rl-rollouts.md).
