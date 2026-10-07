# Retired Cluster performance evidence

**Controller/Worker measurements describe retired artifacts, not the current daemon.** B-CLUSTER has no active measurement, plotting or publication entry. The [historical benchmark](../benchmarks/cluster-scalability.md) preserves completed-task and history tables with their original CSV provenance. No new measurements or daemon-density conclusions are added.

## Historical VM readiness probes {#execution}

The 2026-10-05 shell/sleep experiment increased both Worker count and CPU budget, using one independent Worker per VM. It observed parallel readiness for one to four guests, not fixed-budget useful-task throughput or single-Worker density.

![Historical retired-Worker readiness probe](../assets/benchmarks/cluster-scalability-20261005/execution.svg)

| Live VMs | Total memory P50, MiB | Readiness P50, s | Burst readiness rate, guests/s |
|---:|---:|---:|---:|
| 1 | 92.93 | 3.807 | 0.261 |
| 2 | 178.00 | 3.789 | 0.525 |
| 4 | 342.96 | 4.383 | 0.901 |

Five measured batches per size followed one warmup. Guests used 128 MiB/one vCPU; each Worker and descendants had 512 MiB/0.5-core caps, and Controller 256 MiB/0.25 core, with zero swap. The shared Linux/KVM host used prepared minimal inputs and debug artifacts; other host workloads continued. Readiness includes submit CLI/HTTP, persistence, scheduling and VM startup. Memory is the sum of non-overlapping service cgroups while all guests are ready, not startup peak. Charts show observed ranges, not confidence intervals. These observations do not establish production capacity, tail latency, shared-working-set benefits or current daemon performance.

## Historical Controller costs {#controller}

| Retained records | Indexed counts P50, ns | Full-scan reference P50, ms | Process + ID fixture RSS, MiB | Journal, MiB | Warm replay, s |
|---:|---:|---:|---:|---:|---:|
| 1,000 | 202.83 | 0.068 | 12.13 | 1.93 | 0.079 |
| 10,000 | 286.78 | 5.254 | 70.59 | 19.33 | 0.388 |
| 100,000 | 129.40 | 20.789 | 658.82 | 193.25 | 1.744 |
| 1,000,000 | 169.47 | 134.204 | 6,540.58 | 1,932.55 | 16.090 |

The archived `controller-indexes-20261005-v3-history-*` results used one release process per size, one CPU, NVMe and warm replay. There was one ready record and otherwise cancelled history, without guest execution or HTTP load. Counts used twenty query samples, with one hundred indexed calls per sample; RSS and replay had one observation per size. RSS includes the ID fixture and allocator retention. Warm replay excludes Worker reconciliation and cold-disk recovery. The scan is an algorithm reference, not another Controller binary's throughput. Do not merge this cohort with the independent later history cohort in the historical benchmark.

## Evidence boundaries {#limits}

The readiness and query observations remain historical descriptions of their frozen implementations. They provide no current optimization priority, daemon sizing recommendation or scheduler/runtime ranking. Local [capacity](../benchmarks/density.md) and [VM memory](../benchmarks/vm-memory/index.md) have independent subjects and retained evidence; single-VM memory reclamation does not establish concurrent density. The [retired plan](cluster-benchmark-plan.md) records the boundary between old proposals and measured claims.

## Retention, not active reproduction {#reproduce}

`cluster_scalability.py`, `cluster_worker.py`, `controller_history.py`, their dedicated tests, `plot_cluster_scalability.py` and `publish_controller_history.py` are removed. There is no current build/run/publish command for the retired scheduler example. Original logs, samples, manifests and frozen harnesses remain local under `.data/`; retained copies are archives, not active entry points. Removal of old crate measurements does not authorize rewriting receipts or assigning those measurements to daemon source.

Historical chart inputs are recorded under `docs/src/assets/benchmarks/.data/cluster-scalability-20261005/`: `vm.tsv`, `vm-summary.csv`, `controller-summary.csv`, `controller-provenance.tsv`, `manifest.tsv` and `setup-failure.tsv`. These are local provenance locations, not public download links. The [runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md) documents retirement and the independent active native probes.
