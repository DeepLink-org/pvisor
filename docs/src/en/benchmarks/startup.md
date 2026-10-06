# How long does a usable environment take to start?

## Conclusions {#conclusions}

**Prepared pVisor host/staged first output takes 12.72/25.13 ms, sooner than the tested Docker configuration. pVisor VM takes 100.73 ms, behind Firecracker at 72.61 ms and QEMU microvm at 86.57 ms.**

| Need | Selection implication |
| --- | --- |
| Local execution with retained changes | Evaluate host/staged and the complete review workflow |
| Independent guest kernel | Budget both startup and VM tool waiting |
| Concurrent or idle environments | Require fixed-budget throughput and physical-memory measurements |

## Motivation {#motivation}

Disposable environments repeatedly pay for the first command. Short tasks also pay for exit and tools; startup alone cannot describe their total cost.

## Experiment design {#interpretation}

Linux x86_64, AMD Ryzen 7 9700X, Fedora 7.2.8-200.fc44.x86_64. Launched process trees and the private Docker daemon are pinned to CPUs 0,1. VMs use 2 vCPU, 128 MiB for the shell probe and 16 GiB for tools. Native/Docker memory is uncapped: this is a CPU-controlled task comparison, not a capacity comparison under identical memory limits. Host/staged use rootless_process.

All backends share offline tools and fixed inputs, with a new workspace per trial. Warm caches, three warmups and 60 measured samples per cell; backends alternate in seeded randomized order. Preparation, builds, image import and fixture resets are outside timing; complete tasks include launch and exit. Docker Engine 29.7.2 uses a private rootless **overlay2** daemon, the classic image store and writable bind mounts. Firecracker 1.13.1 PCI runs without jailer; QEMU 10.2.2 uses q35/microvm and private ext4. pVisor VM uses virtio-fs and its own firmware. Kernels, storage and staging semantics differ: these are task costs for the stated configurations, not pure VMM or security rankings.

Ready ends at a validated shell marker; Exit ends when the process exits. Each trial starts a fresh environment without snapshots or pooling. Cold images, full OS initialization and Agent CLI initialization are outside this probe.

## Data and analysis {#results}

Measured on 2026-10-06: each backend/workload has 60/60 valid samples and zero measured failures. Outputs, exit and execution records must pass validation; staging also requires unchanged host originals and complete retained changes. Every valid slow sample is kept, with no timing-based exclusions. Tables normally show P50; separated distributions show cluster medians and counts. P95 is descriptive only. Raw reports, binaries and input/source manifests stay in ignored `.data/`; public CSVs retain workload, cohort and provenance associations.

### Prepared environments {#reference-startup}

<a id="reference-exit"></a>

| Runtime | Valid / failed | Ready P50 ms | Ready P95 ms | Exit P50 ms | Exit P95 ms |
| --- | --- | --- | --- | --- | --- |
| Native | 60 / 0 | 1.28 | 1.71 | 1.35 | 1.82 |
| pVisor host | 60 / 0 | 12.72 | 14.18 | 24.03 | 24.71 |
| pVisor staged | 60 / 0 | 25.13 | 26.85 | 33.84 | 44.55 |
| pVisor VM | 60 / 0 | 100.73 | 109.17 | 136.65 | 156.89 |
| Docker rootless / overlay2 | 60 / 0 | 74.48 | 87.13 | 98.29 | 110.22 |
| Firecracker PCI | 60 / 0 | 72.61 | 74.53 | 95.98 | 104.27 |
| QEMU q35 | 60 / 0 | 213.07 | 217.77 | 237.52 | 245.37 |
| QEMU microvm | 60 / 0 | 86.57 | 95.30 | 109.52 | 119.96 |

### Full distributions and macOS {#full-ubuntu}

<a id="macos"></a>

Full Ubuntu cold startup and Apple Silicon/HVF have not been retested with the current artifacts. There is no matched ranking for those conditions; the lightweight shell probe cannot replace actual deployment measurements.

### Downloads and reproduction {#run}

[Derived statistics CSV](startup.csv) · [All runtime statistics](runtime-summary.csv) · [Differences and 95% confidence intervals](runtime-comparisons.csv) · [Source and artifact provenance](runtime-provenance.csv) · [Method](methodology.md) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
