# How long do developer tools take in pVisor, Docker and lightweight VMs?

## Main conclusions {#conclusions}

**Rootless pVisor host stays close to native tools; staged adds file-access and retained-change costs. pVisor VM takes longer than the measured Firecracker/QEMU configurations for the seven-tool task. Docker VFS creation is costly even where bind-mount tools are fast.**

| Need | Selection implication |
|---|---|
| Local tools and retained edits | Evaluate rootless host/staged |
| Independent guest kernel | Budget complete VM tool time |
| Existing container/Git workflow | Compare costs and required review semantics |

## Motivation {#motivation}

Repository scans, search, builds and dependency installation dominate many Agent tool loops. Choose with those costs alongside startup and review.

## Experiment design {#interpretation}

Shared Linux/x86_64 host, AMD Ryzen 7 9700X, Fedora kernel 7.2.8-200.fc44.x86_64. Launch trees and the private Docker daemon are pinned to host CPUs 0,1; guests have 2 vCPU. Host/staged use rootless_process. Shell VMs use 128 MiB; tool VMs use 16 GiB. Native/Docker memory is not capped: this controls CPU and configured guest RAM, not identical resource enforcement. Tools and inputs are prepared; each run gets a fresh workspace, warm caches, three warmups and 60 measured trials, with seeded randomized backend order. Builds, downloads and input copying are excluded.

Docker Engine 29.7.2 uses a private rootless VFS daemon and writable bind mounts. This does not represent overlay2 or Docker Desktop. Firecracker 1.13.1 PCI runs without jailer; QEMU 10.2.2 uses q35/microvm with private ext4. pVisor VM uses virtio-fs and a different kernel. Kernel, storage, devices and staging semantics remain configuration differences; these results do not isolate the VMM or FUSE alone.

Workload: 2,048 files in 32 directories; 64 MiB read with SHA256 verification; 256 × 64 KiB writes; git status; rg; 64 dependency-free Cargo modules; 32 offline npm packages. Operation timers include checks, excluding launch/exit. Completion includes all seven and exit. Large repositories, cold disks, Internet registries and concurrency throughput are unmeasured.

## Data and analysis {#results}

### Seven operations {#reference-fs}

Measured 2026-10-05, 60/60 valid trials per backend, zero measured failures. Outputs and exits are checked; staged trials additionally check unchanged originals and complete retained edits. All valid slow samples are retained; no timing-based exclusions. P95 is descriptive. Separated clusters show each median and count out of 60 instead of one P50, using the predefined rule in [methodology](methodology.md).

Units: ms; P50 or individual cluster medians with counts.

| Operation | Native | pVisor host | pVisor staged | pVisor VM | Docker rootless / VFS | Firecracker PCI | QEMU q35 | QEMU microvm |
|---|---|---|---|---|---|---|---|---|
| Traverse 2,048 files | 4.68 | 4.69 | 74.90 | 227.11 | 5.03 | 23.30 | 24.99 | 27.76 |
| Read/verify 64 MiB | 33.28 | 32.53 | 68.85 | 154.80 | 32.48 | 81.30 | 39.16 (15/60); 106.63 (45/60) | 41.63 (13/60); 105.45 (47/60) |
| Write 256 files | 3.69 | 3.68 | 27.10 | 159.80 | 3.69 (54/60); 6.80 (6/60) | 43.83 | 5.26 (15/60); 45.30 (45/60) | 5.45 (14/60); 45.22 (46/60) |
| git status | 15.35 | 15.91 | 123.95 | 425.74 | 17.55 | 123.36 | 90.22 | 157.20 |
| Ripgrep search | 8.56 | 8.81 | 91.29 | 460.88 | 9.99 | 14.39 | 14.53 | 15.11 |
| Offline Cargo build | 57.21 (41/60); 144.26 (19/60) | 84.40 | 142.29 | 852.03 | 190.13 | 371.36 | 419.54 | 454.21 |
| Offline npm install | 190.34 | 198.23 | 263.19 | 1662.23 | 273.68 | 565.52 | 583.35 | 619.69 |

Docker bind-mount operation times exclude VFS creation. Staged/VM retain changes for review, unlike direct writable binds. Directory scans, Git and tool loading still add interactive waiting; no single-layer cause follows from this comparison.

### Launch through exit {#complete-task}

Units: seconds; P95 is descriptive, not an upper bound.

| Backend | Valid / failed | Completion P50 s | Completion P95 s |
|---|---|---|---|
| Native | 60 / 0 | 0.51 | 0.70 |
| pVisor host | 60 / 0 | 0.57 | 0.79 |
| pVisor staged | 60 / 0 | 1.29 | 1.58 |
| pVisor VM | 60 / 0 | 6.66 | 8.00 |
| Docker rootless / VFS | 60 / 0 | 5.68 | 6.76 |
| Firecracker PCI | 60 / 0 | 2.16 | 2.35 |
| QEMU q35 | 60 / 0 | 2.44 | 2.75 |
| QEMU microvm | 60 / 0 | 2.45 | 2.79 |

### Complete Ubuntu file operations {#full-ubuntu}

Independent 2026-10-04 Firecracker/Ubuntu cohort, 2 vCPU / 16 GiB, N=10, three warmups. P50 ms; different OS/tools/storage, without pooled distributions. QEMU/Ubuntu has no seven-operation measurement.

| Operation | Firecracker / Ubuntu P50 ms |
|---|---|
| Traverse 2,048 files | 18.64 |
| Read/verify 64 MiB | 115.51 |
| Write 256 files | 36.07 |
| git status | 136.50 |
| Ripgrep search | 20.40 |
| Offline Cargo build | 969.09 |
| Offline npm install | 1079.89 |

[Startup](startup.md) · [Repair tasks](agent-tasks.md) · [Apply/drop](apply.md)

### Downloads and reproduction {#run}

[Derived table CSV](filesystem.csv) · [Runtime statistics](runtime-summary.csv) · [Sources and artifacts](runtime-provenance.csv) · [Evidence source summary](evidence-sources.csv) · [Method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
