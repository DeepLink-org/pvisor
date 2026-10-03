# Benchmarks and comparisons

Every measurement and comparison is reproducible and serves as evidence for the "auditable" promise; the method, environment, and sample counts are in [methodology](methodology.md).

## Available measurements

Both macOS and Linux measurements are retained, with separate records for each platform, measurement date and artifact. The measurements below are from 2026-10-03; new results are added while earlier batches and raw evidence remain available.

| Host platform / backend | Measurement | Observed result | Conditions and evidence |
|---|---|---|---|
| macOS ARM64 / HVF | CLI → VM workload ready | P50 84.35 ms; P95 112.14 ms | Apple M4, controlled P0 candidate, trimmed firmware, 2 vCPU / 128 MiB, N=100; prepared rootfs, warm host caches; [startup latency and historical batches](startup.md) |
| macOS ARM64 / HVF | Cold VM RAM reclamation | About 60% lower RAM proxy for 2 GiB VMs | Apple M4, two VMs with 64 MiB repeated cold data each, seconds 60–90 after ready; first reads slow down and footprint increases; [memory benefits and usage costs](vm-memory/index.md) |
| Linux x86_64 / KVM | CLI → VM workload ready | P50 172.69 ms; P95 180.37 ms | Ryzen 7 9700X, libkrunfw 5.5.0, Fedora rootfs, 2 vCPU / 128 MiB, N=100; prepared rootfs, warm caches; [startup latency](startup.md#linux-results) |
| Linux x86_64 / KVM | VM lifecycle | pause P50 0.24 ms; offload 22.98 ms | Ryzen 7 9700X, 256 MiB / 2 vCPU / raw, N=30; [correctness and distributions](vm-memory/index.md#linux-lifecycle) |
| Linux x86_64 / KVM | Complete VM snapshots | Raw snapshot save P50 712 ms, restore to heartbeat 933 ms | Ryzen 7 9700X, 256 MiB / 2 vCPU, N=10 with two restored forks per run; [save, restore and compression data](vm-memory/index.md#linux-snapshot) |

These datasets cover different workloads and phases, so they do not rank macOS against Linux. The shared cold-page pager currently requires macOS/ARM64; Linux offload and complete snapshots are separate measurements.

Each number describes a specific phase and workload. The startup table includes the full CLI-to-marker path, and cold RAM proxy is not whole-host physical memory. Other benchmarks remain marked below; unmeasured areas have no performance conclusion.

## Benchmarks

[VM memory reclamation, offload and complete snapshots](vm-memory/index.md) · [Startup latency](startup.md) · [Filesystem overhead (planned)](filesystem.md) · [Network overhead (planned)](network.md) · [apply/drop cost (planned)](apply.md) · [End-to-end tasks (planned)](agent-tasks.md) · [Supervision cost (planned)](supervision-cost.md) · [Concurrency density (planned)](density.md) · [Isolation effectiveness (planned)](isolation-tests.md) · [Replay fidelity](replay-fidelity.md)

## Comparisons

[Agent-native sandboxes (planned)](compare-agent-sandboxes.md) · [Docker/devcontainer (planned)](compare-containers.md) · [Cloud sandboxes (planned)](compare-cloud-sandboxes.md) · [Isolation runtimes (planned)](compare-runtimes.md) · [RL infrastructure (planned)](compare-rl-infra.md)

!!! note "Under construction"
    End-to-end cold start, filesystem overhead, and end-to-end agent task overhead have no headline figures yet; see [Startup latency](startup.md), [Filesystem overhead (planned)](filesystem.md), and [End-to-end tasks (planned)](agent-tasks.md) for scope and acceptance criteria.
