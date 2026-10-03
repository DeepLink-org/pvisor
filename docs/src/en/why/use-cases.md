# Use cases

The same pVisor solves different problems at different scales.

## One person, one agent

Let Claude Code or Codex complete a refactor fully unattended: it edits files and runs tests, and may delete a few things along the way. You do not watch; when it finishes you review the changes as you would a PR and merge only the paths you want. **Available today.**

```bash
pvisor run --safe -- claude
pvisor status --review last
pvisor apply last --path src
```

## One person, multiple agents

Run several agents on different approaches at once. Each Job is isolated and leaves its own evidence, so you can review in batches and merge only the results you need. **Next (L2)**; see [parallel agents](../guides/parallel-agents.md).

## Teams and platforms

Put pVisor in CI: let the agent finish, review, and merge only what you want. **Available today (the L1 way)**; policy- and evidence-driven exemption and clustering (L2/L3) are the direction—see [running agents in CI](../guides/ci.md).

## Research and training

Agentic RL and evaluation want exactly a "large-scale, untrusted, recordable" execution substrate: trajectories can be recorded, forked from checkpoints, and replayed by tool prefix. **Direction**; see [RL rollouts](../guides/rl-rollouts.md) and [research directions](../design/research/index.md).
