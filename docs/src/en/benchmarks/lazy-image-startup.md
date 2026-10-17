# How much waiting can lazy images save for startup and file loading?

## Main conclusions {#conclusions}

**pVisor reads image files on demand, reducing environment preparation time for short tasks.**

- **Download on demand.** Tasks can start before the complete image is downloaded. Cold clients fetched less data in the shell and NumPy experiments.
- **Reach results sooner.** Both short-task experiments showed shorter median waits from launch to checked output with cold client caches.
- **Reuse downloaded files.** Warm clients use cached content. The experiments required no repeat downloads of file content.

These results come from local experiments with prepared image services. Cache hits still leave mounting and executor startup costs; with the complete image cached, the Docker container had shorter startup waits in the application-startup experiments.

## Motivation: how long does environment preparation take for short tasks? {#motivation}

Agents frequently create disposable environments, and short tasks often read only a small part of an image — yet a complete pull must finish before they can start. Before adopting on-demand loading, two questions need answers: on first use, how much less data is downloaded and how much waiting is saved; and once content is cached locally, what startup overhead reuse still carries.

## Experiment design {#interpretation}

### What is compared? {#evidence}

Two experiments measure short-task startup and few-file reads separately. Startup uses Docker's complete pull as the reference; file reads use the same execution environment, with [TurboOCI][ref-turbooci] / [OverlayBD][ref-overlaybd] as the reference.

### File-read comparison {#stage-file-design}

On the same Linux machine, the same program reads and verifies four files from the same Ubuntu 26.04 image, comparing pVisor with OverlayBD. Both use the same execution environment, changing only image loading; each trial remounts the image and creates a working directory. This represents tasks that access few files, without running programs inside the image.

The task uses two fixed CPUs and image services use two other CPUs; service memory is not constrained to an equal total limit.

### Short-task startup comparison {#vm-design}

Run an Ubuntu shell and a Python script performing NumPy array calculations, covering a lightweight command and a short task that loads dependencies. Each task fixes the same image, command and output checks across paths: Docker pulls the complete image, while pVisor reads on demand and runs in a VM. Every trial creates a new container or VM; the two workloads are analyzed separately.

Both tasks use two CPUs. Docker has a 2 GiB memory limit, pVisor has 2 GiB guest RAM, and image services use two other CPUs. Container and VM limits cover different resource scopes, and background services are not covered by an equal whole-system limit; results describe each complete usage path.

### Measurement method {#measurement}

- **Cache and network:** Image services are prepared beforehand and accessed through local connections, retaining host page caches. Cold clients start with empty image caches; warm clients reuse previously fetched content. Both create a new execution environment.
- **Sampling:** 3 warmup rounds precede 30 paired rounds with randomized path order; each path has a cold trial followed by a warm trial. Preflight and warmup trials are excluded, and all valid slow samples are retained. Predefined background interference invalidates the entire file-read cohort.
- **Timing:** The primary metric, Ready, runs from launch to checked output, excluding exit and cleanup. Supplementary Completion runs through normal process exit and output-stream closure; the file-read comparison also includes unmount cleanup. Fetching, mounting and execution are timed; service preparation and cache reset are excluded. Response bytes are counted through Completion.
- **Validation:** Check file content or calculation results, execution records, actual mounts and resource cleanup. Cold clients must fetch content, and warm clients must not fetch it again. An execution, validation or cleanup failure invalidates the cohort; failures are never counted as zero time.

Results apply to local short tasks with prepared services. Initial service preparation, public networks, large-scale concurrency and bulk file I/O require separate measurements; file-read results also cannot substitute for application startup inside an image. See [downloads and sources](#run) for full environments, cache reset procedures and reproduction steps.

## Experimental data and analysis {#results}

A lazy image fetches files as they are accessed, so environment preparation advances with the task.

### On-demand fetching and short-task startup {#application-comparison}

Shell and NumPy were measured in independent cohorts on 2026-10-10, with n=30 per condition and Docker's complete pull as the reference. Waiting differences are differences in median Ready; fetch reductions use total application response bodies through Completion.

| Task | Cold cache: less waiting with pVisor (ms) | Cold cache: less data fetched with pVisor | Warm cache: more waiting with pVisor (ms) |
|---|---:|---:|---:|
| Shell | 575.0 | 91.2% | 60.9 |
| NumPy | 396.6 | 65.4% | 339.2 |

Cold clients fetch files on demand, with smaller response payloads and shorter median waits for both tasks. Docker starts faster when the complete image is cached. Lazy-loading benefits depend on how much content a task reads and whether that content is already local.

<details markdown="1">
<summary>Startup distributions, confidence intervals and complete fetch statistics</summary>

#### Ubuntu shell client startup waiting {#startup}

Measured 2026-10-10; n=30 per cell, 120 formal samples, zero formal failures or speed exclusions. Units ms. Unsplit distributions report P50; separated distributions report cluster medians and sample proportions. P95 is reference-only; cluster causes are undetermined.

| Path / cache | Ready: P50 or cluster medians (proportions) | Ready P95 | Completion: P50 or cluster medians (proportions) | Completion P95 |
|---|---:|---:|---:|---:|
| Docker cold | 597.2 (23/30); 1,563.5 (7/30) | 1,923.6 | 690.2 (25/30); 1,744.7 (5/30) | 2,032.0 |
| pVisor lazy cold | 186.1 | 256.5 | 312.5 | 381.2 |
| Docker warm | 85.0 (24/30); 212.6 (6/30) | 225.5 | 113.6 (22/30); 280.3 (8/30) | 317.2 |
| pVisor lazy warm | 151.0 | 184.2 | 267.3 | 310.0 |

With 5,000 paired-round bootstrap resamples, report **Docker minus lazy marginal median Ready differences**; n=30 pairs, units ms. The intervals describe overall medians, not individual cluster centers or every launch.

| Client cache | Median Ready difference (ms) | Paired 95% CI (ms) |
|---|---:|---:|
| Cold | 575.0 | [384.5, 841.3] |
| Warm | -60.9 | [-68.3, -30.4] |

#### Small Python/NumPy script {#numpy}

Independent cohort, also measured 2026-10-10, n=30 per cell, 120 formal samples, no failures or speed exclusions; units ms and the same distribution rules.

| Path / cache | Ready: P50 or cluster medians (proportions) | Ready P95 | Completion: P50 or cluster medians (proportions) | Completion P95 |
|---|---:|---:|---:|---:|
| Docker cold | 1,001.5 (19/30); 2,233.3 (11/30) | 2,836.5 | 1,041.7 (19/30); 2,305.9 (11/30) | 2,877.1 |
| pVisor lazy cold | 778.0 | 1,086.1 | 925.0 | 1,307.3 |
| Docker warm | 120.7 (24/30); 196.6 (6/30) | 211.5 | 161.9 (24/30); 271.1 (6/30) | 276.7 |
| pVisor lazy warm | 462.2 | 521.3 | 606.8 | 677.0 |

Again report Docker minus lazy marginal median Ready differences, with n=30 pairs and 5,000 paired bootstrap resamples; units ms.

| Client cache | Median Ready difference (ms) | Paired 95% CI (ms) |
|---|---:|---:|
| Cold | 396.6 | [231.7, 1,055.3] |
| Warm | -339.2 | [-345.9, -332.4] |

Under these configurations, cold-client median waiting favors lazy loading and warm-client waiting favors Docker. NumPy imports require more files, increasing fetching on the lazy path; Completion also includes Job teardown costs. A smaller median does not promise a shorter wait on every launch.

#### Client payloads {#transfer}

Units bytes, counted through Completion, n=30 per cell; all counts within each cell are identical.

| Workload / path / cache | File content or OCI blobs | Binary Metadata Data | Total responses | Reduction from cold Docker for the same workload |
|---|---:|---:|---:|---:|
| Shell / docker / cold | 41,842,292 | 0 | 41,843,684 | Baseline |
| Shell / docker / warm | 0 | 0 | 0 | — |
| Shell / lazy / cold | 2,580,417 | 1,117,945 | 3,702,376 | 91.2% |
| Shell / lazy / warm | 0 | 0 | 641 | — |
| NumPy / docker / cold | 115,930,809 | 0 | 115,932,435 | Baseline |
| NumPy / docker / warm | 0 | 0 | 0 | — |
| NumPy / lazy / cold | 36,957,665 | 3,085,982 | 40,072,915 | 65.4% |
| NumPy / lazy / warm | 0 | 0 | 816 | — |

Reduction is `1 − lazy total responses / Docker total responses`, comparing only cold clients for the same workload. Docker content counts compressed layers and config; lazy content counts uncompressed Read Data. Binary index pages are counted separately as Metadata Data and included in total responses; this column measures application response bodies rather than total wire traffic. Docker responses exclude HTTP headers/TLS; both paths exclude TCP/IP overhead. Warm lazy still performs Prepare/Ping.

</details>

### On-demand reads and cache reuse {#stage-comparison}

Measured 2026-10-17, n=30 per condition; all 120 formal executions and 12 warmups passed validation, with zero failures or exclusions. This experiment reads and verifies only four files, and is analyzed separately from the application-startup cohorts.

| Client cache | pVisor Ready P50 (ms) | OverlayBD Ready P50 (ms) | Less waiting with pVisor |
|---|---:|---:|---:|
| Cold | 78.15 | 233.29 | 66.5% |
| Warm | 52.15 | 161.19 | 67.6% |

pVisor completes the four-file check in a median 78.15 ms cold and 52.15 ms warm. Warm clients reuse file content on both paths. Timing includes opening the image, mounting and execution, so the differences reflect the complete loading implementations.

<details markdown="1">
<summary>Complete file-read times, confidence intervals and fetch statistics</summary>

Measured 2026-10-17, n=30 per cell; all 120 formal executions and 12 warmups passed independent audit, with zero failures or exclusions. Units ms; all distributions are unsplit and report P50. Different dates and application-startup cohorts are analyzed separately.

| File-loading path / client cache | Ready P50 (ms) | Completion P50 (ms) |
|---|---:|---:|
| pVisor host FUSE / cold | 78.15 | 99.97 |
| OverlayBD ext4 / cold | 233.29 | 384.27 |
| pVisor host FUSE / warm | 52.15 | 83.62 |
| OverlayBD ext4 / warm | 161.19 | 296.69 |

With 5,000 paired-round bootstrap resamples, median differences are **pVisor minus OverlayBD**, so negative values mean less waiting with pVisor. Waiting reduction is `1 − pVisor P50 / OverlayBD P50`, calculated from unrounded statistics within the same cohort. Each cell has n=30; differences are in ms and reductions in %. All four difference intervals exclude zero.

| Client cache | Metric | Median difference and 95% CI (ms) | Waiting reduction and 95% CI (%) |
|---|---|---:|---:|
| Cold | Ready | -155.14 [-171.64, -145.16] | 66.5 [63.2, 68.8] |
| Warm | Ready | -109.04 [-116.76, -103.18] | 67.6 [66.1, 69.0] |
| Cold | Completion | -284.29 [-299.20, -268.76] | 74.0 [71.8, 75.0] |
| Warm | Completion | -213.08 [-234.06, -202.03] | 71.8 [70.4, 77.8] |

Application responses observed after completion are below, in bytes, n=30 per cell, reporting P50. Cold OverlayBD content varies with the ranges actually fetched.

| File-loading path / client cache | Content payload P50 (B) | Metadata payload P50 (B) | Total responses P50 (B) |
|---|---:|---:|---:|
| pVisor host FUSE / cold | 2,575,738 | 921,337 | 3,500,548 |
| OverlayBD ext4 / cold | 4,292,610 | 1,502,328 | 5,794,938 |
| pVisor host FUSE / warm | 0 | 0 | 619 |
| OverlayBD ext4 / warm | 0 | 0 | 0 |

For a few-file read under these conditions, pVisor's file-level path reduces both waiting and content fetching. Warm clients on both paths avoid downloading file content again, while opening, mounting and stage execution costs remain. pVisor content counts uncompressed `Read Data`; OverlayBD content counts Range responses from original compressed OCI layers. Total responses count cache frames and data or registry response bodies, respectively. Both exclude TCP/IP overhead, and registry counts also exclude HTTP headers. These counters are not wire-traffic measurements or estimates of transfer at Ready.

</details>

### Service-side cache preparation {#preparation}

Units s, n=1 per operation, all excluded from client timers. Registry publication excludes the preceding validation/archive step; complete preparation phases remain in retained evidence.

| Cache preparation operation | Shell | NumPy |
|---|---:|---:|
| Cache store copy | 5.503 | 3.562 |
| Original OCI blobs → registry | 0.501 | 0.301 |
| Cached cache-service Prepare | 2.385 | 0.002 |

These are local cached preparation costs. Initial upstream download, unpacking and indexing remain unmeasured for these artifacts; historical first-pull observations use different artifacts and are not substituted here.

<details markdown="1">
<summary>Comparison scope for the upstream performance table</summary>

### Relation to the upstream performance table {#upstream-comparison}

The [official TurboOCI performance table][ref-turbooci-performance] times image pull through service availability, using `rpull` for lazy paths. The stage workload here verifies four files read-only; shell and NumPy application startup use separate experiments. The upstream services, images and environment differ from these conditions. Their seconds are not divided by local milliseconds to calculate speedups; all tables here use their explicitly identified pVisor experiment cohorts.

</details>

### Sources and reproduction {#run}

<details markdown="1">
<summary>Statistics downloads, sources and reproduction methods</summary>

<!-- measurement-status:start -->

The same-stage file-loading comparison has completed measurement through the new framework; the VM / Docker shell and NumPy data retain their independent historical cohorts. See [measurement coverage](index.md#measurement-coverage).

<!-- measurement-status:end -->

[Stage statistics CSV](../../../overrides/assets/benchmarks/data/lazy/lazy-stage-summary.csv) · [Stage paired differences CSV](../../../overrides/assets/benchmarks/data/lazy/lazy-stage-comparisons.csv) · [Stage provenance summary](../assets/benchmarks/lazy-stage.provenance.json) · [Stage reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark.new/startup/README.md#stage-lazy)

Stage raw evidence stays in `benchmark.new/startup/.data/overlaybd-lab-20261017/stage-lazy-20261017-a/`. The independent audit recomputes raw clocks and rechecks file validation, mount and device identities, Run Bundles, per-request bytes, 33 rounds of background observations and cleanup, and verifies the semspec export. Binary, runtime-library and frozen-harness hashes are retained; their exact correspondence to a source build is unverified. Only processed statistics and compact provenance are published; raw samples and full inventories stay in local evidence.

[Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#current-implementation-dockerlazy-comparison)

Complete CSV/JSON and raw evidence stay in `benchmark/pvisor/.data/lazy-shell-current-formal-20261010/` and `lazy-numpy-current-formal-20261010-2/`; each `derive.py` rechecks all 136 launches, frozen inputs, retained OCI digests and cleanup receipts before regenerating CSVs. Build evidence is in `.data/lazy-current-build-20261010/`: 1,193 frozen source inputs and measured binary digests match the retained fresh build. This verifies the recorded relationship without establishing hermetic reproducibility. Failed preflights/cohorts and historical 2026-10-07 data remain separate. These observations are also separate from [prepared-environment startup](startup.md).

</details>

### References {#references}

1. containerd. [OverlayBD README (v1.0.18)][ref-overlaybd]. Layered block images, the TCMU backend and integration with Accelerated Container Image.
2. containerd. [Overlaybd — TurboOCIv1][ref-turbooci], including [Performance][ref-turbooci-performance]. Original OCI content, metadata images and upstream timing definitions; repository `main` documentation, accessed 2026-10-17.
3. Huiba Li et al. [DADI: Block-Level Image Service for Agile and Elastic Application Deployment][ref-dadi]. USENIX ATC 2020, pp. 727–740. Research background for block-level on-demand image services.

[ref-overlaybd]: https://github.com/containerd/overlaybd/blob/v1.0.18/README.md
[ref-turbooci]: https://github.com/containerd/accelerated-container-image/blob/main/docs/TURBO_OCI.md
[ref-turbooci-performance]: https://github.com/containerd/accelerated-container-image/blob/main/docs/TURBO_OCI.md#performance
[ref-dadi]: https://www.usenix.org/conference/atc20/presentation/li-huiba
