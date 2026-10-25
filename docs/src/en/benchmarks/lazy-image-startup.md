# How much disk, transfer and waiting do lazy images save?

## Motivation {#motivation}

Compressed images suit disk-sensitive short tasks that initially access few files: they avoid resident extracted copies and deliver remote content by working set. For the tested short tasks and library scans, native Docker is faster with a complete cache or broad file traversal, while a separate guest kernel adds per-instance memory cost. Choose by disk budget and actual access patterns; compression does not establish universal performance superiority.

The four real images save 71%–73% in independent inventories and 74% in a genuinely shared store. Cold shell responses shrink 89.9% and Python checks 57.1%; a complete library mmap scan saves only 9.7% and waits longer. At four instances, baseline-subtracted memory is 8.31 MiB per native Docker instance and 48.95 MiB per pVisor VM.

## Experiment design {#interpretation}

Measure complete-image allocation, client launch-to-checked-output Ready, response payloads and per-instance memory increments as image-delivery and usage cost. These costs represent disk capacity, short-task waiting and deployment budgets. Lower values require less storage, transfer or resources for the same workload.

2026-10-25, Linux x86_64/KVM, Ryzen 7 9700X, frozen current release; native Docker 29.7.2/rootless containerd overlayfs and default pVisor V2. Both pin the same linux/amd64 manifests and check OCI content, OS/Python identity, private files and normal exit. Four independent cohorts are never pooled: local inventories N=3; prepared shell Ready N=30/memory N=5; three remote workloads cold/warm N=30 each; local Python and two-pass mmap N=30 each. Timings use three warmups, randomized ordering and every valid slow sample; no P99 is claimed.

Local inventories start empty and include metadata, normal sharing and Docker compressed blobs plus unpacked snapshots, subtracting empty Docker storage. Sources, tools and runtime writable layers are excluded. Both directories disable Btrfs transparent compression; hardlinks count once. Allocated blocks are not exclusive device bytes. Prepared instances exclude pull. Memory covers complete limited product groups including daemons/VMMs and charged caches; increments subtract each group's own no-instance baseline, not whole-host added physical memory.

Remote services are prepared locally. Cold clients have empty caches; immediate warm clients reuse content but still create new VMs/containers. Docker includes complete OCI pull+run, pVisor actual on-demand spawn, excluding service preparation/private daemon startup and retaining host page caches. Applications use CPUs 0,1, services 2,3; configurations request 2 CPU/512 MiB containers or 2-vCPU/512-MiB guests. A private HTTPS registry and token-authenticated loopback cache TCP have different transport/runtime boundaries. Timings establish neither pure-format effects nor WAN behavior.

Local product groups have 4 CPU/4 GiB/zero swap; shell instances use 1 CPU/256 MiB, Python/mmap 2 CPU/512 MiB. Container limits and guest RAM have different scopes, not equal application-available memory. Mmap authenticates 1262 regular files and 25,557,453 bytes across the complete Python library tree using an independently OCI-derived aggregate hash. Local instances read twice; each pass includes traversal, mmap and SHA-256. The second pass reuses pages rather than measuring decompression throughput or random first faults.

## Data and analysis {#results}

### Local image inventories {#storage}

MiB, medians of three fresh stores. The four-image combination is genuinely shared.

| Image inventory | Docker MiB | pVisor V2 MiB | Reduction |
| --- | --- | --- | --- |
| Alpine 3.20 | 12.53 | 3.59 | 71.3% |
| Ubuntu 24.04 | 110.18 | 31.64 | 71.3% |
| Python 3.12 slim | 176.68 | 47.15 | 73.3% |
| Python 3.13 slim | 172.34 | 46.18 | 73.2% |
| Four-image shared store | 363.16 | 94.29 | 74.0% |


![Disk allocation of complete local inventories](../assets/benchmarks/lazy-v2-capacity.svg)

Docker's shared store contains 90.29 MiB of compressed blobs and 272.75 MiB of unpacked snapshots. The main structural difference is avoiding a complete resident extracted tree; V2's encoded pool need not be smaller than compressed OCI blobs. Import peaks, online GC and long-term cache growth are excluded.

### Remote cold/warm clients {#application-comparison}

Ready in ms, N=30. Separated groups show `group median (count/30)` instead of a single P50. Predeclared sorted-gap rules identify groups; every point remains. Differences are paired medians with 5,000-draw bootstrap 95% intervals, not differences between marginal medians. Normal exit is retained separately in raw evidence.

| Workload/client cache | Docker Ready ms | pVisor Ready ms | PV−Docker [95% CI] ms |
| --- | --- | --- | --- |
| Shell / cold | 392.99 | 198.25 | -194.70 [-201.47, -190.29] |
| Shell / warm | 68.49 | 162.25 | +93.16 [+91.20, +95.30] |
| Python / cold | 686.46 | 505.75 | -180.81 [-189.82, -173.98] |
| Python / warm | 132.92 | 341.08 | +207.51 [+205.60, +211.45] |
| Python-library mmap / cold | 679.78 | 1601.68 | +919.44 [+907.06, +936.33] |
| Python-library mmap / warm | 103.80 | 1067.12 | +963.57 [+957.46, +966.41] |


![Every cold/warm startup sample and response payloads](../assets/benchmarks/lazy-v2-remote.svg)

Cold shell and Python checks benefit from fetching less content; native Docker is faster for warm clients. The complete library scan waits longer than a full OCI pull followed by execution: metadata and file-access costs can outweigh avoided transfer. Python here means standard-library checks, not the historical NumPy workload.

Cold response payloads, median MiB, N=30, including metadata. Docker file content consists of compressed OCI blobs; pVisor file content is raw Read data. Totals describe application responses, excluding HTTP headers, TLS and TCP/IP, without an unmeasured Ready-traffic cutoff.

| Workload | Docker total MiB | PV file MiB | PV index MiB | PV total MiB | Reduction |
| --- | --- | --- | --- | --- | --- |
| shell | 28.39 | 2.80 | 0.06 | 2.86 | 89.9% |
| python | 43.18 | 18.43 | 0.08 | 18.53 | 57.1% |
| mmap-scan | 43.18 | 38.70 | 0.15 | 39.01 | 9.7% |


Every warm sample fetches zero file content; pVisor retains small Prepare/Ping responses. Payload savings do not directly establish public-network bills or transfer-time savings.

### Reads and memory after complete preparation {#local-access}

Milliseconds, N=30. Ready is host-observed; mmap passes use internal instance clocks and must not be added to Ready. First scans retain host page caches; second scans reuse the same instance's pages.

| Prepared local metric | Docker ms | pVisor ms | PV−Docker [95% CI] ms |
| --- | --- | --- | --- |
| Shell Ready | 79.53 | 106.67 | +26.80 [+25.84, +28.20] |
| Python Ready | 132.70 | 343.88 | +211.27 [+203.16, +216.87] |
| mmap first pass | 25.47 | 619.98 | +594.53 [+593.84, +595.97] |
| mmap repeated pass | 22.31 | 461.79 | +439.48 [+438.70, +439.95] |


![Every local creation and first/repeated mmap scan sample](../assets/benchmarks/lazy-v2-runtime.svg)

Remote access is not the entire cost. Local pools also traverse VM file services, path resolution and guest page caches. This comparison includes runtime/filesystem differences: the full gap is not attributable to Zstd. Warm repeated scans do not establish random mmap or write performance.

Whole-group memory, median MiB across five fresh product groups; idle shell/sleep only.

| Idle deployment memory | Docker MiB | pVisor MiB | PV−Docker [95% CI] MiB |
| --- | --- | --- | --- |
| Prepared / 0 instances | 212.21 | 35.11 | -177.50 [-178.00, -170.77] |
| Whole group / 4 instances | 244.05 | 230.66 | -13.39 [-14.95, -8.84] |
| Per instance / baseline-subtracted at 4 | 8.31 | 48.95 | +40.64 [+40.06, +41.48] |


pVisor has lower fixed management/cache accounting; native Docker has lower instance increments. Reclaimable image caches differ from guest anonymous RAM. A shared existing Docker daemon's entire fixed cost cannot be charged to each new container. Four instances establish neither maximum density nor break-even scale.

### Service preparation and evidence {#preparation}

| Service publication (one observation) | s |
| --- | --- |
| ubuntu-24.04 | 1.81 |
| python-3.12-slim-bookworm | 2.49 |


These are single publications from verified compressed OCI blobs, still resolving the upstream manifest, not first public-network pulls. The published pool occupies 78.42 MiB and retained OCI staging 284.10 MiB on the service side. Client savings do not remove these costs. No matched preparation-speed ranking is available.

[Design/cache boundaries](../design/lazy-image-v2.md#cache-costs) · [Provenance](../assets/benchmarks/lazy-image-v2.provenance.json) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/storage/README.md). Full clocks, request accounting, OCI digests, failed preflights/rejected batches, source/binaries and independent recomputation remain in local experiment directories. Older storage and OverlayBD/NumPy results retain their original scope and do not describe current V2 performance. Real S3/WAN, concurrency, random mmap, copy-up, import peaks and long-term reclamation remain unverified.
