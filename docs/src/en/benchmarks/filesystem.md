# Filesystem and developer-tool performance

## Main conclusions {#conclusions}

**The FUSE/no-stage control is faster than same-batch staged execution, but metadata operations remain substantially slower than native.** Direct host-file access through FUSE takes about **22.04 ms** for traversal, **39.17 ms** for reading 64 MiB and **12.52 ms** for writing 256 files; same-batch staged takes **78.06, 68.56 and 206.40 ms**. This uses a dedicated passthrough benchmark adapter, rather than direct host access or a built-in CLI mode.

**In the historical Docker comparison, pVisor staged offline npm installation is close to Docker, and bulk reads add relatively little waiting; small-file and metadata operations are substantially slower.** Reading/verifying 64 MiB takes staged about **48 ms**, Docker **33 ms**. Offline npm takes **223–256 ms**, Docker **231 ms**. Traversal, file creation, Git and search have larger gaps and remain the main weakness in these workloads.

**pVisor VM performance depends on workload: bulk reads and small Cargo builds beat the measured complete Ubuntu VM, while most file-heavy operations are slower.** A 64 MiB read takes about **89 ms**, Firecracker/Ubuntu **116 ms**. Cargo takes **0.55–0.56 s**, Ubuntu **0.97 s**. Traversal, search, small-file writes and npm lag substantially; overall file access is also slower than Docker bind mounts.

Staged has the lower tool budget for staging, review and selective application. Choose the VM when an independent guest kernel is needed, allowing for cumulative small-file waiting.

## Motivation {#motivation}

Developer tools repeatedly inspect directories/attributes, open files, install dependencies and write build outputs. Users need actual performance differences against familiar containers and VMs, alongside staged changes and execution boundaries.

## Experiment design {#interpretation}

Historical comparisons use complete tool environments. Docker uses rootless Engine and writable bind mounts. Firecracker uses complete Ubuntu with its distribution generic kernel, initrd, systemd and private ext4. pVisor staged and VM use staged file views. Linux same-host measurements share two cores, 2 vCPU / 16 GiB VMs and warm host caches. Docker comparisons have 30 samples per cell; Ubuntu comparisons ten, both with three warmups.

Seven operations use fixed inputs. Timings include tools and result verification, excluding environment boot, image preparation and downloads. Traversal covers 2,048 files across 32 directories; reads verify a 64 MiB SHA256; writes create 256 files. Cargo builds 64 dependency-free modules; npm installs 32 local packages.

**Ranges below are the two complete-tool configurations' respective P50 values, describing configuration differences rather than sample variation or confidence intervals.** Docker and Ubuntu are independent comparisons, without pooled samples. Different tools, kernels and file paths prevent attributing all differences to filesystems alone. Reports pin artifacts and retain P95/P99; detailed directory/lazy measurements are in [technical analysis](../design/filesystem-performance-analysis.md).

A separate 2026-10-05 batch compares native, direct host, host + FUSE passthrough and host staged, each with three warmups and 30 measurements. Cases run serially in shuffled order each round. All three pVisor groups use one pinned release artifact and verify `rootless_process`; the seven workloads are unchanged. This artifact differs from the historical Docker/Ubuntu comparisons below, so results appear in separate tables without pooling samples.

## Data and analysis {#results}

### pVisor versus Docker and complete Ubuntu VMs {#filesystem-service}

Units: **P50 ms; lower is faster**.

| Workload | Native | pVisor staged | pVisor VM | Docker bind mount | Firecracker / Ubuntu |
|---|---:|---:|---:|---:|---:|
| Traverse 2,048 files | 4.85–5.04 | 177.80–180.13 | 291.82–310.54 | 5.06 | 18.64 |
| Read/verify 64 MiB | 33.07–33.29 | 48.12–48.77 | 88.83–89.27 | 33.24 | 115.51 |
| Write 256 files | 3.75–3.96 | 186.95–189.28 | 134.37–144.66 | 3.95 | 36.07 |
| git status | 14.66–15.75 | 172.18–177.81 | 456.21–613.69 | 16.07 | 136.50 |
| rg search | 7.58–7.98 | 140.52–144.61 | 521.65–545.82 | 8.03 | 20.40 |
| Offline Cargo build | 52.79–58.71 | 104.21–112.80 | 549.57–563.24 | 56.40 | 969.09 |
| Offline npm install | 183.46–218.82 | 222.97–256.29 | 1727.04–2260.17 | 231.45 | 1079.89 |

### Same-batch FUSE/no-stage versus staged {#host-direct}

Units: **P50 ms; lower is faster**. All four columns come from one batch, with 30 samples passing correctness checks per cell.

| Workload | Native | pVisor direct host (no FUSE) | pVisor host + FUSE (no stage, control adapter) | pVisor host staged |
|---|---:|---:|---:|---:|
| Traverse 2,048 files | 4.88 | 4.88 | 22.04 | 78.06 |
| Read/verify 64 MiB | 32.83 | 32.18 | 39.17 | 68.56 |
| Write 256 files | 4.12 | 4.07 | 12.52 | 206.40 |
| git status | 14.99 | 15.12 | 40.49 | 173.48 |
| rg search | 8.39 | 7.70 | 28.07 | 91.38 |
| Offline Cargo build | 56.99 | 57.36 | 73.56 | 117.28 |
| Offline npm install | 176.99 | 177.18 | 182.28 | 268.46 |

For FUSE/no-stage, `filesystem_fuse_passthrough.rs` mounts a disposable workspace before running `pvisor run --no-agent-defaults --overlaynet off` inside the mount, without `--stage`. Reads and writes pass through userspace FUSE requests to the backing directory. The adapter does not use OverlayCore, copy-up, access policy or a preimage journal; writes go directly to backing, without apply/drop. This is a control for FUSE data-path costs, rather than an existing production feature.

The adapter and staged share the frozen `fuser 0.15.1`, ABI 7.31, default initialization protocol capabilities, a single synchronous request loop, one-second entry/attribute TTLs and RW/NoAtime/DefaultPermissions mount options. Neither enables writeback cache, keep-cache or kernel backing-FD passthrough. Their filesystem implementations and semantics differ, so this is not a strict experiment changing only a staging switch.

All 30 FUSE samples preserve real mountinfo and LOOKUP/READ/WRITE counters, with more than **64 MiB** read and **16 MiB** written per sample. Run Bundles report `rootless_process` and `filesystem_changes_staged=false`. Execution also checks the names and sizes of all 256 files written to backing, rejecting mount bypass or incomplete writes.

Traversal, Git and search show that FUSE/no-stage retains metadata costs. Staged writes take about **16.5×** this control, so FUSE round trips alone cannot explain the entire gap. Additional costs include differing adapter implementations, copy-up, policy and journal semantics; detailed measurements are needed for attribution. The table compares only tools and verification, excluding mounting, Job startup and unmounting. Raw completion times include those lifecycle steps and cannot be combined directly with historical startup results.

### Against Docker {#reference-fs}

Docker file operations are near native. Staged npm is in the same range, a 64 MiB read adds about **15 ms**, and Cargo takes about **2×** Docker. Traversal, however, takes about **180 ms** versus Docker **5 ms**; writing 256 files takes **188 ms** versus **4 ms**. Repeated repository scans, output generation and Git/rg accumulate these costs.

VM reads take about **2.7×** Docker; npm takes **1.7–2.3 s** versus Docker **0.23 s**. The measured Docker bind mount has the file/tool speed advantage over historical staged and VM. Docker directly modifies mounted host files, while pVisor staged retains changes until apply; consider that workflow difference too. The FUSE/no-stage batch has no matched Docker measurement, so it does not update Docker ratios.

### Against complete Ubuntu VMs {#full-ubuntu}

VM bulk reads and small Cargo builds are faster, so a single multiplier cannot describe tool performance. Traversal takes **0.29–0.31 s** versus Ubuntu **19 ms**; search takes **0.52–0.55 s** versus **20 ms**. In the matched Ubuntu comparison, pVisor VM small-file writes take **134 ms** versus Ubuntu **36 ms**, and npm **2.26 s** versus **1.08 s**.

Dependency installation, Git and repository scans still carry significant VM overhead. For frequently created environments, also consider [startup waiting](startup.md) and [complete tasks](agent-tasks.md). Persistent environments depend more on these internal tool timings.

### Scope {#acceptance}

These are small, offline, warm-cache development workloads. Large repositories, real npm registries, cold disks and concurrent throughput are not covered. Docker writable layers, overlay2, Docker Desktop and matching macOS filesystem workloads lack equivalent results. [Isolation validation](isolation-tests.md) describes actual boundaries.

### Data sources {#run}

[FUSE/no-stage same-batch summary](../../assets/benchmarks/filesystem-fuse-20261005/summary.tsv) · [Per-operation samples](../../assets/benchmarks/filesystem-fuse-20261005/samples.tsv) · [Full report](../../assets/benchmarks/filesystem-fuse-20261005/report.tsv) · [Build provenance](../../assets/benchmarks/filesystem-fuse-20261005/build-provenance.tsv) · [FUSE mounts, request logs and runtime proofs](../../assets/benchmarks/filesystem-fuse-20261005/evidence.tar.gz) · [Verification manifest](../../assets/benchmarks/filesystem-fuse-20261005/manifest.tsv)

[Docker comparison summary](../../assets/benchmarks/reference-env-20261004/summary.tsv) · [Docker comparison samples](../../assets/benchmarks/reference-env-20261004/samples.csv) · [Docker comparison raw report](../../assets/benchmarks/reference-env-20261004/report.tsv) · [Archived commands and run records](../../assets/benchmarks/reference-env-20261004/evidence.tar.gz) · [Ubuntu comparison summary](../../assets/benchmarks/full-ubuntu-20261004/summary.tsv) · [Ubuntu comparison samples](../../assets/benchmarks/full-ubuntu-20261004/samples.csv) · [Methodology and artifacts](methodology.md) · [Detailed measurements and optimization analysis](../design/filesystem-performance-analysis.md)
