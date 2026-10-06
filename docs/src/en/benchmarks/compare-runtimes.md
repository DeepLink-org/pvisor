# How should you choose between pVisor, Firecracker and QEMU?

## Conclusions {#conclusions}

**Against original `/boot` kernels, pVisor VM starts in 100.91 ms, ahead of Firecracker at 285.04 ms and QEMU microvm at 321.56 ms. In a separate custom-reference kernel cohort, pVisor VM repair and file-heavy tasks take longer. Choose using the actual kernel and complete task.**

| Need | Selection implication |
| --- | --- |
| Local execution with retained changes | Evaluate host/staged and the complete review workflow |
| Independent guest kernel | Budget both startup and VM tool waiting |
| Concurrent or idle environments | Require fixed-budget throughput and physical-memory measurements |

## Motivation {#motivation}

An independent guest kernel, an OCI workflow and retained changes meet different needs. Complete-task comparisons help avoid choosing an environment on startup speed alone.

## Experiment design {#interpretation}

Linux x86_64, AMD Ryzen 7 9700X, Fedora 7.2.8-200.fc44.x86_64. Launched process trees and the private Docker daemon are pinned to CPUs 0,1. VMs use 2 vCPU, 128 MiB for the shell probe and 16 GiB for tools. Native/Docker memory is uncapped: this is a CPU-controlled task comparison, not a capacity comparison under identical memory limits. Host/staged use rootless_process.

All backends share offline tools and fixed inputs, with a new workspace per trial. Warm caches, three warmups and 60 measured samples per cell; backends alternate in seeded randomized order. Preparation, builds, image import and fixture resets are outside timing; complete tasks include launch and exit. Docker Engine 29.7.2 uses a private rootless **overlay2** daemon, the classic image store and writable bind mounts. Firecracker 1.13.1 PCI runs without jailer; QEMU 10.2.2 uses q35/microvm and private ext4. pVisor VM uses virtio-fs and its own firmware. Kernels, storage and staging semantics differ: these are task costs for the stated configurations, not pure VMM or security rankings.

This page reuses three separately registered startup, filesystem and repair workloads without pooling samples. Images and tools are prepared; output, exit and staging checks must pass. This is not a ranking under identical OS or security hardening.

Stock startup is a separate 2026-10-07 cohort: Firecracker uses the original Fedora 7.2.8 extracted ELF and QEMU uses the same `/boot/vmlinuz`, sharing minimal initrd and userspace; microvm retains RTC. pVisor uses dedicated Linux 6.12.109 firmware. Firecracker receives controlled termination after checked output and reports Ready only; normal shutdown is unmeasured. See [startup controls](startup.md) for methods and provenance.

## Data and analysis {#results}

Both independent experiments have 60/60 valid samples per cell and zero measured failures. Every valid slow sample is retained without timing-based exclusions; raw reports, binaries and input/source manifests stay in ignored `.data/`.

### Original distribution kernel startup {#stock-startup}

2026-10-07, one cohort at 2 vCPU / 128 MiB, Ready P50 in ms:

| Runtime | Ready P50 ms |
| --- | --- |
| pVisor VM | 100.91 |
| Firecracker PCI / stock | 285.04 |
| QEMU q35 / stock | 702.98 |
| QEMU microvm / stock | 321.56 |

pVisor has shorter first-command waiting for these complete configurations. This does not establish tool-task, same-kernel VMM-cost or capacity advantages. See [startup](startup.md) for 95% intervals of the differences.

### Complete-task comparison {#reference-comparison}

Independent 2026-10-06 custom-reference kernel cohort; Firecracker is legacy reference/unknown, rather than a `/boot` stock control. Stock-kernel tool tasks are unmeasured; this table is not pooled with the table above.

| Runtime | Ready P50 ms | Repair completion P50 s | Seven-tool completion P50 s |
| --- | --- | --- | --- |
| pVisor VM | 100.73 | 3.25 | 4.27 |
| Firecracker PCI | 72.61 | 2.20 | 2.29 |
| QEMU q35 | 213.07 | 1.46 | 1.49 |
| QEMU microvm | 86.57 | 1.40 | 1.50 |

See [startup](startup.md), [filesystem](filesystem.md) and [repair tasks](agent-tasks.md) for operations, timing boundaries and confidence intervals.

<a id="full-ubuntu"></a>
gVisor, Kata and full Ubuntu have no current matched measurements; no ranking is provided.

### Downloads and reproduction {#run}

[Derived statistics CSV](compare-runtimes.csv) · [All runtime statistics](runtime-summary.csv) · [Differences and 95% confidence intervals](runtime-comparisons.csv) · [Source and artifact provenance](runtime-provenance.csv) · [Method](methodology.md) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
