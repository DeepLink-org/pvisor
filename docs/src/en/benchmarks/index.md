# Benchmarks and comparisons

Every measurement and comparison is reproducible and serves as evidence for the "auditable" promise; the method, environment, and sample counts are in [methodology](methodology.md).

## Available measurements

| Measurement | Observed result | Conditions and evidence |
|---|---|---|
| CLI → VM workload ready | P50 82.64 ms; P95 98.98 ms | Apple M4/HVF, trimmed firmware, 2 vCPU / 128 MiB, N=100; prepared rootfs, warm host caches; [startup latency](startup.md) |
| Cold VM RAM reclamation | About 60% lower RAM proxy for 2 GiB VMs | Two VMs with 64 MiB repeated cold data each, seconds 60–90 after ready; first reads slow down and footprint increases. See [memory benefits and usage costs](vm-memory/index.md) |

Each number describes a specific phase and workload. The startup table includes the full CLI-to-marker path, and cold RAM proxy is not whole-host physical memory. Other benchmarks remain marked below; unmeasured areas have no performance conclusion.

## Benchmarks

[VM memory savings, physical pressure, and performance overhead](vm-memory/index.md) · [Startup latency](startup.md) · [Filesystem overhead (planned)](filesystem.md) · [Network overhead (planned)](network.md) · [apply/drop cost (planned)](apply.md) · [End-to-end tasks (planned)](agent-tasks.md) · [Supervision cost (planned)](supervision-cost.md) · [Concurrency density (planned)](density.md) · [Isolation effectiveness (planned)](isolation-tests.md) · [Replay fidelity](replay-fidelity.md)

## Comparisons

[Agent-native sandboxes (planned)](compare-agent-sandboxes.md) · [Docker/devcontainer (planned)](compare-containers.md) · [Cloud sandboxes (planned)](compare-cloud-sandboxes.md) · [Isolation runtimes (planned)](compare-runtimes.md) · [RL infrastructure (planned)](compare-rl-infra.md)

!!! note "Under construction"
    End-to-end cold start, filesystem overhead, and end-to-end agent task overhead have no headline figures yet; see [Startup latency](startup.md), [Filesystem overhead (planned)](filesystem.md), and [End-to-end tasks (planned)](agent-tasks.md) for scope and acceptance criteria.
