# How long does a usable environment take to start?

## Main conclusions {#conclusions}

**Prepared pVisor VM first output is 99.76 ms, versus Firecracker 74.74 ms and QEMU microvm 86.60 ms. Rootless host/staged start sooner. Include process exit and actual tools when budgeting short tasks.**

| Need | Selection implication |
|---|---|
| Local tools and retained edits | Evaluate rootless host/staged |
| Independent guest kernel | Budget complete VM tool time |
| Existing container/Git workflow | Compare costs and required review semantics |

## Motivation {#motivation}

Disposable environments repeatedly pay startup cost. First useful output and process exit differ; persistent pools amortize them.

## Experiment design {#interpretation}

Shared Linux/x86_64 host, AMD Ryzen 7 9700X, Fedora kernel 7.2.8-200.fc44.x86_64. Launch trees and the private Docker daemon are pinned to host CPUs 0,1; guests have 2 vCPU. Host/staged use rootless_process. Shell VMs use 128 MiB; tool VMs use 16 GiB. Native/Docker memory is not capped: this controls CPU and configured guest RAM, not identical resource enforcement. Tools and inputs are prepared; each run gets a fresh workspace, warm caches, three warmups and 60 measured trials, with seeded randomized backend order. Builds, downloads and input copying are excluded.

Docker Engine 29.7.2 uses a private rootless VFS daemon and writable bind mounts. This does not represent overlay2 or Docker Desktop. Firecracker 1.13.1 PCI runs without jailer; QEMU 10.2.2 uses q35/microvm with private ext4. pVisor VM uses virtio-fs and a different kernel. Kernel, storage, devices and staging semantics remain configuration differences; these results do not isolate the VMM or FUSE alone.

Ready ends at the checked shell marker; Exit at process termination. These are fresh starts without RAM snapshots or pools. Full Agent CLI readiness, cold disks and cross-platform rankings are outside this probe.

## Data and analysis {#results}

### Prepared environments {#reference-startup}

Measured 2026-10-05, 60/60 valid trials per backend, zero measured failures. Outputs and exits are checked; staged trials additionally check unchanged originals and complete retained edits. All valid slow samples are retained; no timing-based exclusions. P95 is descriptive. Separated clusters show each median and count out of 60 instead of one P50, using the predefined rule in [methodology](methodology.md).

<a id="reference-exit"></a>

| Backend | Valid / failed | Ready P50 ms | Ready P95 ms | Exit P50 ms | Exit P95 ms |
|---|---|---|---|---|---|
| Native | 60 / 0 | 1.24 | 1.57 | 1.31 | 1.65 |
| pVisor host | 60 / 0 | 12.22 | 13.67 | 23.82 | 24.56 |
| pVisor staged | 60 / 0 | 24.99 | 27.93 | 33.77 | 54.32 |
| pVisor VM | 60 / 0 | 99.76 | 110.86 | 155.02 | 176.06 |
| Docker rootless / VFS | 60 / 0 | 3380.93 | 3622.36 | 4487.13 | 4724.50 |
| Firecracker PCI | 60 / 0 | 74.74 | 81.13 | 99.62 | 107.00 |
| QEMU q35 | 60 / 0 | 213.89 | 223.19 | 239.82 | 254.20 |
| QEMU microvm | 60 / 0 | 86.60 | 101.41 | 110.03 | 123.08 |

VFS container creation includes writable-layer copying. Its seconds-scale launch cost does not establish typical Docker startup performance. Reused environments amortize creation.

### Complete Ubuntu deployment {#full-ubuntu}

Independent 2026-10-04 cohorts, two cores / 2 GiB, warm caches and three warmups. P50 ms; pVisor/Firecracker N=30, QEMU separate N=10. Ubuntu uses a distribution kernel, initrd, systemd and cloud-init, while pVisor reuses host tool directories.

| Deployment | N | Ready P50 ms | Exit P50 ms |
|---|---|---|---|
| pVisor VM / host tools | 30 | 109.69 | 173.57 |
| Firecracker / Ubuntu | 30 | 5644.11 | 9234.94 |
| Firecracker / Ubuntu first boot | 30 | 9246.65 | 12862.17 |
| QEMU q35 / Ubuntu | 10 | 5428.90 | 9054.86 |
| QEMU microvm / Ubuntu | 10 | 7666.69 | 11199.06 |

This answers deployment waiting with different OS configurations, not a pure VMM ranking.

### macOS / HVF {#macos}

Independent Apple M4, 2 vCPU / 128 MiB, N=100: first-output P50 84.35 ms, descriptive P95 112.14 ms. Matching macOS Docker/Firecracker/QEMU comparisons are unavailable.

### Downloads and reproduction {#run}

[Derived table CSV](startup.csv) · [Runtime statistics](runtime-summary.csv) · [Sources and artifacts](runtime-provenance.csv) · [Evidence source summary](evidence-sources.csv) · [Method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
