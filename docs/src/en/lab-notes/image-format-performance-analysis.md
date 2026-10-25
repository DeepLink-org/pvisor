# pVisor V2 versus native Docker: image storage, startup and memory

## Motivation {#motivation}

pVisor V2 substantially reduces disk inventories for these real images; native Docker containers start faster and have lower per-instance memory increments for the lightweight shell workload. V2 offers compressed image delivery and a separate VM boundary per instance. Image compression does not establish superiority on every runtime metric. This record retains an independent local engineering cohort; see the [lazy-image benchmark](../benchmarks/lazy-image-startup.md) for current complete-delivery, remote-fetch and repeated-local-mmap results. Cohorts are not pooled.

Independent inventories of four pinned images save 71%–73%, and their actual shared store saves 74%. Prepared Ubuntu Ready medians are 80.69 ms for Docker and 106.38 ms for the pVisor VM. With four instances, baseline-subtracted increments are 8.46 MiB and 48.83 MiB per instance. Complete deployment accounting also includes image caches and daemon fixed costs; totals alone cannot establish per-instance cost.

## Experiment design {#interpretation}

Measure same-image allocated files, launch-to-first-checked-output waiting and whole-product-group memory as disk, startup and deployment-memory costs. These metrics describe local operation after image preparation. Lower values mean less required disk/memory budget or shorter waits for short tasks.

2026-10-25, Linux x86_64/KVM, AMD Ryzen 7 9700X, kernel 7.2.11-300.fc45. A V2 release/musl CLI frozen before rebasing onto main is compared with native Docker Engine 29.7.2/runc, without OpenSandbox, E2B, execd or an additional API server. A Linux namespace container and an independent pVisor guest kernel provide different isolation mechanisms; no additional physical-memory pool is enabled.

Capacity and runtime are separate cohorts, never pooled. Capacity uses pinned linux/amd64 manifests for Alpine 3.20, Ubuntu 24.04 and Python 3.12/3.13 slim-bookworm. Manifest, config and every compressed-layer SHA-256 are checked, alongside Docker RootFS diff_ids and pVisor catalog identity. Each single-image and actual four-image inventory has three new private stores. Docker is independent rootless/containerd overlayfs, including normal layer sharing, compressed blobs, unpacked snapshots and metadata; its empty inventory is reported separately and subtracted. pVisor includes its complete Docker-profile store: compressed content, indexes, config and catalog, without retained unpacked trees/layer archives. Import sources, tools, binaries and runtime writable layers are excluded.

Inventories are captured before runtime validation. Hardlinked inodes count once, symlinks are not followed, and unreadable subtrees fail instead of disappearing from totals. Both experiment directories disable Btrfs transparent compression without changing global host configuration. Allocation is `st_blocks × 512`, not exclusive device bytes, and excludes global filesystem metadata. Each runtime validates matching `/etc/os-release` and `/bin/sh` hashes; repeated pull adds no pool content.

Startup uses the same Ubuntu digest with images and a private daemon prepared. Every instance is new: 3 warmup pairs and 30 seed-819 randomized pairs with retained host page caches, excluding image fetch and daemon startup. Both execute the same shell: check a private file is absent, write/read a token, emit one exact Ready line, then exit normally. Clocks include the same small cgroup launcher, CLI creation and output pipe; semspec starts immediately before Popen, stops Ready on receipt and records normal exit separately. This is not index Open, cold disk or SDK creation. All valid slow samples remain; 30 samples do not establish P99.

Runtime groups have a 4-CPU quota, 4 GiB and zero swap; clients/services use CPUs 0–3. Without delegated cpuset, the same image's taskset sets the container's host CPU mask and the one-vCPU VM's guest CPU 0 before the workload. Window witnesses verify live member affinity, not every transient init thread's past affinity. Both request 1 CPU/256 MiB, but guest RAM and container memory limits have different scopes and do not establish equal isolation or application-available RAM.

Memory independently uses five randomized pairs of new product groups, one arm at a time. All dockerd, rootlesskit/pasta, containerd, shim/container or supervisor/VMM processes are included. Image import finishes within that group first; the empty baseline includes its caches/residual management costs and the identical delegated-leaf keeper in both arms. Zero/one/four-instance stages each last three seconds, sampled every 200 ms; the final two seconds form a stage median, then five groups are summarized. Live owners/VMMs/container PIDs, actual cgroup limits/membership, zero OOM/swap, private tokens and native removal are checked. Failed preflights remain separate. The observer is outside; shared cache precharged outside the group is excluded, so this is not whole-host added physical memory.

## Data and analysis {#results}

### Real-image disk inventories {#disk}

MiB (2²⁰ bytes), medians of N=3 fresh inventories; the final column shows all three ranges. Docker is the image increment after subtracting empty storage.

| Inventory | Docker MiB | pVisor V2 MiB | Reduction | Docker / V2 min–max MiB |
| --- | --- | --- | --- | --- |
| alpine-3.20 | 12.53 | 3.59 | 71.3% | 12.52–12.53 / 3.59–3.59 |
| ubuntu-24.04 | 110.18 | 31.64 | 71.3% | 110.18–110.18 / 31.64–31.64 |
| python-3.12-slim-bookworm | 176.68 | 47.15 | 73.3% | 176.68–176.68 / 47.15–47.15 |
| python-3.13-slim-bookworm | 172.34 | 46.18 | 73.2% | 172.34–172.34 / 46.18–46.18 |
| combined | 363.16 | 94.29 | 74.0% | 363.16–363.16 / 94.29–94.29 |


![Allocated real-image and shared-store inventories](../assets/benchmarks/image-format-capacity.svg)

The combined inventory uses one genuinely shared store. Docker already reuses common layers; adding four independent inventories is not the comparison. The breakdown below reports median native blob/snapshot storage and empty baselines in MiB. Main totals also include databases/other metadata, so columns need not sum exactly.

| Inventory | Docker blobs MiB | Docker snapshots MiB | Docker empty MiB |
| --- | --- | --- | --- |
| alpine-3.20 | 3.47 | 9.05 | 0.24 |
| ubuntu-24.04 | 28.40 | 81.78 | 0.24 |
| python-3.12-slim-bookworm | 43.19 | 133.45 | 0.24 |
| python-3.13-slim-bookworm | 42.17 | 130.13 | 0.24 |
| combined | 90.29 | 272.75 | 0.24 |


V2 saves capacity through compressed content/indexes, directory small-file packing and content reuse. Results cover these images and Docker 29/containerd overlayfs, not every image or storage driver. They exclude import peaks and establish no mmap, read-throughput or GC advantage.

### Native startup with prepared images {#startup}

Milliseconds, medians of N=30 pairs. Differences are median paired pVisor−Docker with 95% intervals from 5,000 paired bootstrap draws. The difference of marginal medians need not equal the median paired difference.

| Metric | Docker ms | pVisor VM ms | Paired difference [95% CI] ms |
| --- | --- | --- | --- |
| First checked output Ready | 80.69 | 106.38 | +24.67 [+22.63, +26.21] |
| Launch to normal exit | 109.98 | 146.64 | +34.23 [+30.66, +36.03] |


![Every paired first-output and normal-exit sample](../assets/benchmarks/image-format-latency.svg)

Every point is one formal sample; lines join pairs and boxes show quantile ranges without hiding slow samples. Native Docker is faster for this workload. Compact indexes and compressed pools do not remove complete VM creation costs. These results cannot be mixed with historical SDK/service-adapter comparisons or attributed solely to an index algorithm.

### Complete deployment memory and per-instance increments {#memory}

MiB, medians of N=5 fresh product groups with min–max in brackets. Differences are paired-group median differences and bootstrap 95% intervals. Each per-instance increment is computed as `(stage total−same-group empty baseline)/instance count` before taking medians, never by subtracting overall P50s.

| Stage | Docker MiB | pVisor VM MiB | Paired difference [95% CI] MiB |
| --- | --- | --- | --- |
| No instance, images prepared | 211.47 [208.08, 212.85] | 35.36 [34.44, 35.57] | -176.27 [-178.41, -172.68] |
| Service and 1 idle instance | 219.44 [215.38, 220.75] | 84.14 [83.68, 84.37] | -135.07 [-137.00, -131.12] |
| Service and 4 idle instances | 245.33 [242.00, 247.25] | 230.67 [230.36, 231.05] | -14.66 [-16.19, -11.32] |
| 1 instance: baseline-subtracted per instance | 7.86 [7.30, 7.97] | 48.86 [48.32, 49.30] | +41.20 [+40.46, +41.56] |
| 4 instances: baseline-subtracted per instance | 8.46 [8.09, 8.93] | 48.83 [48.70, 49.15] | +40.42 [+39.90, +40.74] |


![Deployment totals, instance increments and memory categories](../assets/benchmarks/native-docker-memory.svg)

Complete accounting includes the daemon, image caches, anonymous memory and kernel overhead, not `docker stats`, configured 256 MiB or individual RSS. Lower pVisor fixed inventory/management costs can lower group totals while native containers retain lower instance increments. When sharing an existing Docker daemon, its entire fixed cost cannot be assigned to every new container. Four instances establish neither maximum density nor a break-even scale.

Below are five-group medians from this cohort's `memory.stat`, in MiB. Categories and total accounting are read separately and may differ slightly.

| Backend/stage | anon MiB | file MiB | kernel MiB |
| --- | --- | --- | --- |
| docker / empty | 90.47 | 109.70 | 8.91 |
| pvisor / empty | 0.11 | 31.64 | 1.90 |
| docker / four | 116.65 | 110.22 | 13.32 |
| pvisor / four | 190.46 | 31.83 | 6.93 |


Image file caches are generally reclaimable and are not equivalent to guest anonymous RAM as long-term occupancy. Capacity planning must consider anon/file/kernel and instance increments together; group `memory.current` differences do not show that every real application uses less physical memory. Only idle shell/sleep is measured, without large shared/unique working sets, memory pressure or long-term stability results.

### Provenance and reproduction {#evidence}

Both sides' 15 capacity inventories and every formal startup/memory group pass identity, content and lifecycle checks. Native Docker/runc is the comparator. Full manifests, per-file inventories, CPU/memory witnesses, paired clocks, failed preflights, frozen sources and artifacts remain under `benchmark/storage/.data/native-docker-storage-formal-20261025/` and `native-docker-runtime-formal-20261025/`, outside the published site. Native Docker is the comparator throughout; internal format regressions remain local evidence.

The pinned platform manifests follow; full config/layer receipts are in batch inputs.json.

| Image | linux/amd64 manifest |
| --- | --- |
| alpine-3.20 | `sha256:c64c687cbea9300178b30c95835354e34c4e4febc4badfe27102879de0483b5e` |
| ubuntu-24.04 | `sha256:a2d43bffc46670d7108bb3c0468c67c5557c4c92e83e1e34cbff1322ad1b94dc` |
| python-3.12-slim-bookworm | `sha256:57d96f71de145a98e7ceca18ee4ade77bba4103a33a72dfaea026b23625c6bf0` |
| python-3.13-slim-bookworm | `sha256:4cea12ab7d78a3312c85f8cab19b6971d4fe9704da25443986c0bc20a6678850` |


| Artifact | SHA-256 |
| --- | --- |
| release CLI | 6f83d7e7e872761df9563137496bc5cb1286c842bd74b37c2ed28a0cd2c722fe |
| disk summary | eed2086cb76fa1d10ecdfa382cc7320583d9abd2593ba26aed822807068c2041 |
| startup summary | 3e16093d217c8a371d8bf9d171b7c2218edce9ca4e68e1396f731538fc7ba622 |
| memory summary | 70100c63051c078eaf5cb16f461ba7503b3ac3a3c875f6e74c9ec955a5f05376 |


Experiments are registered separately as B-IMAGE-STORAGE-ENG and B-NATIVE-DOCKER-ENG. See the [storage experiment manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/storage/README.md) for commands/prerequisites. Runner link/permission-boundary tests and benchmark registration/documentation contracts pass. No storage implementation, semantic approval ledger or snapshot is changed.
