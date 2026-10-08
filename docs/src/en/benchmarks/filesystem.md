# How long do development tools take across pVisor, Docker and lightweight VMs?

## Conclusions {#conclusions}

**Seven tools through exit take 0.47 s in pVisor host, 1.09 s in staged and 4.27 s in VM; Docker takes 0.82 s, Firecracker 2.29 s and QEMU microvm 1.50 s. Staging adds file-access cost for review, and VM tool costs remain substantial.**

| Need | Selection implication |
| --- | --- |
| Local execution with retained changes | Evaluate host/staged and the complete review workflow |
| Independent guest kernel | Budget both startup and VM tool waiting |
| Concurrent or idle environments | Require fixed-budget throughput and physical-memory measurements |

These timings are the 2026-10-06 cross-runtime baseline and do not include subsequent cache optimizations. Current cache experiments do not establish new performance conclusions for default staged, VM or complete review tasks; use this baseline for selection.

## Motivation {#motivation}

Agent tool loops traverse, read, write, search, compile and install dependencies. Compare individual operations and complete waiting so that fast startup does not hide file-heavy task costs.

## Experiment design {#interpretation}

Linux x86_64, AMD Ryzen 7 9700X, Fedora 7.2.8-200.fc44.x86_64. Tool payloads and the private Docker daemon are pinned to CPUs 0,1; container initialization affinity over its complete lifetime is unverified. VMs use 2 vCPU, 128 MiB for the shell probe and 16 GiB for tools. Native/Docker memory is uncapped, so these results do not establish capacity under identical resource limits. Host/staged use rootless_process.

All backends share offline tools and fixed inputs, with a new workspace per trial. Warm caches, three warmups and 60 measured samples per cell; backends alternate in seeded randomized order. Preparation, builds, image import and fixture resets are outside timing; complete tasks include launch and exit. Docker Engine 29.7.2 uses a private rootless **overlay2** daemon, the classic image store and writable bind mounts. Firecracker 1.13.1 PCI runs without jailer; QEMU 10.2.2 uses q35/microvm and private ext4. pVisor VM uses virtio-fs and its own firmware. Kernels, storage and staging semantics differ: these are task costs for the stated configurations, not pure VMM or security rankings.

The workload traverses 2,048 files in 32 directories, reads and SHA256-checks 64 MiB, writes 256 × 64 KiB, runs git status and rg, compiles 64 dependency-free Cargo modules and installs 32 offline npm packages. Operations include validation but exclude launch/exit; the complete task includes all seven and exit. Large repositories, cold disks, online registries and concurrent throughput are unmeasured.

These samples do not verify equal Node/npm compile-cache state across backends. A fresh workspace does not establish an empty tool cache. npm and complete-task differences include each configuration's cache behavior; they cannot all be attributed to FUSE, staging or the VMM.

## Data and analysis {#results}

Measured on 2026-10-06: each backend/workload has 60/60 valid samples and zero measured failures. Outputs, exit and execution records must pass validation; staging also requires unchanged host originals and all 256 upper files with the expected sizes. Complete written bytes were not independently checked. Every valid slow sample is kept, with no timing-based exclusions. Tables normally show P50; separated distributions show cluster medians and counts. P95 is descriptive only. Raw reports, binaries and input/source manifests stay in ignored `.data/`; public CSVs retain workload, cohort and provenance associations.

### Seven tool operations {#reference-fs}

Milliseconds; P50 or separated-cluster medians and counts.

| Operation | Native | pVisor host | pVisor staged | pVisor VM | Docker rootless / overlay2 | Firecracker PCI | QEMU q35 | QEMU microvm |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Traverse 2,048 files | 4.64 | 4.63 | 73.53 | 152.97 | 4.62 | 23.43 | 15.92 | 19.91 |
| Read and verify 64 MiB | 32.62 | 31.73 | 66.98 | 116.45 | 32.64 | 79.79 | 37.43 | 40.19 |
| Write 256 files | 3.62 | 3.66 | 26.23 | 135.16 | 3.82 | 43.11 | 5.02 | 4.99 |
| git status | 14.50 | 14.30 | 120.01 | 352.11 (43/60); 665.46 (17/60) | 14.42 | 129.97 | 80.27 | 155.03 |
| Ripgrep search | 7.35 | 7.22 | 84.81 | 434.07 | 6.86 | 15.82 | 12.57 | 13.03 |
| Offline Cargo build | 51.00 | 50.81 | 71.76 | 482.69 | 48.14 | 373.88 | 260.34 | 296.94 |
| Offline npm install | 170.56 | 170.22 | 226.67 | 1320.16 | 215.77 | 546.50 | 399.42 | 421.37 |

### Launch through exit {#complete-task}

| Runtime | Valid / failed | Completion P50 s | Completion P95 s |
| --- | --- | --- | --- |
| Native | 60 / 0 | 0.45 | 0.46 |
| pVisor host | 60 / 0 | 0.47 | 0.48 |
| pVisor staged | 60 / 0 | 1.09 | 1.13 |
| pVisor VM | 60 / 0 | 4.27 | 4.60 |
| Docker rootless / overlay2 | 60 / 0 | 0.82 | 0.85 |
| Firecracker PCI | 60 / 0 | 2.29 | 2.33 |
| QEMU q35 | 60 / 0 | 1.49 | 1.51 |
| QEMU microvm | 60 / 0 | 1.50 | 1.54 |

Staged minus Docker completion median is +268.81 ms, with a paired-bootstrap 95% interval of [+264.64, +271.15] ms. VM minus QEMU microvm is +2778.04 ms, interval [+2754.17, +2807.74] ms. Staging capabilities and storage configurations differ; total gaps do not identify the cost of one component.

<a id="full-ubuntu"></a>
Full Ubuntu filesystem workloads have not been retested with the current artifacts.

### Where cache optimizations apply {#cache-status}

Immutable-lower physical metadata caching has an independent engineering A/B, with gains limited to Linux HOST FUSE configurations that explicitly declare and maintain lower stability. Host rootfs, OCI extraction directories and lazy-image local projections receive no automatic immutability promise. The experiment does not cover the complete review workflow and cannot be used to rescale staged or VM timings above. See the [immutable-lower cache analysis](../design/filesystem-performance-analysis.md#immutable-lower-cache) for the design, results and confidence intervals.

Extended kernel caching is an explicit Linux HOST API option. Writable long-lived metadata caching requires immutable lowers, exclusive upper/work ownership and stable backing metadata; KEEP_CACHE supports stable read-only regular files only. Configurations incompatible with journals, read-observation metrics or custom path policies are rejected rather than silently omitting observations. Current-source formal timing was contaminated by concurrent build checks, so no speedup magnitude has passed acceptance. This mode is not integrated into default `pvisor run` or VM execution. See the [kernel-cache analysis](../design/filesystem-performance-analysis.md#host-kernel-cache) for capability boundaries and validation status.

### Downloads and reproduction {#run}

[Derived statistics CSV](filesystem.csv) · [All runtime statistics](runtime-summary.csv) · [Differences and 95% confidence intervals](runtime-comparisons.csv) · [Source and artifact provenance](runtime-provenance.csv) · [Method](methodology.md) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
