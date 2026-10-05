# Cluster performance: technical evidence

Exploratory measurements on 2026-10-05 observed that **adding Workers and CPU budget permits parallel readiness for one to four lightweight VMs with roughly linear memory growth; Controller counts queries avoid scanning retained history**. They do not validate fixed-host-budget useful Agent throughput, density or whole-system scalability.

The next round must freeze questions, hypotheses, controls and judgment criteria before sampling; see [questions benchmarks should answer first](cluster-benchmark-plan.md). That protocol was written after these measurements and does not retrospectively preregister them. Growing historical memory and replay costs identify problems to investigate, rather than verified gains from any optimization.

These tasks specify neither immutable environment handles nor checkpoint restores, and each VM uses an independent Worker, bypassing key reuse paths in [shared working sets and lazy loading](cluster/shared-working-set.md). Prioritize S1 shared RAM and S2 large environments with small working sets next, rather than treating these plots as evidence for those mechanisms.

New real-VM experiments and archived Controller history measurements are plotted separately. Their artifacts, workloads and statistical definitions differ; they are not combined into one curve or ranked against other sandboxes measured under different conditions.

## Real VMs: memory, readiness and launch rate {#execution}

![Real-VM memory, readiness latency and burst launch-rate curves](../../assets/benchmarks/cluster-scalability-20261005/execution.svg)

| Simultaneously live VMs | Total memory P50 | Per-VM readiness P50 | Observed P95 | Burst launch rate |
|---|---:|---:|---:|---:|
| 1 | 92.93 MiB | 3.807 s | 3.893 s | 0.261 guests/s |
| 2 | 178.00 MiB | 3.789 s | 3.993 s | 0.525 guests/s |
| 4 | 342.96 MiB | 4.383 s | 4.686 s | 0.901 guests/s |

From one to four VMs, memory is **3.69×**, readiness P50 increases **15.1%**, and burst launch rate is **3.45×**, or **86.2%** of an ideal linear rate derived from the single-VM baseline. In this workload and range, memory grows roughly in proportion to VM count without obvious superlinear amplification; readiness latency does not grow in proportion to VM count.

This experiment scales **Worker count and total CPU budget**: each VM has a dedicated Worker, and CPU budget increases with VM count. It observes parallel readiness for these probes. Without fixed budgets, complete useful tasks and corresponding controls, 86.2% cannot be called pVisor's overall scaling efficiency. It also does not test high density inside one Worker or scaling across hosts.

### Measurement protocol

- One shared Linux host: AMD Ryzen 7 9700X, approximately 30.5 GiB RAM, Linux 7.2.8, KVM/FUSE. Other host workloads were not stopped.
- Sizes 1, 2 and 4 run sequentially, with one warmup and five measured batches each. There are 35 measured guests plus seven warmup guests: **42/42 succeeded**. A batch finishes before the next starts; at most four guests run concurrently.
- Each VM has 128 MiB guest RAM / one vCPU. Each Worker and all its descendants have kernel cgroup hard caps of 512 MiB / 0.5 core; Controller has 256 MiB / 0.25 core; swap is zero. Task CPU time is limited to 2,000 ms and timeout to 60 s. The maximum sum of caps is 2.25 GiB / 2.25 cores. At least 3 GiB available memory is required before starting.
- A prepared minimal rootfs contains only shell, sleep and their libraries. The guest writes a readiness marker, sleeps six seconds, then outputs `scale-ok`. There are no model calls, network traffic or real Agent toolchains.
- The quickstart's verified `target/debug` artifacts are copied and pinned before starting, with identical binary SHA-256 across sizes; firmware is libkrunfw 5.5.0. This is not an optimized release performance ceiling. The working tree is under development: its revision is not a frozen release identity; binary hashes identify the artifacts actually measured.
- **Readiness latency** starts when each task's submit CLI begins and ends when the guest creates its own marker. It includes CLI/HTTP, intent persistence, scheduling and VM startup, excluding service/rootfs preparation. Worker polling is 200 ms; marker observation is 20 ms. It cannot be directly compared with the approximately 110 ms [VM-only startup](../benchmarks/startup.md) measurement.
- **Total memory** sums disjoint Controller and Worker cgroup `memory.current` values while all guests are ready and live. Each batch uses the median of ten samples 50 ms apart; the reported value is the median of five batches. This is neither an instantaneous startup peak nor configured guest RAM.
- The `file` category includes page cache and shmem/memfd/tmpfs memory; guest RAM may be charged there. **It cannot be deducted as freely reclaimable cache**. Anonymous memory, file-category memory and native process PSS are recorded separately; shared process RSS is not summed into the cgroup total.
- Real KVM descriptors, native PID/start time and guest markers establish N live VMs together. Final status, stdout, Worker placement and all cgroup OOM counters passed checks. Native process PSS is also recorded without treating it as full-service memory.

Shading shows observed ranges, not confidence intervals: memory/rate use minimum and maximum across five batches; readiness uses all guest samples. P95 uses linear interpolation on only five, ten and twenty guest samples respectively, and does not establish a production tail-latency SLO. Host caches were not flushed and sizes ran in order, leaving possible cache and host-load confounding.

**Burst launch rate = N / seconds from the first submit beginning until every guest is ready**. It excludes the subsequent fixed six-second hold and is not Agent completion throughput. CLI submissions are sequential, and their spacing counts toward burst readiness time.

## Controller: query, memory and restart costs {#controller}

![Controller history curves for queries, memory and recovery](../../assets/benchmarks/cluster-scalability-20261005/controller.svg)

| Retained task records | Indexed counts P50 | Full-scan algorithm reference P50 | Process + ID fixture RSS | Intent/receipt journal | Warm replay |
|---|---:|---:|---:|---:|---:|
| 1,000 | 202.83 ns | 0.068 ms | 12.13 MiB | 1.93 MiB | 0.079 s |
| 10,000 | 286.78 ns | 5.254 ms | 70.59 MiB | 19.33 MiB | 0.388 s |
| 100,000 | 129.40 ns | 20.789 ms | 658.82 MiB | 193.25 MiB | 1.744 s |
| 1,000,000 | 169.47 ns | 134.204 ms | 6,540.58 MiB | 1,932.55 MiB | 16.090 s |

These plots reuse the repository's `controller-indexes-20261005-v3-history-*` archive. The million-record experiment **was not rerun**. Each size uses a separate process, a release artifact, one pinned CPU, local NVMe journal storage and warm-cache replay. There is one ready task per size; the rest are cancelled history. No guests execute and there is no HTTP load test, so sandbox concurrency does not increase.

Indexed queries and full scans use the same authoritative TaskRecords. The scan is a former-algorithm reference, **not throughput from a former Controller binary**. Each size has twenty query samples, with one hundred indexed calls per sample. Reading maintained counters is effectively decoupled from history size in this measurement. Complete assignment polls were observed only once per size, ranging from 0.811 to 6.235 ms; all assigned in one poll. These observations cannot establish production QPS.

Memory includes Controller, ID fixture and allocator retention, rather than net Controller object size. RSS and replay are single observations at each size, without invented error bars. Replay restores control records from the intent/receipt journal; it **does not include cross-host Worker reconciliation to full scheduling readiness**. This does not require strong persistence of live Worker state.

**The remaining bottleneck is clear**: a million retained records still consume about 6.39 GiB RSS and 1.89 GiB journal storage, with 16.09 s warm replay. Query indexing removes hot-path scans without solving growing history-retention and cold-recovery costs.

## Supported conclusions and next work {#limits}

The curves describe parallel readiness and memory growth for small probes with added resources, plus the local counts-index mechanism. They do not measure many guests inside one Worker, fixed-budget real Agent/Gateway throughput, cold image fetches, large active-task sets, scaling during failures or cross-host networking/storage. The earlier approximately linear single-host scaling wording must be limited to these probes and growing resource budgets, without promoting it to a whole-product conclusion.

Prioritize compacting/archiving terminal records, separating historical and active state, and bounded retention; then measure cold recovery and full Worker reconciliation separately. For execution, keep concurrency at four or below while adding single-Worker, fixed-total-CPU and real Agent controls to identify resource and scheduling bottlenecks. Larger concurrency requires an isolated test host and newly authorized resource budgets. These three points cannot be extrapolated into capacity commitments.

## Reproduction and raw evidence {#reproduce}

Prerequisites and bounded builds are covered in the [Cluster quickstart](../guides/cluster/index.md). With `pvisor-cluster` / `pvisor-worker` binaries, firmware, Linux user systemd and KVM/FUSE prepared, run from the repository root:

```bash
SCALING_PARENT=$(mktemp -d /tmp/pvisor-cluster-scaling.XXXXXX)
python3 benchmark/pvisor/cluster_scalability.py \
  --state "$SCALING_PARENT/state" \
  --output "$SCALING_PARENT/vm.json" \
  --firmware-dir target/libkrunfw/5.5.0-x86_64-unknown-linux-musl \
  --sizes 1 2 4 --repetitions 5
```

The runner accepts only sizes 1, 2 and 4, verifies kernel caps and available memory at startup, and stops its own services on completion, exceptions and termination signals. Private state remains in the output directory for inspection and is excluded from the public archive. An initial preparation used invalid `trace=false` retention and was rejected before any task was accepted or VM started. After correcting it, every real probe succeeded; the failure report is retained too.

Render the public archive again (requires matplotlib):

```bash
MPLCONFIGDIR=/tmp/pvisor-plot-cache python3 benchmark/pvisor/plot_cluster_scalability.py
```

Local raw record `docs/src/assets/benchmarks/.data/cluster-scalability-20261005/vm.tsv` · Local raw record `docs/src/assets/benchmarks/.data/cluster-scalability-20261005/vm-summary.csv` · Local raw record `docs/src/assets/benchmarks/.data/cluster-scalability-20261005/controller-summary.csv` · Local raw record `docs/src/assets/benchmarks/.data/cluster-scalability-20261005/controller-provenance.tsv` · Local raw record `docs/src/assets/benchmarks/.data/cluster-scalability-20261005/manifest.tsv` · Local raw record `docs/src/assets/benchmarks/.data/cluster-scalability-20261005/setup-failure.tsv`
