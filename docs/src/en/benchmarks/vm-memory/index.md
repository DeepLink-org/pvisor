# How much physical memory does a 512 MiB VM use?

## Conclusions {#conclusions}

**Configuring 512 MiB does not immediately consume 512 MiB on the host. With the 64 MiB application working set in these static tests, the default VM and management processes resident footprint was about 178–182 MiB; memory offload reduced it to about 35 MiB, saving 80.1–80.5% per instance; compressed offload reduced it to about 40 MiB, saving 77.6–78.2%. Product-group memory including file cache fell from about 220–224 MiB to 19–25 MiB.** Each condition was measured once, under this Linux configuration and short observation window.

| Your choice | What these readings show |
|---|---|
| Memory offload | Memory offload saves 80.1–80.5% resident memory per instance; cache-inclusive group usage also falls, and the next tool must recover RAM |
| Compressed offload | Offload saves 77.6–78.2% resident memory per instance; saves more disk backing, with parked residency about 40 MiB |
| Idle release | Saves 33.9–35.2% residency per instance while the host retains file cache |
| Memory compression | Per-instance residency increases by 183.8–209.8%; these static readings show no savings |
| Share across instances | Four VMs configured with 512 MiB each save 58.7–60.3% average per-instance residency with identical contents, or 14.5% with unique random contents; savings after complete rewrites are 13.0–15.1% |

## Motivation {#motivation}

512 MiB is the guest's available capacity, not physical memory already consumed by the VM on the host. Choose compression, offload or sharing from actual host usage and the next tool's recovery cost. Guest free memory and compression ratios alone cannot establish how much host RAM was saved.

## Experiment design {#interpretation}

<a id="experiment-design"></a>

Every VM has **512 MiB guest RAM, 2 vCPU and 64 MiB application data**. Working sets stay identical: usage depends on the pages touched, so the default VM is not assumed to consume all 512 MiB.

**Idle release** has the application actively free the test working set while the VM keeps running; the application regenerates the data when needed again. **Memory offload** pauses the VM, retains RAM state in uncompressed backing and unloads resident RAM; execution continues after restoration. **Compressed offload** stores the same state in compressed backing, saving storage at the cost of compression and decompression.

- Single instances compare default running, idle release, memory compression and memory/compressed offload. Repeated and deterministic random data cover compressible and difficult-to-compress contents; default/memory compression also test distinct compressible pages and a 16 MiB hot region plus 48 MiB cold region.
- Multiple instances restore four VMs from one sealed snapshot, comparing a shared baseline with independent-inode copies of identical bytes. Check 25%/100% working-set writes and independent termination. KSM on/off has a separate dynamic-private-page comparison.
- One fresh VM or group per condition, no warmups. Single instances have a five-second observation window; multi-instance scan windows are two seconds. No P50/P95, confidence interval or eventual-convergence claim.

**Resident physical memory** covers the complete product group's VM, management processes, compressed store and helpers, using summed process PSS with shared pages counted proportionally. **Host-group memory** uses complete cgroup physical-memory charging, including unmapped file cache and kernel costs charged to that group. These metrics cannot be added: residency describes actual mapped physical pages, while group usage keeps retained cache visible; shared-page attribution also differs. Neither establishes exclusive incremental whole-host cost or production capacity.

Linux x86_64/KVM on an AMD Ryzen 7 9700X, host kernel 7.2.8-200.fc44.x86_64. The complete group uses logical CPUs 0–3, a four-core quota, a 2 GiB cap and zero swap. Coordinator, observer and global KSM thread remain outside it. Inputs are prepared without global cache eviction; background compilation is recorded, so durations are single observations. All 28 conditions passed data, write-isolation, termination and provenance checks without OOM. Some independent-baseline conditions reached the 2 GiB group cap and experienced cache reclaim.

The commands below configure a 512 MiB VM and enable each feature. Replace `JOB`, `SOCKET` and `ATTEMPT` with startup output identities. Memory offload starts from an ordinary VM; compressed offload requires compressed backing at startup. Memory compression requires userfaultfd permission and compressed backing requires FUSE.

```bash
# Default 512 MiB VM
pvisor run --executor vm --memory 512MiB -- claude

# Memory compression
pvisor run --executor vm --memory 512MiB --vm-cold-ram-compression -- claude

# Compressed backing for later offload
pvisor run --executor vm --memory 512MiB --vm-ram-compression -- claude

# Use the identities printed at startup
pvisor suspend JOB --vm-socket SOCKET --vm-job-id JOB --vm-attempt-id ATTEMPT --vm-offload
pvisor resume JOB --vm-socket SOCKET --vm-job-id JOB --vm-attempt-id ATTEMPT --vm-load

# KSM advice: restored private COW RAM is eligible; fresh shared RAM is skipped
pvisor run --executor vm --memory 512MiB --vm-ram-dedup -- claude
```

Fresh VMs' shared RAM is skipped by this KSM advice; restored private COW RAM is eligible for the tested path. Baseline-sharing results also do not describe four independent `run` commands. Memory compression cannot combine with RAM deduplication, and restored private COW RAM does not support the live offload path above. See [control options](../../reference/cli.md#vm-instance-control) and [deduplication advice](../../reference/cli.md#vm-ram-dedup).

## Data and analysis {#results}

<a id="experiment-data"></a>

### One 512 MiB VM {#measurements}

N=1 per condition, sampled five seconds after the operation, in MiB. Savings = (default residency − current residency) ÷ default residency × 100%, using default running with the same working set and unrounded readings. The 512 MiB configured capacity is not the denominator.

| Mode | Configured capacity | Resident physical memory<br>Repeated / random contents | Per-instance resident savings<br>Repeated / random contents | Host-group memory, including cache<br>Repeated / random contents |
|---|---:|---:|---:|---:|
| Default running | 512 | 178.0 / 182.1 | Baseline | 220.2 / 224.1 |
| Idle release | 512 | 117.7 / 118.0 | Save 33.9% / 35.2% | 223.5 / 219.9 |
| Memory compression | 512 | 505.2 / 564.3 | Increase 183.8% / 209.8% | 489.9 / 548.6 |
| Memory offload | 512 | 35.4 / 35.5 | Save 80.1% / 80.5% | 19.4 / 19.1 |
| Compressed offload | 512 | 39.8 / 39.7 | Save 77.6% / 78.2% | 24.8 / 24.4 |

**Repeated contents**: the 64 MiB application data repeats a fixed byte sequence and is easy to compress, testing savings with repetitive contents. **Random contents**: the same amount of data uses a fixed-seed pseudorandom sequence and is difficult to compress, testing savings with little repetition. Both working sets have the same size; the header order maps to the left and right values in each cell.

Idle release lowered residency to about 118 MiB, while cache-inclusive usage remained about 220 MiB. It therefore does not establish an equal net host-RAM reclaim. Memory offload lowers both readings, providing clearer savings.

The memory-compression group began at about 564 MiB resident, falling to 505.2 MiB for repeated contents after five seconds. That within-mode change cannot replace comparison with the default's 178.0 MiB. Distinct compressible pages used 509.2 MiB and the hot/cold mix 517.8 MiB, versus default residency of 177.2 / 179.9 MiB. The 512 MiB setting limits guest capacity; management and compression outside the VM also consume host RAM, so residency can exceed configured capacity.

### Offload storage and recovery cost

N=1 per condition, with durations in ms. Compare against a default VM that remains loaded and executes the same test task after the same five-second idle window. Task duration starts when the host initiates the operation and ends after the 64 MiB digest, write/fsync/readback check; offloaded cases include RAM restoration. Added duration = post-offload task duration − the loaded baseline's task duration for the same working set, calculated from unrounded readings. These are single observed differences under background compilation, not stable latency guarantees.

| Mode | Task without offload ms<br>Repeated / random contents | Restore and complete task after offload ms<br>Repeated / random contents | Added duration ms<br>Repeated / random contents |
|---|---:|---:|---:|
| Memory offload | 299.6 / 333.2 | 444.7 / 388.2 | +145.0 / +54.9 |
| Compressed offload | 299.6 / 333.2 | 621.6 / 642.2 | +322.0 / +308.9 |

The offload operation happens when entering the idle state and is reported separately below. Backing measures allocated disk blocks in MiB.

| Mode | Offload operation ms<br>Repeated / random contents | Backing MiB<br>Repeated / random contents |
|---|---:|---:|
| Memory offload | 41.1 / 103.4 | 186.3 / 186.7 |
| Compressed offload | 749.5 / 924.4 | 15.5 / 146.1 |

Compressed backing saves storage for repeated contents but requires encoding, decoding and transient memory. Do not set the complete task's memory cap from its parked reading. CSVs retain peaks, phase CPU and complete measurement scope.

### Four VMs with 512 MiB each: shared snapshot baseline {#linux-lifecycle}

This measures pVisor restoring four VMs from the same snapshot, sharing the read-only RAM baseline and isolating modifications through copy-on-write (COW). The independent control uses four separate files containing identical snapshot bytes. Neither arm enables KSM advice or connects to an external shared compressed cold-page pool.

Total configured capacity is 2048 MiB. Per-instance residency is complete-group residency divided by four, including allocated helper overhead and proportional shared pages. N=1 per condition, in MiB. Savings use the independent baseline with the same working set and phase, calculated from unrounded readings.

| Working set | Independent snapshot residency per instance | Shared snapshot residency per instance | Per-instance savings | Per-instance savings after 100% writes |
|---|---:|---:|---:|---:|
| Identical repeated data | 104.7 | 41.6 | 60.3% | 13.8% |
| Identical random data | 104.2 | 43.1 | 58.7% | 13.0% |
| Unique random data per VM | 106.9 | 91.5 | 14.5% | 15.1% |

For example, identical repeated data reduces average per-instance residency from 104.7 MiB to 41.6 MiB, saving 60.3%. After complete rewrites, independent/shared group totals are 426.4 / 367.4 MiB, or 106.6 / 91.8 MiB per instance, saving 13.8%. Post-write savings use the post-write independent baseline.

Identical contents retain the most sharing. Unique random contents already add private pages during preparation, and writes reduce the benefit. Repeated-data independent/shared groups had 2035.7 / 841.2 MiB cache-inclusive usage, distinct from their 418.8 / 166.4 MiB residency. Baseline storage cache consumes RAM, but it cannot all be called the VM's resident working set, nor can its ratio directly predict runnable VM counts.

KSM is a separate comparison on dynamic private pages produced after restoration. Residency with KSM advice off/on was 363.5 / 362.1 MiB for repeated data, 367.8 / 361.3 MiB for identical random data and 366.1 / 365.0 MiB for unique random data. Two seconds and one sample cannot confirm an additional benefit or prove KSM ineffective. The external shared compressed cold-page pool (`--vm-memory-pool`) has no measurements for this 512 MiB configuration. The current Linux pager supports instance-local compressed storage only; the external pool requires Apple Silicon macOS. See the [experimental cold-page pool](../../design/memory-optimization/proof-of-concept.md#v1-integration). These data do not establish long-term stability, real Agent latency, macOS or other-runtime density comparisons.

### Data and reproduction {#run}

[Single-instance readings](memory-choices.csv) · [Single-instance observed differences](memory-choices-comparisons.csv) · [Single-instance provenance](memory-choices-provenance.csv) · [Multi-instance readings](memory-sharing.csv) · [Multi-instance observed differences](memory-sharing-comparisons.csv) · [Multi-instance provenance](memory-sharing-provenance.csv) · [Reproduction commands](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#user-memory-saving-choices)

These CSVs use only the new 512 MiB static cohorts, retaining configuration, N=1, resident/cache scope and source, binary, harness and input-digest links. Original evidence remains local under `.data/`, separate from earlier 256 MiB or historical snapshot results.
