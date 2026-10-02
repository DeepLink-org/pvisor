# Benchmarks and comparisons

This section provides reproducible measurements and comparisons as evidence for claims readers can verify.

## Headline figures

| Metric | Value | Source |
| --- | --- | --- |
| VM executor guest readiness latency (guest init only, Apple M4/HVF) | p50 ≈ 114 ms (Rust init) | [Startup latency](startup.md) |
| End-to-end pVisor cold start (host/container/VM) | In progress | [Startup latency](startup.md) |
| Filesystem overhead | In progress | [Filesystem overhead (planned)](filesystem.md) |
| End-to-end agent task overhead | In progress | [End-to-end tasks (planned)](agent-tasks.md) |

Every benchmark states method, environment and sample count. See [Methodology](methodology.md).

## Benchmarks

[Startup latency](startup.md) · [Filesystem overhead (planned)](filesystem.md) · [Network overhead (planned)](network.md) · [apply/drop cost (planned)](apply.md) · [End-to-end tasks (planned)](agent-tasks.md) · [Supervision cost (planned)](supervision-cost.md) · [Concurrency density (planned)](density.md) · [Isolation effectiveness (planned)](isolation-tests.md) · [Replay fidelity](replay-fidelity.md)

## Comparisons

[Agent-native sandboxes (planned)](compare-agent-sandboxes.md) · [Docker/devcontainer (planned)](compare-containers.md) · [Cloud sandboxes (planned)](compare-cloud-sandboxes.md) · [Isolation runtimes (planned)](compare-runtimes.md) · [RL infrastructure (planned)](compare-rl-infra.md)

!!! note "TODO"
    Add headline filesystem and end-to-end task figures; replace “In progress” with data or links.
