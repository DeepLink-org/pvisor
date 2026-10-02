# Comparing existing approaches

This is the overview comparison table; the README and product overview link here. Each option includes where it fits. The descriptions below still need dated, versioned source verification, as noted at the end.

| Approach | Provides | What needs additional work | When to choose it |
| --- | --- | --- | --- |
| Docker / devcontainer | Isolated, reproducible environment | Change review, conflict refusal, selective apply, evidence of installed limits | Reproducible environment alone |
| Docker + `git diff` | Isolation and change inspection | Protect concurrent edits, batch apply/recovery, network/file evidence | Small changes you already review line by line |
| Agent-native sandbox | Blocks some commands or paths | Cross-agent/executor semantics and checkable records; permissions often granted per command or block | One low-risk session you watch |
| git worktree | File separation for parallel work | Network/credential controls and evidence | File-only parallel experiments |
| Cloud sandboxes (E2B, Daytona, Modal) | Remote isolation and elastic resources | Local workspace/toolchain integration and review flow | Remote isolation or elasticity |
| Kubernetes / Ray | Scheduling and orchestration | Bounded, recoverable, checkable execution semantics | Existing orchestration; potential future pVisor integration |
| gVisor / Firecracker / Kata | Stronger isolation substrates | Staging, selective apply, and evidence | Stronger isolation; potential executor backends |

## Detailed comparisons (in progress)

These pages will include dates, versions, sources, and [benchmark](../benchmarks/index.md) data where possible:

- [Agent-native sandboxes (planned)](../benchmarks/compare-agent-sandboxes.md)
- [Docker / devcontainer (planned)](../benchmarks/compare-containers.md)
- [Cloud sandboxes (planned)](../benchmarks/compare-cloud-sandboxes.md)
- [gVisor / Firecracker / Kata (planned)](../benchmarks/compare-runtimes.md)
- [RL rollout infrastructure (planned)](../benchmarks/compare-rl-infra.md)

!!! note "Dates and sources pending"
    Claims about other products have not yet been individually dated, versioned, and sourced. [Open an issue](https://github.com/DeepLink-org/pvisor/issues) to correct inaccuracies.
