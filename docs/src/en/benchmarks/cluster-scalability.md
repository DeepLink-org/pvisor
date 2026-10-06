# Which conclusions survive retirement of the Cluster measurements?

## Main conclusions {#conclusions}

**The old throughput and history measurements remain evidence only for their frozen, retired Controller/Worker implementation. They do not establish current daemon throughput, density or recovery costs. Independent local capacity and VM-memory benchmarks retain their own evidence scope; no new measurement is reported here.**

| Need | Selection implication |
|---|---|
| Interpret old Cluster results | Use only the historical workload, artifacts and budgets below |
| Size local execution or parked states | Use [capacity](density.md) and [VM memory](vm-memory/index.md), not Worker curves |
| Size the daemon or compare schedulers | Unmeasured; do not transfer old throughput or history figures |

## Motivation {#motivation}

Capacity decisions require evidence for the implementation actually deployed. Retiring Controller/Worker removes the subject of these experiments; renaming its measurements would conceal that boundary. Readiness, validated work and recoverable parked states answer different questions.

## Experiment design {#interpretation}

Historical experiment only: Linux/x86_64, single-host KVM, release binaries built from frozen Controller/Worker source. The runners and their dedicated tests, plotting and publication helpers are retired; there is no active reproduction command. Each batch has a fresh Controller and 1/2/4 one-slot Workers. The complete group shares a two-core quota, CPU 0/1 affinity, 2 GiB memory and zero swap. The Controller has a separate 0.25-core/256 MiB cap and each Worker a 0.5-core/512 MiB cap; services and descendants remain in the common parent cgroup. One Worker therefore cannot consume the whole two-core budget.

Each batch submits the same 12 tasks. Each task creates a 64-file Git repository, verifies all 32 MiB of data eight times, edits four files and checks Git status and all contents. Successful output must belong to the expected Worker and match the workload; each retained completion record is independently checked against the summary. Time runs from submission through verification of all 12 results, excluding earlier Controller/Worker startup.

Three warmups and 30 formal batches per condition; Worker counts are randomized within each round. All slow valid samples remain. OOM, missing results, incorrect output or mismatched controls invalidate throughput samples. Common tools/rootfs are prepared and warm; some shared caches may be charged outside the group. This excludes inference, multi-host networking and cross-host reconciliation, and does not establish real-Agent task rates.

## Data and analysis {#results}

### Completed-task throughput and physical memory {#execution}

Historical Controller/Worker cohort, measured on 2026-10-06 local time. The following tables and downloads describe retired artifacts, not the daemon. N=30 complete batches per condition, 12 tasks per batch. The three conditions verify 1,080 formal tasks, with zero failures or OOM. P50 is the median. Peak memory is the complete parent cgroup's `memory.peak`, including descendants, in MiB.

| Workers | Completed / attempted tasks | 12-task P50, s | Validated rate P50, tasks/s | Whole-group peak P50, MiB |
|---:|---:|---:|---:|---:|
| 1 | 360/360 | 28.28 | 0.424 | 221.3 |
| 2 | 360/360 | 12.47 | 0.962 | 389.0 |
| 4 | 360/360 | 6.93 | 1.731 | 710.6 |

A single Worker is capped at 0.5 cores. Additional execution slots increase available execution resources until the shared two-core limit constrains the group. This explains reduced waiting for the retired implementation within this budget; it does not show a faster scheduler at identical effective CPU utilization.

| Comparison (candidate − one Worker) | Median 12-task difference, s | Paired bootstrap 95% interval, s |
|---|---:|---:|
| Two Workers | −15.81 | [−15.93, −15.03] |
| Four Workers | −21.35 | [−21.36, −20.97] |

Intervals use 30 paired rounds and 5,000 resamples; both exclude zero. Downloaded P95 values are descriptive only, with no P99 or production-tail guarantee.

### Long-term retained history {#controller}

Each size uses three fresh independent processes/cgroups, two cores, 16 GiB and zero swap; WAL storage is NVMe. There is one ready probe and otherwise cancelled records. A synthetic failed terminal receipt verifies fencing and replay without executing tools. Physical peak includes history creation, record-validation temporaries, WAL cache and warm restart; it is not stationary retained-state memory. Each process measures 30 query batches, but restart and memory have only three independent observations, not 90 pooled samples.

N=3 independent processes per size; table values are P50, with observed minimum–maximum in parentheses, not confidence intervals.

| Retained records | Whole-lifecycle peak, MiB | Warm-log replay, s | WAL, MiB |
|---:|---:|---:|---:|
| 1,000 | 24.9 (22.8–31.4) | 0.017 (0.017–0.018) | 1.96 |
| 10,000 | 96.3 (96.2–96.7) | 0.143 (0.143–0.145) | 19.62 |
| 100,000 | 827.9 (827.9–828.0) | 1.422 (1.410–1.435) | 196.21 |
| 1,000,000 | 8142.4 (8127.4–8144.1) | 14.622 (14.411–14.710) | 1962.11 |

The retired Controller's million-record cohort observed about 8 GiB lifecycle peak and about 14.62 s warm-log replay; neither is a current sizing recommendation. Recovery excludes Worker reconciliation and cold-disk reads. The medians across processes for typed in-memory count calls are about 107–110 ns, describing synthetic state-count calls rather than HTTP/CLI latency; see the download for details.

### Comparison with existing schedulers {#limits}

Matched workloads and common budgets for Kubernetes, Ray and cloud sandboxes are unmeasured, so no numeric ranking is provided. The archived measurements do not size the current daemon or justify replacing a general scheduler. Single-task Docker, Firecracker and QEMU measurements are in the [runtime comparison](compare-runtimes.md).

### Historical downloads and provenance {#run}

All five CSVs below are unchanged historical evidence for retired Controller/Worker artifacts. There is no active Cluster measurement or publishing entry; the [runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md) records retirement, not a new reproduction command.

[History costs CSV](controller-history-summary.csv) · [History provenance](controller-history-provenance.csv) · [Throughput and memory CSV](cluster-summary.csv) · [Paired comparisons CSV](cluster-comparisons.csv) · [Artifacts and budget summary](cluster-provenance.csv) · [Comparison method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)

Derived tables link one raw report to the per-task evidence audit digest. Raw reports, completion records, service logs, controls, source and binaries stay in local `.data/`. Implementation analysis is in the [Cluster technical analysis](../design/cluster-performance-analysis.md).
