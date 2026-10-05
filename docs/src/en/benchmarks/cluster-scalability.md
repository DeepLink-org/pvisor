# Do more Workers deliver more valid task results?

## Main conclusions {#conclusions}

**With more Workers and CPU budget, 1–4 lightweight VMs become ready in parallel, with approximately proportional memory growth.** Four VMs have readiness P50 **4.38 s** and combined service cgroup memory about **343 MiB**. This does not establish fixed-budget Agent throughput or a ranking against other cluster tools.

Control-plane counting costs about **0.13–0.29 µs**, while retained history remains expensive: a million records use about **6.39 GiB** process/fixture RSS and **16 s** hot-log replay. Fast queries do not imply unlimited historical-state scaling.

| Need | Selection implication |
|---|---|
| Estimate basic environment occupancy | Use lightweight VM readiness data |
| Plan valid-task throughput | Still needs real task-completion comparisons |
| Retain substantial history | Budget memory and restart replay |

## Motivation {#motivation}

Parallel tasks involve execution environments, scheduling waiting, active resources and retained history. Users need to know whether added resources yield more useful results and what long-lived state costs at restart.

## Experiment design {#interpretation}

On a shared Linux host, 1/2/4 Workers each run one 1 vCPU / 128 MiB minimal-shell VM, marking ready then waiting six seconds. Each size has one warmup and five measured batches. Each Worker cgroup is capped at 512 MiB / 0.5 core; Controller at 256 MiB / 0.25 core. Total CPU grows with Worker count. Fixed debug binaries do not measure a release performance ceiling.

Readiness runs from submit CLI to guest marker, including API, durable records, scheduling and boot. Memory sums nonoverlapping service cgroups after all guests are ready; file memory can include guest RAM. Batch rate is N / total readiness waiting, not completed-Agent throughput. Separate release Controller microbenchmarks have one ready task and canceled history; counting N=20, RSS/replay one observation per size, excluding full Worker reconciliation.

Tables identify pinned artifacts and measurement dates. Failed or invalid samples are excluded from successful timings and counted separately. Existing measurements have no predefined host-interference filter; all slow valid samples are retained. P95 from 30 or fewer samples is descriptive only; no P99 or stable tail-latency claim is made.

## Data and analysis {#results}

### Lightweight VM readiness and memory {#execution}

| Simultaneously live VMs | Total memory P50 | Per-VM readiness P50 | Observed P95 | Burst launch rate |
|---|---:|---:|---:|---:|
| 1 | 92.93 MiB | 3.807 s | 3.893 s | 0.261 guests/s |
| 2 | 178.00 MiB | 3.789 s | 3.993 s | 0.525 guests/s |
| 4 | 342.96 MiB | 4.383 s | 4.686 s | 0.901 guests/s |

![VM scaling](../../assets/benchmarks/cluster-scalability-20261005/execution.svg)

From one to four VMs, memory is 3.69×, readiness P50 grows 15.1%, and batch rate is 3.45×. These apply only to lightweight probes with increasing resources, not fixed-host capacity, shared-RAM savings or real build/model throughput. Observed ranges are not confidence intervals.

### Retained history: queries, memory and recovery {#controller}

| Retained task records | Indexed counts P50 | Full-scan algorithm reference P50 | Process + ID fixture RSS | Intent/receipt journal | Warm replay |
|---|---:|---:|---:|---:|---:|
| 1,000 | 202.83 ns | 0.068 ms | 12.13 MiB | 1.93 MiB | 0.079 s |
| 10,000 | 286.78 ns | 5.254 ms | 70.59 MiB | 19.33 MiB | 0.388 s |
| 100,000 | 129.40 ns | 20.789 ms | 658.82 MiB | 193.25 MiB | 1.744 s |
| 1,000,000 | 169.47 ns | 134.204 ms | 6,540.58 MiB | 1,932.55 MiB | 16.090 s |

![Controller history costs](../../assets/benchmarks/cluster-scalability-20261005/controller.svg)

The full scan is an algorithm reference over identical data, not old Controller throughput. RSS includes ID fixtures and allocator retention; replay rebuilds control records without cross-host Worker reconciliation. Larger deployments also need bounded history and complete-recovery measurements.

### Scope and sources {#limits}

 ·  ·  ·  · [Protocol and reproduction](../design/cluster-performance-analysis.md)
