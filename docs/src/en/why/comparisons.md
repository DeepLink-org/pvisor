# Comparing existing approaches

| Approach | Strong at | Additional setup or integration | When to choose it |
| --- | --- | --- | --- |
| Docker / devcontainer | Isolated environment, reproducible dependencies | Change review, refusing to overwrite on conflict, selective apply, evidence | You only need a reproducible environment |
| Agent-native sandbox | OS sandboxing, approvals and client session records | Unified cross-client stage/apply and executor-capability evidence require additional organization | Prefer native client protections first |
| git worktree | File-level parallelism | Network and credentials, execution evidence | File-only parallelism |
| Cloud sandboxes (E2B, Daytona, Modal) | Remote isolated execution | Local workspace integration and a local review flow | You need remote isolation |
| Kubernetes / Ray | Scheduling and orchestration | Task execution boundaries, file staging, and execution records | You already have an orchestration layer |
| gVisor / Firecracker / Kata | Isolation substrate | Execution semantics, review, and evidence | You need a stronger isolation substrate |

Existing worktrees, patch validation, and merge workflows can be used with Docker and `git diff`. Writes through a bind mount need their own recovery procedure. pVisor provides staging, selective submission, preimage conflict checks, and capability observations; the benchmarks show the runtime cost.

The first comparison was checked against official documentation on 2026-10-04. Local Podman/CLI evidence and unmeasured cloud performance are distinguished. See sources, configurations and boundaries in: [agent-native sandboxes](../benchmarks/compare-agent-sandboxes.md) · [Docker/devcontainer](../benchmarks/compare-containers.md) · [cloud sandboxes](../benchmarks/compare-cloud-sandboxes.md) · [isolation substrates](../benchmarks/compare-runtimes.md) · [RL infrastructure](../benchmarks/compare-rl-infra.md).
