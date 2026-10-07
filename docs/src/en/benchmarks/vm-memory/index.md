# How much physical memory does a 512 MiB VM use?

## Conclusions {#conclusions}

**Configuring 512 MiB does not immediately consume 512 MiB on the host. With the 64 MiB application working set in these static tests, the default VM and management processes resident footprint was about 178–182 MiB; memory offload reduced it to about 35 MiB, saving 80.1–80.5% per instance; compressed offload reduced it to about 40 MiB, saving 77.6–78.2%. Product-group memory including file cache fell from about 220–224 MiB to 19–25 MiB.** Each condition was measured once, under this Linux configuration and short observation window.

| Your choice | What these readings show |
|---|---|
| Memory offload | Memory offload saves 80.1–80.5% resident memory per instance; cache-inclusive group usage also falls, and the next tool must recover RAM |
| Compressed offload | Offload saves 77.6–78.2% resident memory per instance; saves more disk backing, with parked residency about 40 MiB |
| Idle release | Saves 33.9–35.2% residency per instance while the host retains file cache |
| Memory compression | Per-instance residency increases by 183.8–209.8%; these static readings show no savings |
| Shared snapshot + COW | Relative to independent fresh VMs, identical contents save 68.5–69.0% average per-instance residency; unique random contents save 30.9%. Savings after complete rewrites remain 30.8–32.7% |
| KSM | After 60 seconds, per-instance savings are 63.8% for repeated contents, 51.8% for identical random contents and 15.1% for unique random contents |
| Daemon memory pool | Connected to real VMs; about 537–546 MiB resident per instance, an increase of 302.9–309.9%; no savings in this run |

## Motivation {#motivation}

512 MiB is the guest's available capacity, not physical memory already consumed by the VM on the host. Choose compression, offload or sharing from actual host usage and the next tool's recovery cost. Guest free memory and compression ratios alone cannot establish how much host RAM was saved.

## Experiment design {#interpretation}

<a id="experiment-design"></a>

Every VM has **512 MiB guest RAM, 2 vCPU and 64 MiB application data**. Working sets stay identical: usage depends on the pages touched, so the default VM is not assumed to consume all 512 MiB.

**Idle release** has the application actively free the test working set while the VM keeps running; the application regenerates the data when needed again. **Memory offload** pauses the VM, retains RAM state in uncompressed backing and unloads resident RAM; execution continues after restoration. **Compressed offload** stores the same state in compressed backing, saving storage at the cost of compression and decompression.

- Single instances compare default running, idle release, memory compression and memory/compressed offload. Repeated and deterministic random data cover compressible and difficult-to-compress contents; default/memory compression also test distinct compressible pages and a 16 MiB hot region plus 48 MiB cold region.
- Multiple instances compare unshared VMs, shared snapshot + COW, KSM and the daemon memory pool. The unshared arm boots four independent VMs with private anonymous RAM; the shared arm restores from one sealed snapshot; the KSM arm uses the same RAM mappings with dedup advice enabled. The pool arm boots independent VMs with compressed objects held by the daemon pool component; identical cold chunks are stored once and restored into instance-private RAM on faults. Working sets match, with 25%/100% writes and independent termination checks.
- One fresh VM or group per condition, no warmups. Single instances have a five-second observation window. KSM waits 60 seconds before its initial reading; the other multi-instance strategies wait two seconds. No P50/P95, confidence interval or eventual-convergence claim.

**Resident physical memory** covers the complete product group's VM, management processes, compressed store and helpers, using summed process PSS with shared pages counted proportionally. **Host-group memory** uses complete cgroup physical-memory charging, including unmapped file cache and kernel costs charged to that group. These metrics cannot be added: residency describes actual mapped physical pages, while group usage keeps retained cache visible; shared-page attribution also differs. Neither establishes exclusive incremental whole-host cost or production capacity.

Linux x86_64/KVM on an AMD Ryzen 7 9700X, host kernel 7.2.8-200.fc44.x86_64. The complete group uses logical CPUs 0–3, a four-core quota and zero swap. Single-instance groups have a 2 GiB ceiling; all four multi-instance strategies use the same 4 GiB ceiling. Coordinator, observer and global KSM thread remain outside it. Inputs are prepared without global cache eviction; background compilation is recorded, so durations are single observations. The 16 single-instance conditions and 12 new multi-instance conditions come from separate static cohorts. All passed data, write-isolation, termination and provenance checks without OOM.

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

# KSM: without an explicit backing, Linux uses private anonymous RAM
pvisor run --executor vm --memory 512MiB --vm-ram-dedup -- claude
```

Private anonymous RAM and restored private COW RAM accept KSM advice; writable shared RAM backing is skipped. On Linux, dedup without an explicit backing selects private anonymous RAM for fresh VMs; this path does not support whole-VM offload. The benchmark also verifies accepted advice for every VM and mergeable RAM mappings: command acceptance alone does not prove that pages participate in scanning. Baseline-sharing results also do not describe four independent `run` commands. Memory compression cannot combine with RAM deduplication, and restored private COW RAM does not support the live offload path above. See [control options](../../reference/cli.md#vm-instance-control) and [deduplication advice](../../reference/cli.md#vm-ram-dedup).

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

### Four VMs with 512 MiB each: strategy comparison {#linux-lifecycle}

Total configured capacity is 2048 MiB. Unshared means four independently booted VMs. Shared snapshot + COW maps one immutable RAM file and creates instance-private pages on writes. KSM enables deduplication advice on independently booted VMs. The daemon pool arm uses private anonymous RAM and shares compressed, deduplicated objects in the pool process; access restores VM-private pages. Only the KSM arm enables KSM advice.

Per-instance residency = complete-product group PSS divided by four, including allocated helper and daemon pool component overhead. N=1 per condition, in MiB. KSM waits 60 seconds before its initial reading; other strategies wait two seconds. Readings after 25%/100% writes keep the same short sampling procedure, without another 60-second wait. Savings use the unshared baseline with the same working set and phase, calculated from unrounded readings.

| Working set | Strategy | Residency per instance MiB | Per-instance savings | Residency per instance after 100% writes MiB | Post-write per-instance savings |
|---|---|---:|---:|---:|---:|
| Identical repeated data | Unshared | 133.4 | Baseline | 135.0 | Baseline |
| Identical repeated data | Shared snapshot + COW | 41.3 | 69.0% | 90.8 | 32.7% |
| Identical repeated data | KSM | 48.3 | 63.8% | 112.6 | 16.6% |
| Identical repeated data | Daemon memory pool | 537.2 | Increase 302.9% | 517.2 | Increase 283.0% |
| Identical random data | Unshared | 132.8 | Baseline | 133.8 | Baseline |
| Identical random data | Shared snapshot + COW | 41.9 | 68.5% | 92.6 | 30.8% |
| Identical random data | KSM | 64.0 | 51.8% | 112.3 | 16.1% |
| Identical random data | Daemon memory pool | 540.3 | Increase 307.0% | 512.1 | Increase 282.7% |
| Unique random data per VM | Unshared | 133.2 | Baseline | 133.9 | Baseline |
| Unique random data per VM | Shared snapshot + COW | 92.0 | 30.9% | 92.2 | 31.1% |
| Unique random data per VM | KSM | 113.0 | 15.1% | 113.5 | 15.2% |
| Unique random data per VM | Daemon memory pool | 545.9 | Increase 309.9% | 508.8 | Increase 280.0% |

Shared snapshot + COW retains both common system state and unchanged application pages. After completely rewriting the 64 MiB application working set, system pages may still remain shared. Unique random contents already create private pages during preparation, reducing initial savings.

KSM produced actual merged pages during the 60-second window. Per-instance residency was 48.3 MiB for repeated contents, 64.0 MiB for identical random contents and 113.0 MiB for unique random contents. It merges identical pages within and across instances; unique random data must remain separately stored, reducing savings. Complete rewrites break existing sharing, leaving about 15–17% savings in the short post-write observation; these readings do not wait another 60 seconds for new pages to merge.

The earlier KSM startup path used shared RAM mappings and skipped all dedup advice, so its result could not establish a lack of KSM benefit. This run uses private anonymous RAM for both KSM and the unshared control, and verifies accepted advice and mergeable mappings for every VM. The 60-second window does not establish eventual convergence or stable production savings.

**The daemon memory pool saves no memory in this short static test.** Initial encoded pool data is about 11.2 MiB for repeated contents or 63.8–86.7 MiB for random contents, which does not represent whole-group usage. The cold-page path currently prefaults all guest RAM; reclaim in this short window does not offset that cost. The four-VM/pool startup peak measured about 2.16 GiB, so all four strategies use a 4 GiB group ceiling. Pool scanning, encoding and fault restoration consume CPU, and pool-process loss fails dependent VMs.

Append `--memory-pool` to `pvisor-daemon serve` to enable it; the default is off. The pool runs separately from the API process and retains objects across API restart. See [the daemon guide](../../guides/daemon/index.md#memory-pool) for configuration and budgets. This run measures the real daemon pool component and VM path; it does not start the complete OpenSandbox API/SDK data plane or establish long-term stability, real Agent latency or production density.

### Data and reproduction {#run}

[Single-instance readings](memory-choices.csv) · [Single-instance observed differences](memory-choices-comparisons.csv) · [Single-instance provenance](memory-choices-provenance.csv) · [Multi-instance readings](memory-sharing.csv) · [Multi-instance observed differences](memory-sharing-comparisons.csv) · [Multi-instance provenance](memory-sharing-provenance.csv) · [Reproduction commands](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#user-memory-saving-choices)

These CSVs use only the new 512 MiB static cohorts, retaining configuration, N=1, resident/cache scope and source, binary, harness and input-digest links. Original evidence remains local under `.data/`, separate from earlier 256 MiB or historical snapshot results.
