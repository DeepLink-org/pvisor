# Comparing existing approaches

| Approach | Strong at | Does not solve | When to choose it |
| --- | --- | --- | --- |
| Docker / devcontainer | Isolated environment, reproducible dependencies | Change review, refusing to overwrite on conflict, selective apply, evidence | You only need a reproducible environment |
| Agent-native sandbox | Blocking some commands | Per-command or per-block approval; nothing across agents/executors; no checkable record | A single low-risk session |
| git worktree | File-level parallelism | Network and credentials, execution evidence | File-only parallelism |
| Cloud sandboxes (E2B, Daytona, Modal) | Remote isolated execution | Local workspace integration and a local review flow | You need remote isolation |
| Kubernetes / Ray | Scheduling and orchestration | They schedule Pods and processes, not "bounded, reversible, checkable" execution semantics | You already have an orchestration layer |
| gVisor / Firecracker / Kata | Isolation substrate | Execution semantics, review, and evidence | You need a stronger isolation substrate |

The most common objection is "Docker plus `git diff` is enough." It shows what changed, but it cannot refuse to overwrite on conflict, apply selectively by path, or show evidence of "which limits actually took effect"—exactly what pVisor adds.

These descriptions are not yet dated; version and source will be filled in as the per-comparison pages land. For per-row comparison date, version, and sources, see the per-comparison pages (under construction): [agent-native sandboxes](../benchmarks/compare-agent-sandboxes.md) · [Docker/devcontainer](../benchmarks/compare-containers.md) · [cloud sandboxes](../benchmarks/compare-cloud-sandboxes.md) · [isolation substrates](../benchmarks/compare-runtimes.md) · [RL infrastructure](../benchmarks/compare-rl-infra.md).
