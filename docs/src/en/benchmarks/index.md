# Benchmarks and comparisons

pVisor VM starts in about **86 ms**, near Docker, Firecracker and QEMU microvm on a hundred-millisecond scale. Complete repair/testing takes **0.70 s** staged, **0.90 s** in Docker, and **3.97 s** in the VM: interactive staging, with a remaining VM tool-path gap. A roughly 5.5-minute 100,000-file apply and Claude/VM initialization timeout are also published limitations.

Measurements publish reproduction scripts, samples and failures; comparisons identify official sources and unmeasured areas. See [methodology](methodology.md).

## Available measurements

Both macOS and Linux measurements are retained, with separate records for each platform, measurement date and artifact. The VM measurements below are from 2026-10-03; new results are added while earlier batches and raw evidence remain available.

| Host platform / backend | Measurement | Observed result | Conditions and evidence |
|---|---|---|---|
| macOS ARM64 / HVF | CLI → VM workload ready | P50 84.35 ms; P95 112.14 ms | Apple M4, controlled P0 candidate, trimmed firmware, 2 vCPU / 128 MiB, N=100; prepared rootfs, warm host caches; [startup latency and historical batches](startup.md) |
| macOS ARM64 / HVF | Cold VM RAM reclamation | About 60% lower RAM proxy for 2 GiB VMs | Apple M4, two VMs with 64 MiB repeated cold data each, seconds 60–90 after ready; first reads slow down and footprint increases; [memory benefits and usage costs](vm-memory/index.md) |
| Linux x86_64 / KVM | CLI → VM workload ready | P50 172.69 ms; P95 180.37 ms | Ryzen 7 9700X, libkrunfw 5.5.0, Fedora rootfs, 2 vCPU / 128 MiB, N=100; prepared rootfs, warm caches; [startup latency](startup.md#linux-results) |
| Linux x86_64 / KVM | VM lifecycle | pause P50 0.24 ms; offload 22.98 ms | Ryzen 7 9700X, 256 MiB / 2 vCPU / raw, N=30; [correctness and distributions](vm-memory/index.md#linux-lifecycle) |
| Linux x86_64 / KVM | Complete VM snapshots | Raw snapshot save P50 712 ms, restore to heartbeat 933 ms | Ryzen 7 9700X, 256 MiB / 2 vCPU, N=10 with two restored forks per run; [save, restore and compression data](vm-memory/index.md#linux-snapshot) |

These datasets cover different workloads and phases, so they do not rank macOS against Linux. The shared cold-page pager currently requires macOS/ARM64; Linux offload and complete snapshots are separate measurements.

Each number describes a specific phase and workload. The startup table includes the full CLI-to-marker path, and cold RAM proxy is not whole-host physical memory. New product measurements appear below, with unmeasured metrics identified separately.

## Performance position: baselines, gaps, and meaning {#reference-position}

Different questions have different familiar scales; bare startup cannot substitute for a complete task. The new Linux environment has 1,410 valid samples plus 320 resource/exit follow-ups. Historical macOS/Linux data and 3,375 first-edition product samples remain separate.

| Question | Familiar baseline | pVisor position | User implication |
|---|---|---|---|
| [New VM first output](startup.md#reference-startup) | Firecracker 74 ms; QEMU microvm 88 ms; Docker 90 ms | VM 86 ms | Hundred-millisecond microVM range for prepared short tasks; downloads excluded |
| [Complete repair/testing](agent-tasks.md#reference-env) | Native 0.50 s; Docker 0.90 s; QEMU microvm 1.85 s | Staged 0.70 s; VM 3.97 s | Staging adds ~0.20 s; VM takes ~4.4 times Docker, a remaining gap |
| [Reads/traversal](filesystem.md#reference-fs) | Docker 64 MiB read 33 ms; 2,048-file traversal 5 ms | Staged 49 ms / 180 ms | Modest individual read cost; frequent small-file scans accumulate |
| [Complete-task memory](methodology.md#reference-resources) | Native RSS 169 MiB; Docker including daemon 280 MiB | Staged 227 MiB; VM 722 MiB | Actual residency scale, not Agent density; summed RSS duplicates shared pages |
| [Real client tool loops](agent-tasks.md#reference-env) | Claude/Docker 1.23 s; Codex/Docker 6.26 s | Staged 1.07 s / 2.25 s; Codex/VM 10.93 s; Claude/VM failed | Choose per client; initialization and compatibility can dominate startup |
| [Admission](apply.md) | Git patch apply: 10 files 0.73 ms; 1,000 files 13.61 ms | ~15 ms / 836 ms; 100,000 files ~5.5 min | Interactive small admission; protocol overhead and costly large batches; Git has different transaction semantics |
| [Local requests](network.md) | Native HTTP 0.95 ms | Proxy 1.24 ms; VM 3.83 ms | +0.29 / +2.88 ms locally, not Internet model latency |
| [Review/supervision](supervision-cost.md) | Familiar Git/diff workflow; not a measured performance control | 20-item machine review/apply/drop ~25 ms | Machine cost measured; human minutes saved unmeasured |
| [Concurrency](density.md) | Native/Podman also pass 128 idle probes | Staged passes 128 idle probes; safe has failures | Idle success does not establish capacity for 128 complete Agents |

Numbers are P50 within their own batches. New controls share tools and two host cores; VMs use 2 vCPU and 16 GiB for complete tasks. Docker is rootless Engine with bind mounts; VM file paths differ. These locate practical costs, not identical security boundaries/filesystems or pure VMM rankings. New macOS complete-environment/Docker/Firecracker/QEMU comparisons are unmeasured; existing HVF evidence remains in separate macOS sections.

[Full method, pinned artifacts and failure evidence](methodology.md#reference-env)


## Benchmarks

[VM memory reclamation, offload and complete snapshots](vm-memory/index.md) · [Startup latency](startup.md) · [Filesystem overhead](filesystem.md) · [Network overhead](network.md) · [apply/drop cost](apply.md) · [End-to-end tasks](agent-tasks.md) · [Supervision cost](supervision-cost.md) · [Concurrency density](density.md) · [Isolation effectiveness](isolation-tests.md) · [Replay fidelity](replay-fidelity.md)

## Comparisons

[Agent-native sandboxes](compare-agent-sandboxes.md) · [Docker/devcontainer](compare-containers.md) · [Cloud sandboxes](compare-cloud-sandboxes.md) · [Isolation runtimes](compare-runtimes.md) · [RL infrastructure](compare-rl-infra.md)

## Product performance first version, 2026-10-04

Linux controls cover native, staged, safe, VM and OCI. Host tool time is near native; staging adds tens of milliseconds for sequential reads/offline npm and more for small files. Apply rises from about 15 ms for ten files to 5.5 minutes for 100,000. Failures are published with timings.

The measurements support small-batch review and local tool execution: staging costs are modest for sequential reads and offline installs, while 1,000-file submission approaches a second. Directory traversal, dense small-file writes, large submissions and safe-mode concurrency stability still need improvement. Idle-probe capacity does not establish full-agent capacity.

| Topic | Observed result |
|---|---|
| [Filesystem](filesystem.md) | 64 MiB read worker P50 32.66 ms native / 40.76 staged; 2,048-file metadata 4.84 → 149.60 ms |
| [Network](network.md) | Local HTTP small-request P50 0.95 ms native / 1.24 proxy / 3.83 VM; not public API latency |
| [apply/drop](apply.md) | 10/1,000/100,000-file apply P50 about 15 ms / 0.84 s / 5.5 min; conflicts and SIGKILL recovery retained |
| [Density](density.md) | Idle concurrency 128: native/host/staged/Podman all completed; safe 638/640, large OCI rootfs hit tmpfs quota |
| [Agent tool loop](agent-tasks.md) | Real Claude/Codex CLIs, 72/72 controlled repairs; real-model task success unmeasured |
| [Review](supervision-cost.md) | Review 20, apply ten/drop ten costs about 25 ms machine time; human minutes unmeasured |
| [Isolation](isolation-tests.md) | Five profiles, outside-path/socket/alias probes with actual host effects |
| [Replay](replay-fidelity.md) | Six adapters, 360/360 synthetic prefixes; prepare-only side-effect bug found and fixed |

Platforms have separate sections. New macOS workloads, real-model quality, human studies and cloud timings/cost remain unmeasured. See [protocol and raw evidence](methodology.md#product-v1).
