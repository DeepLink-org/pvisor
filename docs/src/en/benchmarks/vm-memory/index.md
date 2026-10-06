# How much memory can an idle VM release, and what does recovery cost?

## Main conclusions {#conclusions}

**For persistent, repeated-content Job snapshots on Linux, compression reduces both charged page cache just after capture and retained storage, at the cost of longer suspension; SDK offload compression saves backing storage, not parked memory.** Choose compression for the storage/cache benefit supported by your data, not as a general live-VM memory or capacity improvement.

Existing frozen public cohorts; N=30 per condition, P50, raw → compressed. The two mechanisms are separate cohorts, not a cross-mechanism comparison.

| Use case | Supported compression outcome | Cost / selection implication |
|---|---|---|
| Retain a repeated-content Job execution snapshot | Just-captured parked cgroup memory **308.43 → 33.28 MiB**, mainly reduced charged page cache; retained Job allocated blocks **122.76 → 41.04 MiB** | Suspend **960.08 → 1082.81 ms**; paired difference +122.73 ms, 95% CI [+111.82, +135.54]. Cache is reclaimable, so this is not a capacity result |
| Park a repeated-content SDK VM | Backing allocated blocks **181.72 → 15.65 MiB**, but parked memory **30.64 → 32.07 MiB** | Offload **45.31 → 427.79 ms**; compression is a storage tradeoff, not additional parked-memory reclamation |
| Reclaim cold pages from a running Linux VM | Current instance-local live compression is experimental | No completed public multi-round cohort supports a numeric memory or recovery claim; see [mechanism and evidence scope](../../design/memory-optimization/compression-local.md) |

These historical measurements have not been revalidated against the newly integrated product. They do not measure a release improvement or establish an advantage over Docker, Firecracker or other runtimes.

## Motivation {#motivation}

VM residency limits how many environments you can retain while waiting for model responses. You also need to know whether memory moves into file cache or a compression pool, and how long the next tool access will wait. Measure parked usage together with complete recovery before choosing suspension or compression.

## Experiment design {#interpretation}

<a id="experiment-design"></a>

B-VM-MEMORY's existing frozen public cohorts use Linux x86_64/KVM to measure SDK whole-VM offload and Job API raw/compressed execution snapshots in separate cohorts. Within each mechanism, repeated/deterministic random data and raw/compressed storage alternate randomly on the same host: 30 fresh VMs and three warmups per condition. Each has two-core affinity/quota, a 2 GiB cgroup, zero swap and a 256 MiB guest. Tools and firmware are prepared; caches are warm without global eviction.

Each VM verifies all 64 MiB of private data and recovered execution state. This is not the entire guest working set: SDK input construction leaves additional copies and mutable state. The primary physical-memory metric is the complete bounded cgroup's `memory.current`, including the VM, collector/recovery processes, backing and charged page cache; anon/file/kernel and CPU counters are retained. SDK active and offloaded points each follow two seconds of settling. Job points follow confirmed running/suspended states. Do not rank the two mechanisms by their absolute values.

Each frozen cohort has 30 samples × four conditions (120 valid formal samples), with no OOM, failures or timing exclusions. SDK resume acknowledgement and the first full scan are separate. Job resume-to-completion includes the remaining guest wait; the guest times its first restored scan separately. CPU is the complete cgroup's phase delta, including control/background work. Single-VM reclamation does not establish parked capacity under pressure. Current macOS automatic cold paging and matching container-pause or Firecracker/QEMU recovery controls have not completed fresh measurement.

Public evidence requires a completed cohort with the declared sample count, frozen source/binary/input provenance, matched resources, full recovery correctness and the predeclared interference gate. Contaminated or incomplete cohorts are ineligible; engineering A/B and diagnostic observations cannot supply public numbers or fill missing conditions. Execution snapshots, SDK offload and automatic live cold-page compression remain distinct mechanisms.

## Data and analysis {#results}

<a id="experiment-data"></a>

### Parked physical memory and storage {#measurements}

N=30 per condition, values are P50. Physical memory is in MiB. Storage sums file `st_blocks × 512`: backing only for SDK, the complete retained Job for snapshots. It is neither logical RAM size nor exclusive disk usage after accounting for reflink sharing.

| Mechanism | Data | Storage | Active physical memory MiB | Parked physical memory MiB | Backing / Job allocated blocks MiB |
|---|---|---|---:|---:|---:|
| SDK offload | repeated | raw | 230.56 | 30.64 | 181.72 |
| SDK offload | repeated | compressed | 388.96 | 32.07 | 15.65 |
| SDK offload | random | raw | 234.85 | 30.63 | 186.97 |
| SDK offload | random | compressed | 406.45 | 32.19 | 144.33 |
| Job execution snapshot | repeated | raw | 142.31 | 308.43 | 122.76 |
| Job execution snapshot | repeated | compressed | 142.40 | 33.28 | 41.04 |
| Job execution snapshot | random | raw | 142.52 | 308.49 | 122.76 |
| Job execution snapshot | random | compressed | 142.36 | 101.98 | 175.01 |

SDK raw and compressed parked usage are close. Compressed active points also charge more file cache, so compression does not establish lower active-VM memory. Job raw parked points charge about 297 MiB of file cache; repeated/random compressed points charge about 21/88 MiB. Cache can be reclaimed under pressure: multiplying this just-captured difference by environment count does not establish capacity. Random compressed Jobs allocate more file blocks than raw; storage benefits depend on the data.

### Parking, recovered access and CPU

N=30 per condition; units are ms. Unsplit distributions show P50; clustered cells show “cluster count/30: cluster median”. The two clusters do not receive one combined P50.

| Mechanism | Data | Storage | Offload / suspend ms | First full scan after recovery ms | Active→parked CPU ms | Parked→restored/completed CPU ms |
|---|---|---|---:|---:|---:|---:|
| SDK offload | repeated | raw | 45.31 | 108.47 | 34.34 | 155.66 |
| SDK offload | repeated | compressed | 427.79 | 13/30: 125.09; 17/30: 266.51 | 412.50 | 13/30: 320.26; 17/30: 515.59 |
| SDK offload | random | raw | 56.87 | 123.65 | 43.05 | 163.68 |
| SDK offload | random | compressed | 479.88 | 714.49 | 449.46 | 919.80 |
| Job execution snapshot | repeated | raw | 960.08 | 151.88 | 827.26 | 392.50 |
| Job execution snapshot | repeated | compressed | 1082.81 | 152.27 | 780.01 | 425.59 |
| Job execution snapshot | random | raw | 1020.27 | 151.65 | 825.33 | 391.85 |
| Job execution snapshot | random | compressed | 1538.23 | 152.17 | 908.76 | 437.94 |

Compressed SDK repeated-data first scans form 13/30 and 17/30 clusters at about 125 and 267 ms. Random-data scans take about 714 ms. Raw SDK warm scans take about 25 ms; compressed warm scans also split (repeated: 25/30 at 25.10 ms and 5/30 at 38.53 ms; random: 20/30 at 25.03 ms and 10/30 at 38.57 ms). The cause is unverified; one median cannot define the recovery budget.

Job warm scans take about 49 ms and first recovered scans about 152 ms. Resume-to-completion is 8.28–8.35 seconds, including roughly eight seconds of remaining guest waiting; this is not eight seconds of recovery work. Complete command time and the first restored scan are different metrics.

The following table contains within-mechanism, matched-condition median differences and paired 95% bootstrap intervals (5,000 resamples). Mechanisms remain separate and no P99 is reported.

| Mechanism | Data | Metric | Compressed minus raw | Paired difference 95% interval |
|---|---|---|---:|---:|
| SDK offload | repeated | parked memory mib | +1.43 | [+1.25, +1.57] |
| SDK offload | repeated | offload ms | +382.48 | [+378.12, +389.59] |
| SDK offload | random | parked memory mib | +1.57 | [+1.39, +1.62] |
| SDK offload | random | offload ms | +423.01 | [+417.79, +427.74] |
| Job execution snapshot | repeated | parked memory mib | -275.15 | [-275.35, -274.94] |
| Job execution snapshot | repeated | suspend ms | +122.73 | [+111.82, +135.54] |
| Job execution snapshot | random | parked memory mib | -206.50 | [-206.65, -206.41] |
| Job execution snapshot | random | suspend ms | +517.96 | [+498.96, +529.49] |

### Evidence compared with existing tools {#linux-lifecycle}

| Tool | Available evidence | What capacity selection still needs |
|---|---|---|
| pVisor Linux SDK offload / Job snapshots | Physical memory, storage, phase CPU and complete restored data, with mechanisms separate | A complete recoverable parked-capacity sweep under pressure |
| Podman / Docker | [Active-task capacity](../density.md), including matched-budget Podman controls | Pause/recovery capacity for the same private working set; active capacity is different |
| Firecracker / QEMU | [Startup and developer tools](../compare-runtimes.md) | Parked memory and recovery cost for the same working set |
| pVisor macOS automatic cold-page pool | Fresh current-implementation physical-memory measurement is incomplete | Whole-system or bounded-group accounting including pool, backing and recovery CPU |

### Downloads and reproduction {#run}

The same-directory CSVs retain the existing frozen cohorts separately, with report and binary digests linked to source/build and input receipts in the provenance CSV. All numeric tables above use only those cohorts; no samples from newly integrated implementations are pooled with them.

[Memory and cost CSV](memory-summary.csv) · [Paired comparisons CSV](memory-comparisons.csv) · [Source, inputs and evidence summary](memory-provenance.csv) · [Evidence source summary](../evidence-sources.csv) · [Comparison method](../methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
