# How soon is an environment Ready, and what do three tool rounds cost?

## Main conclusions {#conclusions}

**With two CPUs and warm host caches, median Python-payload Ready is 201.19 ms for pVisor host, 209.56 ms staged and 749.15 ms in VM; Docker takes 450.94 ms and Firecracker 854.79 ms. The VM's seven operation clocks sum to 3323.01 ms in round one and 1758.28 ms in round three. Budget launch and tool execution separately.**

| Scenario | Decision |
| --- | --- |
| Create a prepared environment | Compare first payload Ready; exit is outside startup |
| Repeated tool calls in one environment | Use round three while retaining the first-round cost |
| Frequent readonly opens or small stream writes | Check the targeted cases; writeback is an explicit option |

<!-- measurement-status:start -->

Five independent environments for each of nine configurations: 45 total, three resident rounds and 1,215 operations, all correctness checks passed. Quick measurements have no external interference gate; historical data remain separate.

<!-- measurement-status:end -->

## Motivation {#motivation}

Frequent environment creation makes command availability the first wait. A resident Agent repeatedly traverses, searches, builds and installs, so initial access and later calls need separate budgets. Repeated opens and small stream writes test whether a particular call pattern suits the available optimizations.

## Experiment design {#interpretation}

Eight backends share prepared offline tools and fixed inputs, with an additional VM explicitly setting `PVISOR_VM_FS_WRITEBACK=1`. The default VM has writeback disabled; guest cache is enabled and DAX disabled in both. Ordinary CLI rootfs/workspace lowers are Mutable: repetition does not make them eligible for directory images, adaptive metadata prefetch or immutable-rootfs data-page retention.

Linux x86_64 / Ryzen 7 9700X / Fedora 7.2.8-200.fc44.x86_64; Docker 29.8.2 rootless overlay2, Firecracker PCI 1.13.1 and QEMU 10.2.2. Firecracker/QEMU use an independently customized reference kernel, not stock kernels. The whole parent has CPU affinity 0,1, a two-CPU quota, 16 GiB and zero swap; each VM has 2 vCPU/16 GiB. cpuset was not delegated, so placement uses inherited affinity. Host workspace/stage storage is Btrfs and reference VM storage ext4; private /tmp is a 2 GiB tmpfs and tools retain executor-default temporary storage.

Five fresh environments per configuration, no separate warmups, seed 20261016 randomizes each sampling round. Every environment executes the seven tools and two targeted cases three times while retaining its VM/container, workspace and task caches. Cargo/npm outputs start empty and remain for later rounds; the write task overwrites the same 256 files in rounds two and three. Every measured Python is frozen 3.14.7 with bytecode disabled. Three resident rounds are not fifteen independent samples; warm host caches do not mean every guest metadata TTL remains valid.

Startup runs from host executor creation to the common Python payload's unique `REFERENCE_READY`, including interpreter and shared module imports, excluding tool operations, exit and teardown. Each operation uses its internal monotonic clock, excluding child Python startup and VM exit; written-byte readback follows that clock. Normal exit, output, cache inheritance, Run Bundle, staging isolation and full written bytes must still pass. Failures are recorded separately and every valid slow sample is retained. There is no quiet wait, external interference filtering or new full input-inventory scan; N=5 establishes neither stable tails nor whole-Agent speedup.

## Data and analysis {#results}

### Python-payload startup {#quick-results}

Milliseconds, median and minimum–maximum, N=5 per configuration. This is command Ready, not full-distribution boot.

| Runtime | N | Ready ms | Min–max ms |
| --- | --- | --- | --- |
| Native | 5 | 180.43 | 178.95–181.15 |
| pVisor host | 5 | 201.19 | 195.70–203.26 |
| pVisor staged | 5 | 209.56 | 206.75–215.36 |
| pVisor VM | 5 | 749.15 | 707.91–806.28 |
| pVisor VM + writeback | 5 | 746.21 | 704.93–832.73 |
| Docker rootless | 5 | 450.94 | 323.55–557.16 |
| Firecracker PCI | 5 | 854.79 | 832.52–865.48 |
| QEMU q35 | 5 | 486.80 | 478.82–509.91 |
| QEMU microvm | 5 | 364.31 | 359.88–433.75 |

In this quick sample, the default VM reaches Ready before Firecracker: a -105.64 ms median difference, paired-bootstrap 95% interval [-126.17, -48.51] ms. Docker and both QEMU configurations still reach Ready sooner.

### Three resident tool rounds {#reference-fs}

Milliseconds, N=5 per round. Sum the seven actual operation clocks within each environment before taking the median. This is neither a sum of medians nor a wall clock including wrappers, byte readback and extra cases.

| Runtime | R1 ms | R2 ms | R3 ms |
| --- | --- | --- | --- |
| Native | 331.41 | 226.80 | 225.48 |
| pVisor host | 351.67 | 239.40 | 237.94 |
| pVisor staged | 798.59 | 526.49 | 518.99 |
| pVisor VM | 3323.01 | 1765.81 | 1758.28 |
| pVisor VM + writeback | 2839.84 | 1826.43 | 1693.68 |
| Docker rootless | 332.76 | 212.50 | 213.37 |
| Firecracker PCI | 1237.13 | 301.14 | 332.43 |
| QEMU q35 | 977.96 | 267.86 | 266.27 |
| QEMU microvm | 1095.77 | 260.47 | 247.01 |

Later rounds combine file caching and incremental tool behavior: Cargo does not rebuild unchanged sources and npm retains node_modules. Use this table to budget repeated calls; do not attribute the entire reduction to one filesystem optimization.

<details markdown="1">
<summary>All seven tools by resident round (ms, N=5 per round)</summary>

#### R1

| Operation | Native | pVisor host | pVisor staged | pVisor VM | pVisor VM + writeback | Docker rootless | Firecracker PCI | QEMU q35 | QEMU microvm |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Traverse 2,048 files | 4.63 | 4.60 | 79.87 | 216.15 | 198.66 | 4.70 | 24.28 | 16.57 | 17.79 |
| Read and verify 64 MiB | 32.87 | 32.81 | 78.53 | 121.60 | 118.59 | 33.06 | 81.17 | 37.81 | 42.45 |
| Write/overwrite 256 files | 3.80 | 3.90 | 33.27 | 187.78 | 148.01 | 4.12 | 40.46 | 5.22 | 5.31 |
| git status | 15.69 | 15.67 | 159.19 | 520.03 | 368.02 | 15.61 | 126.32 | 83.96 | 168.22 |
| Ripgrep search | 8.73 | 8.83 | 112.86 | 425.47 | 401.23 | 8.93 | 14.05 | 13.24 | 14.15 |
| Offline Cargo build | 53.83 | 60.36 | 92.84 | 622.86 | 548.90 | 49.29 | 397.35 | 331.11 | 351.91 |
| Offline npm install | 212.17 | 225.46 | 244.94 | 1083.78 | 1053.20 | 217.76 | 550.02 | 453.31 | 506.73 |

#### R2

| Operation | Native | pVisor host | pVisor staged | pVisor VM | pVisor VM + writeback | Docker rootless | Firecracker PCI | QEMU q35 | QEMU microvm |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Traverse 2,048 files | 4.91 | 4.95 | 87.55 | 138.17 | 138.74 | 4.92 | 4.21 | 4.19 | 4.11 |
| Read and verify 64 MiB | 27.12 | 27.14 | 46.55 | 41.60 | 38.01 | 27.22 | 27.76 | 27.86 | 27.88 |
| Write/overwrite 256 files | 3.84 | 4.10 | 51.03 | 110.07 | 143.37 | 3.87 | 22.49 | 21.82 | 23.55 |
| git status | 2.58 | 2.60 | 13.13 | 39.12 | 38.34 | 2.56 | 2.28 | 2.94 | 2.50 |
| Ripgrep search | 8.58 | 9.66 | 112.58 | 455.77 | 384.39 | 8.27 | 6.38 | 7.23 | 5.91 |
| Offline Cargo build | 7.88 | 8.26 | 14.98 | 49.16 | 47.07 | 5.42 | 4.93 | 5.31 | 5.15 |
| Offline npm install | 173.00 | 185.30 | 200.13 | 938.55 | 935.41 | 161.12 | 232.13 | 197.63 | 191.20 |

#### R3

| Operation | Native | pVisor host | pVisor staged | pVisor VM | pVisor VM + writeback | Docker rootless | Firecracker PCI | QEMU q35 | QEMU microvm |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Traverse 2,048 files | 4.56 | 4.77 | 75.38 | 194.63 | 152.33 | 4.70 | 4.29 | 4.18 | 4.16 |
| Read and verify 64 MiB | 27.13 | 27.11 | 48.97 | 38.02 | 37.38 | 27.16 | 27.67 | 28.00 | 27.80 |
| Write/overwrite 256 files | 4.52 | 4.45 | 50.46 | 103.77 | 138.08 | 4.49 | 16.86 | 21.96 | 21.36 |
| git status | 2.48 | 3.02 | 13.30 | 36.29 | 38.08 | 2.79 | 2.55 | 2.52 | 2.26 |
| Ripgrep search | 7.46 | 8.88 | 112.74 | 437.97 | 359.88 | 8.60 | 7.98 | 6.75 | 6.03 |
| Offline Cargo build | 7.37 | 7.62 | 12.68 | 44.66 | 43.76 | 5.03 | 5.20 | 4.77 | 4.65 |
| Offline npm install | 171.77 | 182.80 | 208.14 | 908.41 | 861.16 | 161.06 | 268.70 | 197.83 | 178.75 |

</details>

### Hot opens and small stream writes {#cache-status}

Milliseconds, N=5 per round, medians; each cell is R1 / R2 / R3.

Hot open primes the same file once outside the timer, then performs 10,000 open/fstat/close sequences without reading content. Each stream round creates one new file, performs 8,192 unbuffered 1 KiB writes, then fsync/close. The write column stops before fsync; write + fsync + close retains the explicit synchronization cost. Every configuration executes the same calls and verifies all 8 MiB byte-for-byte after timing.

| Runtime | open/fstat R1 / R2 / R3 ms | 8 MiB write R1 / R2 / R3 ms | write + fsync + close R1 / R2 / R3 ms |
| --- | --- | --- | --- |
| Native | 10.11 / 10.11 / 10.12 | 5.89 / 5.92 / 5.88 | 9.67 / 9.61 / 9.55 |
| pVisor host | 10.18 / 10.19 / 10.13 | 5.84 / 5.99 / 5.91 | 9.37 / 9.62 / 9.55 |
| pVisor staged | 409.28 / 409.54 / 398.16 | 219.54 / 203.09 / 227.36 | 224.42 / 207.18 / 233.35 |
| pVisor VM | 10.26 / 11.88 / 10.27 | 468.78 / 468.24 / 465.69 | 471.84 / 478.35 / 475.88 |
| pVisor VM + writeback | 10.35 / 10.41 / 10.24 | 3.78 / 5.90 / 8.16 | 15.23 / 12.81 / 21.59 |
| Docker rootless | 10.37 / 10.51 / 10.49 | 5.92 / 6.01 / 5.97 | 9.52 / 9.04 / 9.53 |
| Firecracker PCI | 8.37 / 8.33 / 8.34 | 3.37 / 4.36 / 3.64 | 9.42 / 9.31 / 10.06 |
| QEMU q35 | 8.22 / 8.35 / 8.23 | 2.94 / 3.24 / 2.88 | 9.43 / 9.95 / 9.22 |
| QEMU microvm | 8.36 / 8.24 / 8.22 | 2.93 / 3.65 / 2.97 | 10.79 / 11.20 / 9.68 |

One specific case with a lower observed cost is round-three hot open/fstat: 10.24 ms for VM + writeback versus 10.49 ms for Docker across 10,000 calls, a -0.25 ms median difference with 95% interval [-0.48, -0.10] ms. The margin is small and limited to this content-free hot loop. Default VM versus Docker has no detected difference here; the opt-in configuration does not establish that writeback accelerates metadata.

Deferred readonly open lets the VM keep this content-free hot path in the guest; rg/Cargo file reads exercise a different path. Writeback shortens small-write acceptance, while synchronization still costs time: the write column alone cannot establish a durability wait. Lower sample values in one case do not imply a lead on all workloads. Downloads include same-cohort paired-bootstrap intervals from only five independent environments, without multiple-comparison adjustment.

### Historical sources {#complete-task}

Earlier single-pass and N30 complete-task data remain in the [old quick statistics](filesystem-quick.csv), [historical N30 statistics](filesystem.csv) and [historical provenance](filesystem-provenance.csv). Their launch/result/exit clocks are not pooled with current Ready or resident-round observations. These samples do not cover full Ubuntu, macOS, cold disks, large repositories or concurrent throughput.

### Downloads and reproduction {#run}

[Per-round statistics CSV](filesystem-resident.csv) · [Same-cohort differences and intervals](filesystem-resident-comparisons.csv) · [Provenance and conditions](filesystem-resident-provenance.csv)

Measured 2026-10-16, B-FS-TOOLS, GNU release/gateway CLI and newly built matching firmware. Raw samples and frozen entry points are local at `benchmark/.data/fr3-20261016b/` and `benchmark.new/filesystem/.data/retest-20261016/resident-fixed/`. [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#ready-and-resident-filesystem-rounds) · [Engineering evidence scope](../design/filesystem-performance-analysis.md#filesystem-status) · [Method](methodology.md).
