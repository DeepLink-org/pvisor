# How long do developer tools take in pVisor, Docker and lightweight VMs?

**pVisor host staged is close to Docker for offline npm installs and writes 256 files in about 27 ms; pVisor VM is slower than the measured Firecracker and QEMU configurations for file-heavy Git, search and npm workloads.** The tables place the latest pVisor measurements alongside containers, lightweight VMs and complete Ubuntu VMs to help assess performance for your tasks.

## Tool timings {#reference-fs}

Values are **P50 milliseconds**; lower is faster. Each operation includes tool execution and validation, excluding environment startup and teardown. pVisor and native columns were measured on 2026-10-05; Docker, Firecracker and QEMU columns on 2026-10-04. All use the same workloads and a two-core budget on the same host, with 3 warmups and 30 samples per column. pVisor VM uses **2 vCPU / 4 GiB**, reference VMs **2 vCPU / 16 GiB**. Configurations and batches differ: these are measured performance levels, rather than a strict A/B changing only the runtime.

| Operation | Native | pVisor host staged | pVisor VM | Docker bind mount | Firecracker PCI | QEMU q35 | QEMU microvm |
|---|---:|---:|---:|---:|---:|---:|---:|
| Traverse 2,048 files | 4.76 | 77.12 | 166.90 | 5.06 | 21.60 | 14.89 | 18.07 |
| Read and verify 64 MiB | 32.29 | 67.84 | 117.82 | 33.24 | 81.63 | 39.21 | 40.72 |
| Write 256 files | 3.82 | 27.05 | 141.59 | 3.95 | 43.68 | 5.26 | 5.20 |
| git status | 14.79 | 124.37 | 371.58 | 16.07 | 129.37 | 86.26 | 123.93 |
| Ripgrep search | 7.38 | 88.07 | 442.74 | 8.03 | 15.41 | 12.19 | 12.58 |
| Offline Cargo build | 51.55 | 73.42 | 489.18 | 56.40 | 354.65 | 324.62 | 366.10 |
| Offline npm install | 170.70 | 228.72 | 1330.95 | 231.45 | 556.70 | 603.25 | 614.88 |

Docker file access is close to native execution. Host staged npm installs take about **229 ms**, versus Docker’s **231 ms**; the small Cargo build takes about **73 ms**, versus **56 ms**. Directory scans, Git and search still add noticeable waiting. pVisor VM npm installs take about **1.33 s**, versus **0.56–0.61 s** for lightweight Firecracker/QEMU; search takes about **443 ms**, versus **12–15 ms**. For frequent tool runs, consider execution time alongside [startup time](startup.md) and staging/review requirements.

### Complete Ubuntu VM comparison {#full-ubuntu}

Complete Ubuntu uses a distribution kernel, initrd, systemd and private ext4. Firecracker uses 2 vCPU / 16 GiB, 3 warmups and 10 samples (2026-10-04). These are the same seven workloads; pVisor columns use the latest measurements above. Units remain **P50 milliseconds**.

| Operation | pVisor host staged | pVisor VM | Firecracker / Ubuntu |
|---|---:|---:|---:|
| Traverse 2,048 files | 77.12 | 166.90 | 18.64 |
| Read and verify 64 MiB | 67.84 | 117.82 | 115.51 |
| Write 256 files | 27.05 | 141.59 | 36.07 |
| git status | 124.37 | 371.58 | 136.50 |
| Ripgrep search | 88.07 | 442.74 | 20.40 |
| Offline Cargo build | 73.42 | 489.18 | 969.09 |
| Offline npm install | 228.72 | 1330.95 | 1079.89 |

pVisor VM completes the small Cargo build faster than the measured Ubuntu VM, but is slower at traversal, search and small-file writes. See [Ubuntu startup](startup.md#full-ubuntu) for distribution boot times, and [runtime comparisons](compare-runtimes.md#full-ubuntu) for QEMU’s complete-Ubuntu repair/tests and CLI loops; that configuration has no measurements for these seven filesystem operations.

## Complete-task waiting {#results}

These timings include launch, all seven operations in sequence and teardown with persisted staged changes. Image preparation and input copying are excluded. Units are **seconds**. P95 means approximately 95% of samples complete within that time.

| Execution mode | P50 | P95 |
|---|---:|---:|
| Native | 0.45 | 0.52 |
| pVisor host staged | 1.11 | 1.26 |
| pVisor VM | 4.08 | 9.30 |
| pVisor VM, independent measurement | 4.08 | 4.46 |
| Docker bind mount | 0.97 | 1.19 |
| Firecracker PCI | 2.37 | 2.46 |
| QEMU q35 | 1.82 | 2.37 |
| QEMU microvm | 1.77 | 2.15 |

Docker and reference-VM rows use the 2026-10-04 configurations above, including their startup and exit but excluding image preparation and input copying.

VM medians are similar across the two measurements, but slower tasks vary substantially. Allow headroom for interactive workflows and task timeouts; **4.08 s** is not an execution-time upper bound. Each sample group is summarized separately.

## Choosing an execution mode {#conclusions}

| Need | Choose | Cost to consider |
|---|---|---|
| Run host tools and retain changes for review and application | Host staged | About 1.11 s for this complete task; repository scans, Git and search are slower than native |
| Use an independent guest kernel and retain workspace changes | VM | About 4.08 s for this complete task; tool startup, Git, builds and dependency installation take longer |

Host staged and VM retain workspace changes until apply. Choose the required isolation using [executor boundaries](../security/executor-boundaries.md) and [isolation checks](isolation-tests.md). See [apply/drop](apply.md) for application costs and [complete tool tasks](agent-tasks.md) for repair and test workflows.

## Test conditions {#interpretation}

pVisor tests use Linux, a local release build and warm host caches. Host tests have a two-core budget; pVisor VMs use **2 vCPU / 4 GiB**. Each group has three warmups and 30 measured samples. Host staged uses rootless execution. Every included task passes output validation; staged modes also verify that original host files are not modified.

The workload contains 2,048 files in 32 directories; a 64 MiB read with SHA256 verification; 256 × 64 KiB writes; a Cargo build with 64 dependency-free modules; and installation of 32 local npm packages. Each execution uses a fresh workspace. Tools and dependencies are prepared, with no Internet access during testing.

These results cover small, offline workloads with warm caches. Measure your own tasks for large repositories, cold disks, real npm registries or concurrent throughput. Host CPUs are shared, so slower-sample timings can vary in particular.

Tables identify pinned artifacts and measurement dates. Failed or invalid samples are excluded from successful timings and counted separately. Existing measurements have no predefined host-interference filter; all slow valid samples are retained. P95 from 30 or fewer samples is descriptive only; no P99 or stable tail-latency claim is made.

## Data sources {#run}

pVisor and native measurements come from one batch on **2026-10-05**. Docker, Firecracker, QEMU and complete-Ubuntu references were measured on **2026-10-04**. The independent pVisor VM measurement is shown separately; percentiles are not pooled across batches.

 ·  ·  · [Methodology and artifacts](methodology.md)
