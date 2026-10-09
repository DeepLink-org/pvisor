# How much startup waiting can on-demand image reads save for shell and NumPy tasks?

## Conclusions {#conclusions}

**With Linux/KVM, prepared local image services and warm host page caches, pVisor lazy VM reaches checked output at cold-client P50 186.1 ms for Ubuntu shell and 778.0 ms for a small NumPy script. Both same-cohort Docker comparisons support shorter cold-client median waits; warm-client Docker is faster.**

| Need | Selection implication |
|---|---|
| Uncached image, short shell or NumPy task | On-demand reads reduce content downloads and cold-client median waiting under these conditions |
| Complete image already cached locally | Docker produces checked output sooner; lazy VM still incurs VM/Job startup and teardown |
| First deployment of the image service | Budget upstream download, unpack and indexing separately; cached preparation does not measure these costs |

## Motivation {#motivation}

Downloading and unpacking a complete image can cost more than a short task in a disposable environment. Choosing on-demand reads requires distinguishing service preparation from client startup and checking whether the advantage persists after caching.

## Experiment design {#interpretation}

`B-LAZY-STARTUP`, Linux x86_64/KVM, Fedora kernel 7.2.8-200.fc44.x86_64, Docker 29.7.2 with overlayfs/containerd image store. Both paths use the same pinned amd64 manifest per workload: Ubuntu 26.04 shell; Python 3.13.14 / NumPy 2.5.2 from `amancevice/pandas` (pandas is not imported). Docker pulls complete compressed blobs from a fresh Distribution registry; pVisor uses real `pvisor-cache serve` with client binary index pages, upstream pooling and the persistent private bridge enabled. Static release artifacts and embedded firmware match retained fresh-build evidence.

Shell verifies `/etc/os-release` before its unique marker. NumPy uses `-B -u`, `PYTHONHASHSEED=0` and one thread per numerical library, checks versions, verifies square sum 1240 and matrix-product sum 3680 for an int64 4×4 array, then prints its marker. Every launch must exit 0. **Ready** ends at checked output; **Completion** includes exit and output-stream closure.

Each workload is an independent 30-round cohort, with randomized Docker/lazy order, cold immediately followed by warm, and a new container/VM/workspace/stage each launch. One initial round and three warmups are excluded. Cold Docker must fetch all blobs; cold lazy uses a fresh client cache; warm reuses the corresponding cache and must transfer no content. Any correctness, accounting or teardown failure invalidates the campaign; valid slow samples are retained. Sampling runs without concurrent tests or builds.

| Control | Configuration and scope |
|---|---|
| Workload budget | CPUs 0/1; Docker two-core quota and 2 GiB hard limit with no extra swap; VM 2 vCPU and 2 GiB guest RAM, differing from an enclosing process-tree limit |
| Services | Registry/cache CPUs 2/3; daemon/containerd and proxies are not fully constrained to an equal whole-system budget |
| Network/cache | Local loopback, HTTPS registry versus authenticated unencrypted TCP cache; no RTT/bandwidth injection; warm services and host page cache |
| Isolation/validation | Private user/mount/PID namespace and pivot_root preserve the existing listener; output, Run Bundle, frozen bytes, cold/warm accounting and namespace teardown checked |

Registry preparation publishes validated retained **original compressed OCI blobs**; the cache service receives a copied supported store and performs cached Prepare. No upstream pull/unpack/index time is measured. Host trees are bound read/write; these checks do not establish complete host integrity. WAN, concurrency, full distribution boot, large computation, equal whole-system budgets and pure VMM/lazy-algorithm contributions remain unmeasured. Separate workload and historical cohorts are not pooled or used for implementation speedup claims.

## Data and analysis {#results}

### Ubuntu shell client startup waiting {#startup}

Measured 2026-10-10; n=30 per cell, 120 formal samples, zero formal failures or speed exclusions. Units ms. Unsplit distributions report P50; separated distributions report cluster medians and proportions. P95 is reference-only; cluster causes are undetermined.

| Path / cache | Ready: P50 or cluster medians (proportions) | Ready P95 | Completion: P50 or cluster medians (proportions) | Completion P95 |
|---|---:|---:|---:|---:|
| Docker cold | 597.2 (23/30); 1,563.5 (7/30) | 1,923.6 | 690.2 (25/30); 1,744.7 (5/30) | 2,032.0 |
| pVisor lazy cold | 186.1 | 256.5 | 312.5 | 381.2 |
| Docker warm | 85.0 (24/30); 212.6 (6/30) | 225.5 | 113.6 (22/30); 280.3 (8/30) | 317.2 |
| pVisor lazy warm | 151.0 | 184.2 | 267.3 | 310.0 |

Docker minus lazy **marginal median Ready differences**, with 5,000 paired-round bootstrap resamples: Cold: **575.0 ms**, 95% CI **[384.5, 841.3] ms**; Warm: **-60.9 ms**, 95% CI **[-68.3, -30.4] ms**. These intervals describe overall medians, not individual cluster centers or every launch.

### Small Python/NumPy script {#numpy}

Independent cohort, also measured 2026-10-10, n=30 per cell / 120 formal samples, no failures or speed exclusions; ms and the same distribution rules.

| Path / cache | Ready: P50 or cluster medians (proportions) | Ready P95 | Completion: P50 or cluster medians (proportions) | Completion P95 |
|---|---:|---:|---:|---:|
| Docker cold | 1,001.5 (19/30); 2,233.3 (11/30) | 2,836.5 | 1,041.7 (19/30); 2,305.9 (11/30) | 2,877.1 |
| pVisor lazy cold | 778.0 | 1,086.1 | 925.0 | 1,307.3 |
| Docker warm | 120.7 (24/30); 196.6 (6/30) | 211.5 | 161.9 (24/30); 271.1 (6/30) | 276.7 |
| pVisor lazy warm | 462.2 | 521.3 | 606.8 | 677.0 |

Docker minus lazy marginal median Ready differences: Cold: **396.6 ms**, 95% CI **[231.7, 1,055.3] ms**; Warm: **-339.2 ms**, 95% CI **[-345.9, -332.4] ms**. Cold waiting favors lazy under this configuration; warm waiting favors Docker. Lazy cold Ready ranges from 682.8 to 2,397.3 ms, so a smaller median does not promise a shorter wait on every launch. Completion also includes Job teardown costs.

### Client payloads {#transfer}

Bytes through Completion, n=30 per cell; all counts within each cell are identical.

| Workload / path / cache | File content or OCI blobs | Binary Metadata Data | Total responses |
|---|---:|---:|---:|
| Shell / docker / cold | 41,842,292 | 0 | 41,843,684 |
| Shell / docker / warm | 0 | 0 | 0 |
| Shell / lazy / cold | 2,580,417 | 1,117,945 | 3,702,376 |
| Shell / lazy / warm | 0 | 0 | 641 |
| NumPy / docker / cold | 115,930,809 | 0 | 115,932,435 |
| NumPy / docker / warm | 0 | 0 | 0 |
| NumPy / lazy / cold | 36,957,665 | 3,085,982 | 40,072,915 |
| NumPy / lazy / warm | 0 | 0 | 816 |

Docker content counts compressed layers and config; lazy content counts uncompressed Read Data. Binary index pages are counted separately as Metadata Data and included in total responses. Thus fewer file-content bytes do not specify total wire savings. Docker responses exclude HTTP headers/TLS; both paths exclude TCP/IP overhead. Warm lazy still performs Prepare/Ping.

### Cached service preparation {#preparation}

Seconds, n=1 per operation; excluded from client timers. Registry publication excludes the preceding validation/archive step; all preparation phases remain in retained evidence.

| Cached preparation operation | Shell | NumPy |
|---|---:|---:|
| Store copy | 5.503 | 3.562 |
| Original OCI blobs → registry | 0.501 | 0.301 |
| Cached cache-service Prepare | 2.385 | 0.002 |

These are local cached preparation costs. Initial upstream download, unpack and indexing remain unmeasured for these artifacts; historical first-pull observations use different artifacts and are not substituted here.

### Downloads and sources {#run}

[Shell statistics CSV](lazy-startup-summary.csv) · [Shell differences/preparation CSV](lazy-startup-details.csv) · [Shell provenance CSV](lazy-startup-provenance.csv)

[NumPy statistics CSV](lazy-numpy-summary.csv) · [NumPy differences/preparation CSV](lazy-numpy-details.csv) · [NumPy provenance CSV](lazy-numpy-provenance.csv) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#current-implementation-dockerlazy-comparison)

Raw evidence stays in `benchmark/pvisor/.data/lazy-shell-current-formal-20261010/` and `lazy-numpy-current-formal-20261010-2/`; each `derive.py` rechecks all 136 launches, frozen inputs, retained OCI digests and teardown receipts before regenerating CSVs. Build evidence is in `.data/lazy-current-build-20261010/`: 1,193 frozen source inputs and measured binary digests match the retained fresh build. This verifies the recorded relationship, without establishing hermetic reproducibility. Failed preflights/campaigns and historical 2026-10-07 cohorts remain separate. These observations are also separate from [prepared-environment startup](startup.md).
