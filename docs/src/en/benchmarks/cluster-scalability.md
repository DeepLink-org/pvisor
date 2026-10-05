# Do more Workers deliver more valid task results?

## Main conclusions {#conclusions}

**Valid-task throughput at a fixed total budget remains unmeasured, with no matching Kubernetes or Ray ranking.** Lightweight VM readiness and retained-history costs provide initial resource context.

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

2026-10-05, five batches per size; medians. Units are in the headers. Readiness is not a completed tool task.

| Simultaneously live VMs | Total memory P50 | Per-VM readiness P50 | Burst launch rate |
|---|---:|---:|---:|
| 1 | 92.93 MiB | 3.807 s | 0.261 guests/s |
| 2 | 178.00 MiB | 3.789 s | 0.525 guests/s |
| 4 | 342.96 MiB | 4.383 s | 0.901 guests/s |


Total resources grow with Worker count. These readiness results do not establish throughput scaling at a fixed budget; matching task-completion comparisons with Kubernetes and Ray remain unmeasured.

### Retained history: queries, memory and recovery {#controller}

| Retained task records | Indexed counts P50 | Process + ID fixture RSS | Intent/receipt journal | Warm replay |
|---|---:|---:|---:|---:|
| 1,000 | 202.83 ns | 12.13 MiB | 1.93 MiB | 0.079 s |
| 10,000 | 286.78 ns | 70.59 MiB | 19.33 MiB | 0.388 s |
| 100,000 | 129.40 ns | 658.82 MiB | 193.25 MiB | 1.744 s |
| 1,000,000 | 169.47 ns | 6,540.58 MiB | 1,932.55 MiB | 16.090 s |


RSS includes the ID fixture and retained allocator memory. Replay restores control records, excluding cross-host Worker reconciliation. Larger deployments need bounded retention and complete recovery measurements.

### Comparison with existing schedulers {#limits}

 · [Protocol and reproduction](../design/cluster-performance-analysis.md)

Kubernetes and Ray are references for actual task scheduling, but matching tasks at a fixed total budget and shared success criteria remain unmeasured. Valid-result scaling still needs measurement; readiness and history costs cannot substitute for it.

### Downloads and reproduction {#run}

[Derived table CSV](cluster-scalability.csv) · [Evidence source summary](evidence-sources.csv) · [Comparison method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
