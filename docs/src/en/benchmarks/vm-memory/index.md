# How can multiple VMs use less memory?

## Main conclusions {#conclusions}

Agent tools load dependencies, read files and process data, and multiple environments may use identical contents at similar times. **pVisor shares duplicate pages early and retains that sharing on subsequent reads, reducing the memory resident across these execution environments.**

- **Establish sharing early.** In the shared-random preflight, the physical pool reaches the observed sustained 128 MiB drop at about 1.7 seconds, providing evidence of early reclamation for short-lived working sets.
- **Carry sharing through the next read.** Physical sharing retains low usage after readback. Explicit immutable files can also be reused directly through DAX, while file-read hints help the scanner discover candidates earlier.
- **Evaluate the complete cost for the working set.** Savings depend on duplication and page lifetime. Consider scanning CPU, end-of-window memory usage and the cost of restoring private RAM from compressed storage together.

## Motivation {#motivation}

Agent file reads, searches and small script calls can be brief. For short-lived tool environments or working sets that are soon released or overwritten, low memory usage after tens of seconds may arrive after the sharing opportunity has passed; both the first few seconds and the end of the window matter. Library pages in a persistent VM can survive multiple calls, so tool duration is not a direct measure of how long pages remain shareable. Deployment decisions should use the working set's lifetime to balance reclamation speed, scanning CPU, readback costs and the complete runtime group's memory budget.

## Experiment design {#interpretation}

Evaluate deduplication with compressible repeated contents, identical random contents across instances and instance-unique random contents. Use the same observation window to compare idle usage, reclamation speed, CPU and first complete readback. Evaluate library loading, file-read hints and explicit immutable DAX separately to avoid combining different workloads into one ranking.

The engineering preflights use four instances per group with 64 MiB of data each. VMs have 256 MiB RAM / 1 vCPU; each Docker container has a 256 MiB limit and one CPU quota. Complete groups have four CPUs / 2 GiB / zero swap and include the pool or dedicated rootless Docker runtime. Each condition has N=1 and a 60-second ready window. Content, private-write, exit and resource checks must pass; failed and incomplete cohorts are excluded. See the [engineering experiment design](../../design/vm-memory-performance-analysis.md#pool-design) for the environment and sampling boundaries.

This compares complete configurations under common budgets and workloads. Docker uses a dedicated rootless daemon whose runtime costs are borne by four containers; a shared-daemon deployment can amortize them differently. Firecracker and pVisor use different guest kernels and virtio-blk / virtio-fs file paths, while Docker shares the host kernel. Global ksmd runs outside the group's CPU quota and is reported separately, so the results cannot all be attributed to the scanning algorithm.

**Cgroup-charged memory** includes cache and kernel costs charged to the group. **PSS** proportionally allocates shared pages mapped by processes. File pages may be charged outside the group, so retain both metrics separately; do not add them or directly convert them into incremental whole-host usage. Repeated rounds, real-Agent latency and long-term capacity remain unmeasured.

### Whole-group totals and per-VM averages {#memory-accounting}

**The figures show totals for all four instances, without dividing by the VM count.** Daemon-backed modes include the `pvisor-daemon memory-pool` process, all VM processes and in-group helper processes. To calculate average usage per VM:

- **Average process PSS** = (pool-process PSS + all VM-process PSS + in-group helper-process PSS) / 4. PSS proportionally allocates shared pages across mappings; summing RSS would count shared pages repeatedly.
- **Average group-charged memory** = the complete cgroup's `memory.current` / 4. This already includes the pool, file cache and kernel costs charged to the group; do not add daemon memory again.

This average allocates the complete group cost and does not measure the marginal cost of another VM. The experiment starts the standalone memory-pool component, without the full OpenSandbox API service. External observers are also excluded from these memory totals.

### Distinguishing deduplication from compression {#dedup-modes}

Physical page sharing retains one copy during reads; cold compression restores private memory on access. Evaluate idle usage, reclamation speed, scanning CPU and memory after the first complete read together.

| Experimental label | Memory mechanism | Cross-VM reuse | On subsequent reads |
|---|---|---|---|
| `off` | Extra RAM deduplication and cold compression disabled | No active RAM merging | Read existing pages |
| `ksm` | Host kernel scans and merges identical pages | Kernel-managed shared pages | Retain sharing; copy on write |
| `local` | Each instance compresses cold blocks and can deduplicate within its local store | No shared compressed objects across VMs | Restore instance-private RAM |
| `physical` | Daemon holds shared physical pages with identical contents | VMs map the same page | Retain sharing; copy on write |
| `compressed` | Daemon stores reusable compressed cold blocks | VMs reuse compressed objects | Restore instance-private RAM |

Physical sharing currently operates on 4 KiB pages. Local and pooled compression mainly use 64 KiB blocks with a minimum compression-savings requirement. Identical cross-VM data that compresses poorly can therefore be rejected by the compressed path. These labels identify experimental controls, not configuration options that can all be combined.

Immutable-file DAX sharing uses a separate path: VMs directly map the same offsets of one backing file, without first scanning those file pages. The library experiment uses an explicit immutable attachment; ordinary rootfs `.so` files do not acquire this property automatically. Ordinary file-read hints only prioritize candidates for the physical scanner; merging still requires content validation.

## Experimental data and analysis {#results}

### Deduplication and shared-library curves {#linux-lifecycle}

The [static working-set curves](../../design/vm-memory-performance-analysis.md#pool-static-results) include stock Firecracker, dedicated rootless Docker, and pVisor off, KSM, physical, compressed and local. Each of the three workloads has 0–60-second group-charged memory and PSS curves, with first-readback and CPU readings retained.

![60-second memory curves for three working sets: Firecracker, Docker and pVisor](../../assets/benchmarks/memory-pool-static-idle.svg)

Rows show repeated contents, identical random contents across instances, and instance-unique random contents. The left column shows whole-group cgroup charges; the right column shows process PSS. The horizontal axis is seconds after all instances are ready, and the vertical axis is MiB, with N=1 per condition. The physical pool drops earlier for shared random contents, while KSM uses less memory at the end of the window. Unique random contents show limited sharing benefit.

### Reclamation speed and CPU cost {#reclaim-cost}

The table uses identical random contents across instances, with N=1 per condition. Memory is whole-group charged MiB at 60 seconds; CPU is cumulative CPU seconds over the 60-second window. Reclamation time is the sampled interval for a 128 MiB drop from each condition's all-ready charge, sustained through the end of the window. “Not reached” means the target was not met within the window.

| Mode | Charged MiB at 60 s | Group CPU s | Time to sustained 128 MiB drop, s |
|---|---:|---:|---:|
| firecracker | 365.67 | 0.164 | Not reached |
| docker-rootless | 423.99 | 0.183 | Not reached |
| pvisor-off | 398.91 | 0.386 | Not reached |
| pvisor-ksm | 177.33 | 0.384 | 27.723–27.773 |
| pvisor-physical | 222.61 | 5.924 | 1.688–1.739 |
| pvisor-compressed | 377.89 | 2.838 | Not reached |
| pvisor-local | 373.48 | 2.341 | Not reached |

KSM additionally has **0.41 CPU seconds** of global ksmd work outside the group quota, which cannot be attributed entirely to this group. Group CPU includes VMs, the pool and in-group control work; these readings measure the complete path.

These curves show a tradeoff between reclamation speed and sustained cost: the physical pool reaches the observed target at about 1.7 seconds, while KSM reaches it at about 27.8 seconds and has lower memory and CPU readings at 60 seconds. Short-lived working sets place more value on memory released in the first few seconds; long-lived pages have more time to accumulate sharing benefits. Selection depends on shareable page lifetime and CPU budget.

This experiment does not measure real-Agent call-duration distributions or establish benefits for subsecond calls. Time to target is not the first merge or full convergence time; pages in persistent VMs can retain sharing across multiple tool calls.

### Does readback preserve the saving? {#readback-cost}

These separate readings use repeated contents, with N=1 per condition and whole-group charged MiB. Complete readback time includes all four instances, full-byte and digest verification, and control transport.

| Mode | MiB before read | MiB after read | Complete read ms |
|---|---:|---:|---:|
| pvisor-off | 398.92 | 399.09 | 621.99 |
| pvisor-local | 117.46 | 374.58 | 1157.44 |
| pvisor-compressed | 186.17 | 419.73 | 1099.13 |
| pvisor-physical | 149.14 | 149.10 | 839.57 |

Physical sharing retains low usage after reads but still incurs access costs. Compressed paths rise from their low idle readings as private RAM is restored; frequent reuse needs to account for that cost. The paths also differ in block size, admission and IPC, so their full timing difference cannot be attributed to the compression algorithm.

### Shared libraries: file hints and DAX {#library-curves}

The [shared-library curves](../../design/vm-memory-performance-analysis.md#pool-library-results) compare loading paths for the same `.so`, including file hints off/on and explicit DAX. Time starts when all instances are ready; sharing during startup appears in the initial footprint. Complete readback and private-write validation remain intact; post-write curves are omitted.

![60-second charged-memory and PSS curves for the shared-library workload](../../assets/benchmarks/memory-pool-library-idle.svg)

![Shared-library memory curves zoomed into the first five seconds after readiness](../../assets/benchmarks/memory-pool-library-early.svg)

Both library figures use the same N=1 preflight and whole-group MiB on the vertical axes. The first-five-second view retains original samples to show early differences with file hints off/on; explicit DAX sharing is already reflected in the initial footprint. See the [detailed analysis](../../design/vm-memory-performance-analysis.md#pool-readback) for scanning CPU and readback costs.

### Sources and configuration {#run}

The [memory experiment record](https://github.com/DeepLink-org/pvisor/blob/main/benchmark.new/memory/README.md#published-curves-and-archived-history) retains current curve provenance and reproduction protocols. Failed attempts, incomplete repeated measurements and post-write evidence remain intact and excluded from successful preflights.

See the [daemon guide](../../guides/daemon/index.md#memory-pool) to enable the physical pool and [CLI control options](../../reference/cli.md#vm-instance-control) for VM compression, deduplication and offload configuration.
