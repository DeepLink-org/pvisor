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

Assess overhead across the task flow: [end-to-end tasks](../benchmarks/agent-tasks.md) combines measured repair, filesystem tools, workspace review and CLI compatibility to analyze native Agent sandboxes, Docker/devcontainer, VMs and clouds. [Reinforcement learning](../benchmarks/compare-rl-infra.md) combines active-capacity and replay evidence to explain executor choices for rollout environment budgets. Cloud bills, default nested Agent sandboxes and complete training performance have no matched comparison.
