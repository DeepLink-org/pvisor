# Comparing existing approaches

| Approach | Strong at | Does not solve | When to choose it |
| --- | --- | --- | --- |
| Docker / devcontainer | Isolated environment, reproducible dependencies | Change review, refusing to overwrite on conflict, selective apply, evidence | You only need a reproducible environment |
| Agent-native sandbox | OS sandboxing, approvals and client session records | Unified cross-client stage/apply and executor-capability evidence require additional organization | Prefer native client protections first |
| git worktree | File-level parallelism | Network and credentials, execution evidence | File-only parallelism |
| Cloud sandboxes (E2B, Daytona, Modal) | Remote isolated execution | Local workspace integration and a local review flow | You need remote isolation |
| Kubernetes / Ray | Scheduling and orchestration | They schedule Pods and processes, not "bounded, reversible, checkable" execution semantics | You already have an orchestration layer |
| gVisor / Firecracker / Kata | Isolation substrate | Execution semantics, review, and evidence | You need a stronger isolation substrate |

The most common objection is "Docker plus `git diff` is enough." It can be sufficient with worktrees, patch validation and a complete merge protocol. A diff alone cannot undo writes already made through a bind mount. pVisor unifies staging, selective submission, preimage conflicts and capability observations; the benchmarks show the cost.

The first comparison was checked against official documentation on 2026-10-04. Local Podman/CLI evidence and unmeasured cloud performance are distinguished. See sources, configurations and boundaries in: [agent-native sandboxes](../benchmarks/compare-agent-sandboxes.md) · [Docker/devcontainer](../benchmarks/compare-containers.md) · [cloud sandboxes](../benchmarks/compare-cloud-sandboxes.md) · [isolation substrates](../benchmarks/compare-runtimes.md) · [RL infrastructure](../benchmarks/compare-rl-infra.md).
