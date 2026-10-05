# VM memory, reclamation and snapshot performance

> CLI update: complete snapshot figures come from historical artifacts. The standalone `pvisor snapshot` entry is removed; see [CLI reference](../../reference/cli.md) for current entries and capability boundaries.

## Main conclusions {#conclusions}

**pVisor can reduce cold guest-page residency, with access-recovery time and CPU costs; evidence does not establish lower net physical memory than Docker or other VMs.** On repetitive macOS workloads, the cold-page RAM proxy falls **60–89%**, not equivalent to system-wide physical savings. With 2 GiB configured, first 64 MiB access rises from about **20 ms** to **164 ms**; late footprint increases from about **18 MiB** to **52 MiB**.

Linux raw offload with 2 vCPU / 256 MiB takes about **23 ms**, and first full read after resume **109 ms**. Compression uses less backing allocation but raises that read to **699 ms**. Complete raw snapshot save/restore takes **0.71/0.93 s**, compressed **1.24/1.92 s**. These suit idle environments that tolerate recovery waiting; frequently active tasks require a tradeoff.

## Motivation {#motivation}

VMs waiting for model responses can retain cold pages. Users need to know which residency decreases, backing allocation and waiting on the next tool access or restore. Reclamation percentages alone cannot establish capacity benefits.

## Experiment design {#experiment-design}

On Apple M4 with 24 GiB RAM, macOS/HVF compares the shared cold-page pool enabled/disabled, two VMs per group, each writing 64 MiB of repetitive compressible data: 40 parameter-matrix runs and four extended 2 GiB runs. The RAM proxy includes guest residency and pool/inflight data, not net physical memory. Footprint, CPU and recovered access are also recorded. These macOS figures do not measure the Linux shared pool.

Linux/KVM lifecycle and complete snapshots are independent measurements with fixed 64 MiB guest data and RAM/vCPU configurations. They track pause/resume/offload, heartbeat and first reads. Complete snapshots have ten trials per cell, two forks per trial, with each pair median treated as one sample. Checks cover source exit/deletion, memory digest, counters, open files, directory handles and fork write isolation. Matching Docker/Firecracker/Kata memory workloads are unmeasured.

## Data and analysis {#experiment-data}

### macOS / HVF: cold-page residency and recovery {#measurements}

| RAM / VM | Window after all guests ready | RAM proxy: off → on | Approximate decrease |
|---|---|---|---|
| 256 MiB | 18–33 s | 231 → 27 MiB | 89% |
| 512 MiB | 18–33 s | 243 → 61 MiB | 75% |
| 2 GiB | 60–90 s | 309 → 124 MiB | 60% |

First access with 2 GiB slows about **7.3–8.8×**, adding **15.9–17.2 s** sampled CPU. The compression pool consumes memory too: falling guest residency alongside increasing footprint cannot promise higher Agent density. Incompressible data, repeated access and real repositories need their own measurements.

### Linux / KVM: offload {#linux-lifecycle}

| MiB / vCPU / backing | pause | resume | offload | offload resume | heartbeat |
|---|---:|---:|---:|---:|---:|
| 256 / 1 / raw | 0.24 / 0.30 | 0.26 / 0.34 | 20.43 / 21.76 | 0.28 / 0.37 | 49.57 / 51.92 |
| 256 / 2 / raw | 0.24 / 0.31 | 0.29 / 0.35 | 22.98 / 24.90 | 0.29 / 0.40 | 46.79 / 49.83 |
| 256 / 4 / raw | 0.25 / 0.36 | 0.27 / 0.35 | 22.93 / 25.60 | 0.29 / 0.35 | 43.61 / 46.87 |
| 512 / 2 / raw | 0.24 / 0.31 | 0.28 / 0.35 | 26.44 / 29.35 | 0.28 / 0.39 | 44.63 / 46.74 |
| 2048 / 2 / raw | 0.21 / 0.27 | 0.23 / 0.31 | 23.86 / 28.80 | 0.26 / 0.32 | 40.39 / 46.71 |
| 256 / 2 / compressed | 0.21 / 0.33 | 0.25 / 0.40 | 66.93 / 505.43 | 0.24 / 0.31 | 175.09 / 203.24 |

| MiB / vCPU / backing | 64 MiB read before offload (ms) | First complete read after offload (ms) | Backing allocation (MiB, P50) |
|---|---:|---:|---:|
| 256 / 1 / raw | 32.12 / 32.31 | 109.02 / 111.56 | 200.80 |
| 256 / 2 / raw | 32.12 / 32.37 | 109.49 / 110.84 | 202.23 |
| 256 / 4 / raw | 32.10 / 32.93 | 114.85 / 117.64 | 203.99 |
| 512 / 2 / raw | 32.15 / 32.48 | 115.15 / 117.42 | 206.30 |
| 2048 / 2 / raw | 25.03 / 31.91 | 86.12 / 109.30 | 235.91 |
| 256 / 2 / compressed | 25.06 / 27.64 | 698.71 / 754.50 | 23.96 |

Times are P50/P95 ms. Offload pauses, writes back and requests RAM reclamation; resume is still required. Control-call return differs from guest heartbeat progress. The 2 GiB configuration still writes the same 64 MiB payload, not 2 GiB writeback throughput. Compressed backing is smaller, with longer first access and tails.

### Linux / KVM: complete snapshots and forks {#linux-snapshot}

| RAM storage | save (ms, P50 / P95) | restore heartbeat (ms, P50 / P95) | Published allocation (MiB, P50 / P95) |
|---|---:|---:|---:|
| raw | 712.11 / 819.75 | 932.88 / 1034.90 | 296.99 / 296.99 |
| compressed | 1236.62 / 1343.63 | 1920.23 / 1947.43 | 16.48 / 16.58 |

Save includes freezing, CPU/device/RAM capture, file copying, verification, durable publication and source exit. Restore ends at new guest heartbeat, including decoding and private copies. Published allocation measures published files' disk allocation, excluding active forks, temporary artifacts and process memory. High compression here comes from repetitive data, not arbitrary Agent working sets.

### Data sources and reproduction {#evidence}

[macOS JSON](assets/decision.json) · [macOS CSV](assets/decision.csv) · [Compatibility](assets/compatibility.json) · [Linux lifecycle](../../../assets/benchmarks/vm-lifecycle-20261003/lifecycle.tsv) · [Linux snapshots](../../../assets/benchmarks/vm-lifecycle-20261003/snapshot.tsv) · [Technical analysis and reproduction](../../design/vm-memory-performance-analysis.md)
