# How should you decide whether VM cold-page reclamation is worthwhile?

## Main conclusions {#conclusions}

**Existing data does not establish lower system-wide physical memory than Docker, Firecracker or QEMU; evaluate cold-page reclamation with your working set and recovery budget.** Repetitive-data macOS probes observe lower cold guest-page residency, but the pool also consumes memory. Reduced residency does not establish capacity for more Agents.

| Need | Selection implication |
|---|---|
| Many idle environments; first-access waiting is acceptable | Measure net physical memory and recovery with the actual working set |
| Active builds, random or incompressible data | Repetitive-data probes do not establish a benefit |
| Compare capacity with Docker / Firecracker / QEMU | No matching net-physical-memory ranking |

## Motivation {#motivation}

VMs waiting for model responses may retain cold pages, which must still be read on recovery. Capacity planning needs actual system-wide savings and waiting on the next tool execution; guest residency alone misses pool and recovery costs.

## Experiment design {#interpretation}

<a id="experiment-design"></a>

B-VM-MEMORY uses Apple M4, 24 GiB RAM and macOS/HVF. Pool-on and pool-off configurations each run two VMs, each writing 64 MiB of repetitive compressible data: 40 parameter-matrix runs and four extended 2 GiB observations. Recovery checks data integrity while observing guest residency, pool/inflight data, footprint and CPU.

The RAM proxy combines guest residency and pool/inflight data; it is not net physical memory for the system or a cgroup. The table summarizes observation windows, rather than P50 or confidence intervals. Sample counts do not support tail-latency conclusions. Random data, real tool working sets and matched Docker/Firecracker/QEMU comparisons remain unmeasured.

## Data and analysis {#results}

<a id="experiment-data"></a>

### Observed residency changes {#measurements}

Units are MiB; two VMs per group. Configurations and observation windows remain separate.

| Configured RAM per VM | Window after all guests are ready | Pool-off RAM proxy | Pool-on RAM proxy |
|---|---|---:|---:|
| 256 MiB | 18–33 s | 231 | 27 |
| 512 MiB | 18–33 s | 243 | 61 |
| 2 GiB | 60–90 s | 309 | 124 |

These observations show residency moving into other storage and recovery paths, without proving equal reductions in system-wide physical memory. Include pool memory, backing, CPU and subsequent tool waiting when deciding whether to enable reclamation.

### Evidence compared with existing tools {#linux-lifecycle}

| Tool | Net physical memory for a matched working set | Tool time after recovery | Available comparison |
|---|---|---|---|
| pVisor macOS cold-page pool | No complete net-savings result | Repetitive-data probes; real tools unmeasured | Pool-on/off residency observations |
| Docker / Podman | Unmeasured | Unmeasured | [Idle concurrency](../density.md), a different metric |
| Firecracker / QEMU | Unmeasured | Unmeasured | [Startup and tool tasks](../compare-runtimes.md), not a memory measurement |

Linux and macOS capabilities and accounting differ; these residency figures cannot be transferred across platforms. The standalone snapshot CLI is retired, so its timings do not provide current product recovery budgets. See [CLI reference](../../reference/cli.md) for available entries.
