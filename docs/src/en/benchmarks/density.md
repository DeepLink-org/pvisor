# How many tool tasks fit in two cores and 2 GiB?

## Main conclusions {#conclusions}

**Under a shared two-core, 2 GiB, zero-swap budget, stage and Podman complete all five rounds at 32 concurrent Python/Git tasks. pVisor VM completes all five at 16, but only three at 32. Idle stage probes reach 128 and Podman reaches 64; idle occupancy does not establish active-task capacity.**

| Need | Selection implication |
|---|---|
| Parallel edits and Git checks | Stage and Podman reach the same tested task capacity; choose by isolation and review needs |
| Many idle environments | Stage reaches higher tested concurrency, using a host-process boundary |
| A VM boundary per task | Reserve more resources; do not size active tasks from idle counts |

## Motivation {#motivation}

When multiple Agents execute together, completed work matters more than created environments. Capacity planning must include runtimes, helpers, page cache and VM backing; neither single-process RSS nor configured RAM can establish capacity.

## Experiment design {#interpretation}

Compare native processes, pVisor stage, pVisor VM and Podman 5.8.7 on Linux/x86_64. Each batch has a fresh cgroup with a shared two-core quota, CPU 0/1 affinity, a 2 GiB memory limit and zero swap. The coordinator, payloads and helpers are inside the budget; Podman payload/conmon membership is checked. Each VM has 2 vCPU/256 MiB, still constrained by the common total budget.

Scan concurrency 1, 2, 4, 8, 16, 32, 64 and 128, with five fresh batches per cell and no warmups. Conditions are randomized within each round. All tasks wait at a stdin readiness barrier before release. Idle tasks use the same Python environment without private data or edits. Useful tasks touch and checksum all 32 MiB of private data, edit four files in a 64-file Git repository, run `git status` and verify all contents. Stage/VM preserve the original workspace and match results to independent Run records.

The primary memory metric is whole-cgroup physical accounting, including charged page cache, kernel memory and private backing. Prepared shared tool/image caches may be charged to a parent cgroup, so this does not rank net whole-machine memory. Failed, unknown and OOM outcomes remain counted; only entirely verified, no-OOM batches enter memory/timing summaries. All slow valid samples remain. Five rounds do not establish long-term reliability or tail latency. This workload excludes inference, large builds and real Agent CLIs.

## Data and analysis {#results}

Measured on 2026-10-06 local time. There are 320 batches: 263 recorded as fully passed and 57 failed; all contribute to capacity statistics. Memory is readiness-barrier `memory.current` P50 in MiB, from complete no-OOM batches only. “—” denotes no such batch.

### Python/Git tool tasks

| Mode | Concurrency | Fully valid batches | Verified tasks / attempted | Unknown results | OOM batches | Barrier memory P50, MiB |
|---|---:|---:|---:|---:|---:|---:|
| Native process | 16 | 5/5 | 80/80 | 0 | 0 | 640.4 |
| Native process | 32 | 5/5 | 160/160 | 0 | 0 | 1268.3 |
| Native process | 64 | 0/5 | 233/320 | 0 | 5 | — |
| Native process | 128 | 0/5 | 244/640 | 0 | 5 | — |
| pVisor stage | 16 | 5/5 | 80/80 | 0 | 0 | 723.0 |
| pVisor stage | 32 | 5/5 | 160/160 | 0 | 0 | 1434.0 |
| pVisor stage | 64 | 0/5 | 204/320 | 0 | 5 | — |
| pVisor stage | 128 | 0/5 | 183/640 | 0 | 5 | — |
| pVisor VM | 16 | 5/5 | 80/80 | 0 | 0 | 2047.9 |
| pVisor VM | 32 | 3/5 | 138/160 | 0 | 0 | 2047.8 |
| pVisor VM | 64 | 0/5 | 1/320 | 0 | 0 | — |
| pVisor VM | 128 | 0/5 | 0/640 | 0 | 5 | — |
| Podman | 16 | 5/5 | 80/80 | 0 | 0 | 969.3 |
| Podman | 32 | 5/5 | 160/160 | 0 | 0 | 1931.8 |
| Podman | 64 | 0/5 | 120/320 | 64 | 5 | — |
| Podman | 128 | 0/5 | 128/640 | 128 | 5 | — |

Stage and Podman each complete 160/160 tasks at concurrency 32. Both encounter OOM at 64/128; partial completions do not establish those capacities. VM completes 138/160 tasks at 32 with only 3/5 complete batches. Launch/completion failures at higher concurrency remain visible and cannot all be attributed to OOM. Unknown results lack retained verifiable completion evidence; they are neither successes nor zero-duration tasks.

### Idle environments

| Mode | Concurrency | Fully valid batches | Verified tasks / attempted | Unknown results | OOM batches | Barrier memory P50, MiB |
|---|---:|---:|---:|---:|---:|---:|
| Native process | 32 | 5/5 | 160/160 | 0 | 0 | 242.0 |
| Native process | 64 | 5/5 | 320/320 | 0 | 0 | 471.9 |
| Native process | 128 | 5/5 | 640/640 | 0 | 0 | 931.9 |
| pVisor stage | 32 | 5/5 | 160/160 | 0 | 0 | 407.8 |
| pVisor stage | 64 | 5/5 | 320/320 | 0 | 0 | 804.5 |
| pVisor stage | 128 | 5/5 | 640/640 | 0 | 0 | 1600.4 |
| pVisor VM | 32 | 5/5 | 160/160 | 0 | 0 | 2047.9 |
| pVisor VM | 64 | 0/5 | 7/320 | 0 | 0 | — |
| pVisor VM | 128 | 0/5 | 0/640 | 0 | 5 | — |
| Podman | 32 | 5/5 | 160/160 | 0 | 0 | 902.8 |
| Podman | 64 | 5/5 | 320/320 | 0 | 0 | 1806.9 |
| Podman | 128 | 0/5 | 141/640 | 384 | 5 | — |

Stage passes five rounds at idle concurrency 128, but adding private data and Git work reduces its highest wholly successful tested level to 32. Plan the two workloads separately; discrete levels also do not establish an exact maximum.

Compressed parked snapshots versus container pause have not completed validation. Snapshot file size or single-VM reclamation cannot establish capacity. Firecracker/QEMU density under this budget is unmeasured; their single-task latency is in the [runtime comparison](compare-runtimes.md). Matching macOS capacity is unmeasured.

### Downloads and reproduction {#run}

[Complete concurrency scan CSV](density-summary.csv) · [Artifacts, budget and evidence summary](density-provenance.csv) · [Comparison method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)

Derived tables retain attempts, complete batches, failure reasons, unknowns, OOM, observed ranges and source digests for every condition. Raw reports, logs, input manifests and source/binaries stay in local `.data/`; failed batches remain in capacity denominators.
