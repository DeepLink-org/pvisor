# How should you choose between pVisor, Firecracker, QEMU and isolation runtimes?

## Main conclusions {#conclusions}

**pVisor VM starts in the lightweight-VM range, but measured repair and seven-tool tasks take longer than Firecracker and QEMU. Choose with complete execution and retained-change costs; gVisor/Kata have no matching ranking.**

| Need | Selection implication |
|---|---|
| Lightweight VM execution | Compare Firecracker and QEMU microvm |
| Unified stage/apply workflow | Evaluate pVisor execution plus application |
| gVisor or Kata required | No matching local performance ranking |

## Motivation {#motivation}

An independent guest kernel, OCI workflow or system-call isolation can determine runtime choice. VMM startup, OS boot, tool execution and pVisor staging/recording costs must be distinguished.

## Experiment design {#interpretation}

Shared Linux/x86_64 host, AMD Ryzen 7 9700X, Fedora kernel 7.2.8-200.fc44.x86_64. Launch trees and the private Docker daemon are pinned to host CPUs 0,1; guests have 2 vCPU. Host/staged use rootless_process. Shell VMs use 128 MiB; tool VMs use 16 GiB. Native/Docker memory is not capped: this controls CPU and configured guest RAM, not identical resource enforcement. Tools and inputs are prepared; each run gets a fresh workspace, warm caches, three warmups and 60 measured trials, with seeded randomized backend order. Builds, downloads and input copying are excluded.

Docker Engine 29.7.2 uses a private rootless VFS daemon and writable bind mounts. This does not represent overlay2 or Docker Desktop. Firecracker 1.13.1 PCI runs without jailer; QEMU 10.2.2 uses q35/microvm with private ext4. pVisor VM uses virtio-fs and a different kernel. Kernel, storage, devices and staging semantics remain configuration differences; these results do not isolate the VMM or FUSE alone.

Outputs, staging and exits must validate. These are prepared-environment comparisons, not identical OS or security-hardening rankings.

## Data and analysis {#results}

### Lightweight VM waiting {#reference-comparison}

Startup/filesystem: 2026-10-05; repair: 2026-10-06. Each independent workload/backend N=60, failures=0, three warmups. Units and timing boundaries are in the headers. P50 or separated cluster medians with counts; no cross-batch pooled distribution.

| Runtime | Valid / failed | Ready P50 ms | Repair completion P50 s | Seven-tool completion P50 s |
|---|---|---|---|---|
| pVisor VM | 60 / 0 | 99.76 | 3.25 | 6.66 |
| Firecracker PCI | 60 / 0 | 74.74 | 2.06 | 2.16 |
| QEMU q35 | 60 / 0 | 213.89 | 1.33 | 2.44 |
| QEMU microvm | 60 / 0 | 86.60 | 1.27 | 2.45 |

For reused environments, startup is amortized and tool time matters more. Kernels and filesystem paths differ, so this does not isolate libkrun as the sole cause. [Startup](startup.md), [filesystem](filesystem.md) and [repair/CLI checks](agent-tasks.md) give details.

### Execution scope

| Runtime | Execution scope | Measurement |
|---|---|---|
| pVisor / libkrun | Integrated guest VM + stage/apply | Local comparisons below |
| Firecracker / QEMU | Independent VMM CLIs | Reference measurements, not integrated pVisor executors |
| gVisor | Application kernel / runsc | Matching performance unmeasured |
| Kata | VM-backed container workflow | Matching performance unmeasured |

Official descriptions: [gVisor](https://gvisor.dev/docs/), [Firecracker](https://firecracker-microvm.github.io/), [Kata](https://katacontainers.io/). Accepted pVisor backends are in [executors](../guides/executors/index.md).

### Complete Ubuntu deployment {#full-ubuntu}

Independent 2026-10-04, two cores. Startup: 2 GiB, pVisor/Firecracker N=30, QEMU N=10; repair: 16 GiB, N=10. P50, different OS initialization/tools/storage.

| Deployment | Ready P50 ms | Repair result P50 s |
|---|---|---|
| pVisor VM / host tools | 109.69 | 4.61 |
| Firecracker / Ubuntu | 5644.11 | 8.51 |
| QEMU q35 / Ubuntu | 5428.90 | 8.12 |
| QEMU microvm / Ubuntu | 7666.69 | 10.33 |

This answers deployment waiting, not a pure VMM ranking.

### Downloads and reproduction {#run}

[Derived table CSV](compare-runtimes.csv) · [Runtime statistics](runtime-summary.csv) · [Sources and artifacts](runtime-provenance.csv) · [Evidence source summary](evidence-sources.csv) · [Method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
