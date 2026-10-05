# What do concurrent environments cost compared with Podman?

## Main conclusions {#conclusions}

**In idle probes, all 128 staged jobs complete with about 1.56 GiB combined RSS; all 32 minimal-shell VM jobs complete with about 3.02 GiB. Podman also completes 128 jobs under the corresponding conditions, with longer launch delays. Some high-concurrency safe and full-tool OCI cases fail. These describe idle occupancy and reliability, not real-Agent throughput or a capacity guarantee.**

| Need | Selection implication |
|---|---|
| Many idle environments | Plan with success rates and resident resources |
| Complete OCI tool environment | Check temporary-storage quota and failures |
| Real parallel Agents | Idle probes do not guarantee task capacity |

## Motivation {#motivation}

With multiple agents, memory, startup and environment preparation accumulate. Publish completion rates with resources rather than only successful fast samples.

## Experiment design {#interpretation}

Each Job prints ready then holds for one second. Concurrency 1/8/128, five batches per cell, no warmups. Sample owned process-tree peak RSS every 20 ms and collect child CPU and per-Job wall time. VM uses 2 vCPU/128 MiB. This is an occupancy probe, not active agent/build throughput. Only wholly successful batches enter timing/resource summaries; completion denominators include failed batches.

These results are from Linux/x86_64; matching macOS workloads are unmeasured. Linked reports pin artifacts, cache conditions and samples.

Tables identify pinned artifacts and measurement dates. Failed or invalid samples are excluded from successful timings and counted separately. Existing measurements have no predefined host-interference filter; all slow valid samples are retained. P95 from 30 or fewer samples is descriptive only; no P99 or stable tail-latency claim is made.

## Data and analysis {#results}

Measured on 2026-10-04; configurations retain separate samples. P50 is the median.

| Environment | Backend | Concurrency | Completed/attempted | Full batches | RSS P50 MiB | CPU P50 ms/job | Job P50/P95 ms |
|---|---|---|---|---|---|---|---|
| Host workspace | native | 128 | 640/640 | 5 | 266.4 | 0.68 | 1002.2/1003.2 |
| Host workspace | staged | 128 | 640/640 | 5 | 1597.2 | 13.96 | 1204.6/1248.6 |
| Tool rootfs | safe | 128 | 638/640 | 3 | 3245.0 | 39.97 | 1446.3/1490.5 |
| Tool rootfs | vm | 8 | 40/40 | 5 | 786.5 | 274.22 | 1281.2/1323.2 |
| Tool rootfs | vm | 32 | guard | 0 | — | — | — |
| Tool rootfs | vm | 128 | guard | 0 | — | — | — |
| Tool rootfs | podman | 8 | 40/40 | 5 | 391.9 | 36.69 | 1114.2/1181.3 |
| Tool rootfs | podman | 32 | 160/160 | 5 | 1570.6 | 45.42 | 1407.4/1679.2 |
| Tool rootfs | podman | 128 | 640/640 | 5 | 6011.3 | 51.42 | 6469.9/8542.5 |
| Tool rootfs | pVisor OCI | 8 | 40/40 | 5 | 210.1 | 493.64 | 1521.9/2512.8 |
| Tool rootfs | pVisor OCI | 32 | 45/160 | 0 | — | — | — |
| Tool rootfs | pVisor OCI | 128 | 55/640 | 0 | — | — | — |
| Minimal shell | vm | 8 | 40/40 | 5 | 789.8 | 283.13 | 1292.1/1306.1 |
| Minimal shell | vm | 32 | 160/160 | 5 | 3093.4 | 370.44 | 1939.0/2115.2 |
| Minimal shell | vm | 128 | guard | 0 | — | — | — |
| Minimal shell | podman | 8 | 40/40 | 5 | 389.2 | 38.50 | 1132.4/1189.4 |
| Minimal shell | podman | 32 | 160/160 | 5 | 1559.6 | 45.99 | 1497.0/1969.4 |
| Minimal shell | podman | 128 | 640/640 | 5 | 5929.0 | 50.74 | 6473.4/8242.6 |
| Minimal shell | pVisor OCI | 8 | 40/40 | 5 | 208.1 | 20.36 | 1048.4/1048.9 |
| Minimal shell | pVisor OCI | 32 | 160/160 | 5 | 834.2 | 22.05 | 1060.1/1066.9 |
| Minimal shell | pVisor OCI | 128 | 640/640 | 5 | 3338.9 | 28.43 | 1300.2/1478.3 |

### Analysis

Main safe concurrency 128 completed **638/640** jobs. Failures reported `Address already in use`, a race between free-port probing and actual listening. Successful samples do not establish stable concurrency 128. Idle VM tree RSS is roughly 100 MiB at one and 789 MiB at eight, not configured RAM or a maximum active working set.

The tools rootfs is about 749 MiB. pVisor OCI copies a private environment per Job into default `/tmp`; concurrency 32/128 hit the tmpfs user quota (`Disk quota exceeded`). Failures remain visible. A minimal-shell follow-up is separate, distinguishing runtime from tool-environment preparation. Podman uses a prebuilt shared image rather than the same full per-Job copy.
### Idle occupancy and complete Agent capacity differ {#baseline-meaning}

Native shell and Podman/crun provide familiar occupancy baselines. Success at concurrency 128 answers whether these idle processes can be maintained together. It does not answer whether 128 tasks using Python, Node, Rust, and Agent CLIs can run together. A 2 vCPU/128 MiB idle VM does not represent the active working set of a complete tool environment.

Single-task latency for the complete environment is in the [Agent environment comparison](agent-tasks.md#reference-env). These Podman RSS scopes may omit background processes and do not establish a total physical-memory ranking. Plan concurrency from your task working set, then measure success and completion time. No complete-Agent capacity claim at concurrency 128 is published.

### Scope {#acceptance}

Idle occupancy does not establish active tool capacity; process-tree RSS is not total physical memory.

### Downloads and reproduction {#run}

[Derived table CSV](density.csv) · [Evidence source summary](evidence-sources.csv) · [Comparison method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
