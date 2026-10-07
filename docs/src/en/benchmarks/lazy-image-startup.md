# How much startup waiting can on-demand image reads save for shell and NumPy tasks?

## Conclusions {#conclusions}

**With Linux/KVM, prepared image services and warm host page caches in a local protocol simulation, the Ubuntu shell on pVisor lazy VM reaches the checked first output with a cold-client P50 of 183.7 ms; all 30 observations precede cold-client Docker. For a small Python/NumPy script, no cold-client Ready median difference is detected despite lower content transfer; warm-client Docker is faster.**

| Need | Selection implication |
|---|---|
| Uncached image, short shell task | On-demand reads reduce client content downloads and startup waiting |
| Uncached image, small Python/NumPy script | Less content transfer does not establish a cold-startup benefit |
| Complete image already cached locally | Docker usually has a shorter wait for the shell marker and is faster for NumPy |
| First deployment of the image service | Budget pull, unpack and indexing separately; client timing excludes preparation |

## Motivation {#motivation}

For short commands in disposable environments, downloading and unpacking the complete image can cost more than the task itself. Choosing on-demand reads requires distinguishing service preparation from client startup and checking whether the advantage persists when the image is cached.

## Experiment design {#interpretation}

Linux x86_64 / KVM, Fedora kernel 7.2.8-200.fc44.x86_64, Docker 29.7.2, overlayfs/containerd image store. `ubuntu:latest` is pinned to the same Ubuntu 26.04 amd64 manifest. Docker pulls the complete image from an open-source Distribution `registry:3` mirror; pVisor reads files on demand through real `pvisor-cache serve`. This service is distinct from OpenSandbox's `pvisor-daemon serve`.

The Ubuntu shell cohort runs the same `/bin/sh` on both paths: read `/etc/os-release`, verify Ubuntu 26.04, print one correct marker and exit successfully. **Ready** measures CLI launch to observation of the complete marker; **Completion** ends at process exit and output-stream closure, including teardown. The workload represents initialization for short commands; full distribution boot, Agent CLI initialization and large tool tasks are unmeasured.

A separate Python/NumPy cohort uses the public `amancevice/pandas:slim-3.0.5` image, pinned to amd64 digest `sha256:9a3a94039175ac799ad33c1a207997994ff9b24814e06508dddfa259b1ed9159`. Both paths run the same small script with Python 3.13.14 and NumPy 2.5.2; pandas is included but not imported. With `-B -u`, `OMP_NUM_THREADS=1`, `MKL_NUM_THREADS=1`, `OPENBLAS_NUM_THREADS=1` and `PYTHONHASHSEED=0`, it checks both versions, creates `x = np.arange(16, dtype=np.int64).reshape(4, 4)`, asserts `np.square(x).sum() == 1240` and `(x @ x.T).sum() == 3680`, then prints one unique marker and exits with code 0. Ready therefore includes imports and numerical correctness checks, representing a small numerical-script environment rather than large computation. The CPU/memory budgets, loopback protocol and cache/sampling controls below also apply to NumPy. Formal NumPy sampling ran without concurrent tests, builds or kernel benchmarks. The two workload cohorts remain separate; comparisons between them do not establish causal effects of workload or implementation changes.

Thirty formal samples per path/cache state in each cohort, with randomized Docker/lazy order within each round and cold followed immediately by warm for each path. One initial preflight and three warmup rounds are excluded. Each launch uses a fresh container or VM, workspace and stage. Before cold Docker, only the benchmark image is removed, and complete retrieval of every blob is required. Lazy uses a fresh client cache per pair, reused for warm startup. Services and host page caches remain warm; cold host storage and fully cold Docker snapshotter state are not independently established.

| Control | Actual configuration and scope |
|---|---|
| Workload CPU / memory | Docker: CPUs 0/1, two-core quota, 2 GiB hard limit, no additional swap; pVisor: launch affinity 0/1, 2 vCPU, 2 GiB guest RAM |
| Services and clients | Registry/cache pinned to CPUs 2/3; Docker daemon/containerd and counting proxies are not fully constrained to two cores; guest RAM differs from an enclosing host process-tree hard limit |
| Network | Local loopback; Docker uses HTTPS through a counting proxy, cache uses authenticated unencrypted TCP; no injected RTT or bandwidth limit |
| Validation and rejection | Checked output, successful exit, complete cold downloads and warm-cache hits; lazy also checks the Run Bundle's VM/staging declarations. Failure or timeout fails the experiment; valid slow samples are retained |

Included samples do not establish independent resource enforcement or host-integrity verification. Results describe these complete startup configurations; actual WAN, concurrency, equal whole-system budgets, pure lazy-algorithm contributions and pure VMM contributions are unmeasured. Service preparation is recorded separately: cache imports the same digest from Docker Hub, since the OCI client requires trusted HTTPS and does not import from the insecure local registry.

## Data and analysis {#results}

### Ubuntu shell client startup waiting {#startup}

Measured 2026-10-07, n=30 per cell, 120 valid samples, zero formal failures and zero speed-based exclusions. Unit ms; unsplit distributions report P50, separated distributions report cluster medians and sample proportions. P95 is reference-only.

| Path / cache | Ready: P50 or cluster medians (proportions) | Ready P95 | Completion: P50 or cluster medians (proportions) | Completion P95 |
|---|---:|---:|---:|---:|
| Docker cold | 582.9 (23/30); 1,524.4 (7/30) | 1,855.1 | 600.6 (19/30); 1,465.1 (11/30) | 1,944.8 |
| pVisor lazy cold | 183.7 | 229.2 | 331.0 | 381.2 |
| Docker warm | 82.6 (25/30); 202.3 (5/30) | 210.3 | 119.6 | 311.0 |
| pVisor lazy warm | 162.7 | 182.1 | 295.5 | 330.6 |

| Cold-client Ready range, n=30 | Minimum ms | Maximum ms |
|---|---:|---:|
| Docker | 528.2 | 2,052.4 |
| pVisor lazy | 170.1 | 302.1 |

Every cold lazy observation precedes cold Docker, so the direction does not depend on a single overall median. Most warm Docker observations have shorter waits, with a separate slower cluster. Completion shows that VM/Job teardown also belongs in short-task budgets. Clusters follow the existing descriptive rule; their causes are undetermined.

Resampling complete paired rounds 5,000 times gives 95% intervals for **Docker minus lazy marginal overall median differences**: **389.2–833.9 ms** cold and **−86.1 to −48.2 ms** warm. These intervals do not represent the centers of Docker's individual clusters and do not establish a single speed multiplier or causal pure-lazy benefit.

### Client content transfer {#transfer}

Unit bytes, n=30 per cell, identical counts within each condition; measured through Completion.

| Path / cache | OCI blob or file content | Related response count |
|---|---:|---:|
| Docker cold | 41,842,292 | 41,843,684 |
| pVisor lazy cold | 2,580,417 | 2,588,104 |
| Docker warm | 0 | 0 |
| pVisor lazy warm | 0 | 575 |

Docker's content column counts compressed OCI layers and config; lazy counts uncompressed file-read payload. The short shell needs only the shell, dynamic loader, libc and related files, making the lazy content payload **93.8% smaller** than complete OCI blobs. This describes application payloads for two delivery formats; total network traffic reduction is unmeasured. Docker's response count excludes HTTP headers/TLS, while lazy includes protocol frames and metadata; both exclude TCP/IP. Warm lazy still requires ping/prepare requests.

### Image-service preparation cost {#preparation}

Unit s, n=1 per operation, single observed values.

| Preparation operation | Time | Included work |
|---|---:|---|
| Docker Hub → Distribution mirror | 12.483 | skopeo pull and push of the selected platform image |
| Docker Hub → first cache preparation | 15.130 | Pull, unpack, index and return a read handle |

**The 183.7 ms cold-client Ready excludes the 15.130 s initial cache preparation.** Preparation paths and work differ, so no preparation-speed ranking or amortization break-even estimate is supplied.

### Small Python/NumPy script {#numpy}

Measured 2026-10-07 in a separate cohort: 120 formal samples, n=30 per cell, zero formal failures and zero speed-based exclusions. Unit ms; all Ready and Completion distributions are unsplit under the publication rule, so report P50. P95 is reference-only.

| Path / cache | Ready P50 | Ready P95 | Completion P50 | Completion P95 |
|---|---:|---:|---:|---:|
| Docker cold | 1,077.6 | 2,367.1 | 1,118.1 | 2,417.8 |
| pVisor lazy cold | 1,213.0 | 1,312.1 | 1,400.8 | 1,532.4 |
| Docker warm | 120.8 | 151.5 | 157.3 | 206.9 |
| pVisor lazy warm | 520.5 | 626.7 | 686.9 | 825.1 |

Paired-round bootstrap (5,000 resamples) gives a **Docker minus lazy marginal Ready median difference** of **−135.3 ms**, 95% CI **[−204.1, 176.7] ms**, for cold clients. The interval crosses zero: **no cold-Ready median difference is detected**, so neither path is established as reliably faster. The warm difference is **−399.7 ms**, 95% CI **[−408.0, −389.8] ms**: Docker is faster.

Through Completion, cold content is **115,930,809 bytes** for Docker versus **36,951,980 bytes** for lazy, a **68.1% reduction**; related response counts are **115,932,435** versus **37,153,682 bytes**. Warm content is **0 bytes** for both, with response counts **0** for Docker and **750 bytes** for lazy. Counts are identical within each cell (n=30); the content/response definitions and network-overhead exclusions above apply. Reduced content does not establish reduced cold startup waiting for this script. Separate service preparation takes **33.944 s** for Docker Hub → Distribution mirror and **46.189 s** for Docker Hub → cache (n=1 each), excluded from client timing; these single observations do not support a preparation-speed ranking.

### Downloads and sources {#run}

[Startup and payload statistics CSV](lazy-startup-summary.csv) · [Differences and preparation costs CSV](lazy-startup-details.csv) · [Artifact and experiment provenance CSV](lazy-startup-provenance.csv) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#lazy-image-client-startup)

[NumPy startup and payload statistics CSV](lazy-numpy-summary.csv) · [NumPy differences and preparation costs CSV](lazy-numpy-details.csv) · [NumPy artifact and experiment provenance CSV](lazy-numpy-provenance.csv)

NumPy and Ubuntu shell use the same preexisting launcher and embedded firmware. NumPy uses a separately built cache-service binary with Docker gzip-layer support; this comparison does not measure gains from implementation changes. Exact artifact hashes are retained separately in the provenance CSVs.

`B-LAZY-STARTUP` uses a pinned manifest, preexisting release static musl artifacts and embedded firmware. Build-time source relationships are unverified; the current source manifest does not prove the artifacts came from that checkout. Artifact hashes match after sampling. Raw samples, logs, Run Bundles, artifact copies and the frozen harness remain in local ignored `.data/`; CSVs retain cohort, conditions, statistical definitions and the original report digest. Samples are kept separate from [prepared-environment startup](startup.md).
