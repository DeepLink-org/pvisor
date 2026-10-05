# How long does a usable environment take to start?

## Main conclusions {#conclusions}

**pVisor VM starts in the same hundred-millisecond range as Docker and QEMU microvm; minimal Firecracker is slightly faster.** Prepared-environment first-output P50 is **86, 90, 88 and 74 ms**, respectively. pVisor host/staged takes about **6/15 ms**, suited to lighter local tasks.

Against complete Ubuntu boot, image-free pVisor VM takes about **110 ms**, while Firecracker/QEMU takes **5–8 s**, reducing short-task startup waiting. These are different deployment approaches, kernels, services and initialization paths. See [task performance](agent-tasks.md) for tool and complete-task costs.

| Need | Selection implication |
|---|---|
| Frequent short local tasks | Host/staged have a smaller startup budget |
| Independent guest kernel | Budget VM startup and tool time |
| Full Ubuntu services | Budget a complete distribution boot |

## Motivation {#motivation}

Startup waiting directly affects frequent disposable Agent environments. Persistent environments amortize it. First output, complete CLI exit and working tools are different budgets.

## Experiment design {#interpretation}

Ready starts before host command launch and ends at useful output; Exit ends at process termination. Downloads, installation, templates and per-trial copying are excluded. Each task creates a fresh environment, without RAM snapshots or persistent pools; host caches are warm.

Linux minimal environments share a two-core budget, 2 vCPU / 128 MiB and tool artifacts, with three warmups, 30 samples per cell and randomized order. Firecracker/QEMU use trimmed kernels and static init; Docker's daemon is running. Complete Ubuntu uses a generic kernel, initrd and systemd, with 2 vCPU / 2 GiB; Firecracker/pVisor N=30 and separate QEMU N=10. Shared-host caches, background load and configuration differences limit fine rankings. [Methodology](methodology.md) pins artifacts and conditions.

Tables identify pinned artifacts and measurement dates. Failed or invalid samples are excluded from successful timings and counted separately. Existing measurements have no predefined host-interference filter; all slow valid samples are retained. P95 from 30 or fewer samples is descriptive only; no P99 or stable tail-latency claim is made.

## Data and analysis {#results}

### Prepared environments: lightweight startup {#reference-startup}

| Backend | N | Ready P50 / P95 ms |
|---|---|---|
| Native | 30 | 1.20 / 1.42 |
| pVisor host | 30 | 6.10 / 6.65 |
| pVisor staged | 30 | 14.68 / 15.94 |
| pVisor VM | 30 | 86.29 / 92.99 |
| Docker rootless | 30 | 90.12 / 101.13 |
| Firecracker PCI | 30 | 73.74 / 79.06 |
| QEMU q35 | 30 | 218.12 / 235.27 |
| QEMU microvm | 30 | 88.10 / 103.42 |

pVisor VM, Docker and QEMU microvm have close medians; a few milliseconds do not establish a stable lead. Heavier q35 devices do not represent QEMU's minimum startup path. pVisor's kernel/virtio-fs and reference ext4 differ: this compares complete command paths.

![Prepared-environment first output](../../assets/benchmarks/reference-env-20261004/reference-startup.svg)

### Complete distributions: deployment waiting {#full-ubuntu}

| Backend | N | Ready P50 / P95 ms | Exit P50 / P95 ms |
|---|---:|---|---|
| Native / Fedora | 30 | 1.23 / 1.49 | 1.29 / 1.57 |
| pVisor staged | 30 | 15.04 / 18.32 | 43.75 / 44.52 |
| pVisor VM / host | 30 | 109.69 / 121.62 | 173.57 / 193.94 |
| Firecracker / Ubuntu | 30 | 5644.11 / 6009.57 | 9234.94 / 9626.10 |
| Firecracker / Ubuntu first boot | 30 | 9246.65 / 10293.13 | 12862.17 / 13875.61 |
| QEMU q35 / Ubuntu | 10 | 5428.90 / 7698.78 | 9054.86 / 12078.24 |
| QEMU microvm / Ubuntu | 10 | 7666.69 / 8547.61 | 11199.06 / 12100.94 |

Image-free pVisor uses tool directories directly, reducing full-OS boot waiting. For Ubuntu services and a distribution environment, the seconds above are the cost of obtaining that environment. First cloud-init excludes initial downloading. Complete Ubuntu shares a disk template; QEMU rows form a separate cohort, without pooled distributions. This does not establish “libkrun is 50 times faster than Firecracker.”

![Complete-distribution and image-free deployment](../../assets/benchmarks/full-ubuntu-qemu-20261004/ubuntu-startup.svg)

### First output and cleanup/exit {#reference-exit}

| Backend | Ready P50/P95 ms | Exit P50/P95 ms |
|---|---|---|
| Native | 1.27 / 1.99 | 1.35 / 2.05 |
| pVisor host | 5.98 / 9.91 | 13.23 / 15.78 |
| pVisor staged | 14.61 / 21.83 | 43.52 / 47.36 |
| pVisor VM | 88.46 / 142.99 | 153.11 / 207.68 |
| Docker rootless | 94.29 / 237.58 | 123.38 / 286.71 |
| Firecracker PCI | 74.18 / 92.76 | 101.82 / 121.81 |
| QEMU q35 | 219.74 / 306.82 | 248.39 / 338.34 |
| QEMU microvm | 87.42 / 170.44 | 116.09 / 199.42 |

This independent 30-sample table uses dedicated process waiting for precise exit. VM first output takes about **88 ms**, complete exit **153 ms**; cleanup and recording have a budget too. Ready does not mean a real CLI has initialized or can repair a project.

### macOS / HVF {#macos}

On Apple M4, pVisor VM with 2 vCPU / 128 MiB has first-output P50 **84.35 ms**, P95 **112.14 ms**, across 100 measured samples. Matching macOS Docker/Firecracker/QEMU comparisons are unavailable. Linux and macOS results are not ranked together.

