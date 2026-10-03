# VM memory experiments: results and evidence

[Usage guidance](index.md) · [Methodology](methodology.md)

**Cold RAM reclamation is observed at 256 MiB, 512 MiB and 2 GiB capacities.** Extending the 2 GiB experiment to 180 seconds shows why observation time matters: RAM proxy falls by about 60% in the 60–90 second window, with restoration costs on first access. This page explains that curve first, then parameter choices and complete ledgers.

## Experiment scope {#validity}

On 2026-10-03, Apple M4 / 24 GiB / macOS 27.0.1 runs use the same frozen signed CLI, pool executable and firmware. The original matrix covers 10 configurations and 40 runs with 35 seconds of idle or hot access. The 2 GiB follow-up adds four runs with 180 seconds idle. Each configuration has two enabled / disabled pairs, reversing order on repetition two.

Each VM allocates and accesses 64 MiB of application data. Configured capacity is the guest address capacity; 2 GiB does not mean 2 GiB of allocated application data. All successful runs pass three full content checks and private mutation checks; pool references clear on exit. Builds and image preparation are excluded, with diagnostics enabled in both groups. Results describe the frozen version, excluding subsequent source changes; [methodology](methodology.md#scope) and [evidence](#evidence) establish provenance.

## 2 GiB: observing the full reclamation process {#long-idle-2048}

On the same frozen version, idle observation extends from 35 to 180 seconds. Each VM still owns 64 MiB of repeated cold data: two VMs, 2 vCPUs and the default 16 MiB pool budget. Two pairs reverse order, keeping observation duration separate from configuration changes.

**The sustained-window result is about 60% lower RAM proxy: approximately 309 MiB off and 124 MiB on.** These are samples from seconds 60–90 after ready, with similar results in both pairs. Before roughly 34 seconds, scanning is still starting, explaining negligible reclamation in the original short window.

![2 GiB reclamation curves and memory ledgers](assets/2048-long-idle.svg)

Top: RAM proxy; middle: encoded pool data; bottom: footprint. Solid / dashed lines show both repetitions. Another decline occurs near three minutes, followed by a RAM increase on reading. Exact short-endpoint readings remain below, but last only seconds and do not establish steady state. See [performance](#performance) for first-access and CPU costs and [physical accounting](#physical) for footprint increases.

??? note "Exact window data (including startup and descriptive endpoint)"

    | Window after ready (seconds) | RAM proxy MiB off / on | Median paired savings (range) | Footprint ledger MiB off / on |
    |---|---|---|---|
    | 18–33 | 309.0 / 310.1 | -0.3% (-1.1–0.4%) | 18.0 / 30.4 |
    | 60–90 | 309.0 / 124.2 | 59.8% (59.7–59.9%) | 18.0 / 47.2 |
    | 120–150 | 309.0 / 124.5 | 59.7% (59.6–59.8%) | 18.0 / 48.7 |
    | 150–175 | 309.0 / 124.4 | 59.7% (59.6–59.9%) | 18.0 / 49.1 |
    | 175–178* | 309.0 / 28.6 | 90.7% (90.7–90.8%) | 18.0 / 52.3 |

??? note "Window accounting, restoration costs and plateau criteria"

    Each case uses the median within a window, followed by aggregation across the two pairs. Ranges are not confidence intervals. The starred 175–178 second interval is a descriptive endpoint added after inspecting the first run, with only a few samples per case; it is neither a predefined window nor steady-state evidence. Samples require diagnostics from both runners no more than three seconds old. Restoration and exit after 180 seconds do not measure idle savings. RAM proxy includes resident RAM, pending file reclamation, temporary snapshots and pool payload. Footprint is a separate ledger and cannot substitute for it.

    **The 2 GiB configuration substantially reduces RAM residency for this workload too.** Both runs begin publishing cold pages around 34 seconds. At 60–90 seconds, proxy memory falls from about 309 MiB with the pool disabled to about 124 MiB enabled, yielding 59.7%–59.9% paired savings. Another decline occurs around 166–176 seconds; the descriptive 175–178 second endpoint is about 28.6 MiB, with roughly 90.7% paired savings. The intermediate plateau has transient increases, and the lower terminal plateau is observed for only a few seconds. **Neither run passes the full predefined plateau criterion; final steady-state time remains unknown.** These curves establish reclamation for this workload, not the same savings for arbitrary 2 GiB applications.

    Costs: whole-run sampled CPU is about 23.6 / 40.2 seconds off / on, with 15.9–17.2 seconds of paired additional CPU. The first 64 MiB read takes about 20.3 / 164.1 ms, or 7.3–8.8 times longer per pair. Terminal footprint ledgers are about 18.0 / 52.3 MiB, still roughly 190% higher. Pool payload approaches the 16 MiB budget, with 30 / 25 whole-run capacity rejections; content checks and exit reference cleanup all pass. Host pressure remains WARN in all four runs without triggering stop guards. Other applications affect whole-host compressor and swap readings, so this follow-up does not prove improved physical-memory pressure. Diagnostics are enabled in both groups and their CPU overhead is included.

    Predefined plateau criteria: at least 25 valid samples per 30-second window; RAM proxy span at most max(4 MiB, 10% of the median); nonzero pool payload with span at most 10% of its median; absolute least-squares RAM trend at most 1 MiB per 30 seconds. Plateau onset also requires every subsequent complete rolling window through 175 seconds to meet those criteria. These criteria describe this experiment, not production steady state.

[Full timeline CSV](assets/2048-long-idle.csv) · [Independent verification JSON](assets/2048-long-idle.json). Raw runs, the predefined protocol, source snapshots and input hashes are in `review_project/06-evidence/macos-memory/cli-2048-long-idle-2026-10-03/`, with runs under `cases/`. Binary source provenance remains the original matrix's `input-provenance.json`; follow-up working-tree hashes do not establish the frozen executable's source.

## How parameters affect benefits {#matrix}

Repeated cold data provides the clearest benefits under the default budget. Continuously accessed data, independent random content and smaller pool budgets reduce reclamation. Explicit RAM files perform similarly to default backing; the file option does not itself add compression or sharing. Single-VM benefits include cold compression and cannot all be attributed to cross-VM deduplication.

The original matrix compares seconds 18–33 after ready to observe behavior at equal task duration. The 2 GiB profile has not begun publishing cold pages in that window. Near-zero savings there do not contradict the longer experiment above: short tasks and long idle tasks answer different usage questions.

??? note "Full 35-second parameter matrix and paired ranges"

    | Profile | RAM proxy MiB off / on | Proxy saved | Added CPU seconds | First read ms off / on |
    |---|---|---|---|---|
    | cold-2048-dual | 311.7 / 310.6 | 0.4% | +0.89 | 24.4 / 15.6 |
    | cold-256-cpu1 | 224.1 / 25.2 | 88.8% | +3.94 | 19.7 / 184.1 |
    | cold-256-dual | 231.4 / 26.5 | 88.6% | +4.69 | 23.3 / 176.9 |
    | cold-256-file | 232.0 / 26.1 | 88.7% | +3.66 | 18.4 / 181.3 |
    | cold-256-pool1 | 231.5 / 201.1 | 13.1% | +3.57 | 28.5 / 31.7 |
    | cold-256-single | 115.6 / 19.1 | 83.5% | +2.34 | 18.5 / 189.8 |
    | cold-512-dual | 242.8 / 60.7 | 75.0% | +5.12 | 22.1 / 168.9 |
    | hot-256-dual | 230.1 / 155.7 | 32.4% | +2.55 | 9.1 / 5.5 |
    | network-256-dual | 231.7 / 26.3 | 88.6% | +3.83 | 24.7 / 183.5 |
    | random-256-dual | 230.9 / 187.4 | 18.8% | +3.61 | 22.4 / 26.4 |

    | Profile | Paired savings range | Window sampled-max savings | Max cross-session objects | Max rejection counter |
    |---|---|---|---|---|
    | cold-2048-dual | -0.6–1.3% | 0.6% | 256 | 0 |
    | cold-256-cpu1 | 88.8–88.8% | 76.6% | 597 | 0 |
    | cold-256-dual | 88.5–88.6% | 78.9% | 596 | 0 |
    | cold-256-file | 88.7–88.8% | 74.9% | 583 | 0 |
    | cold-256-pool1 | 12.7–13.5% | 12.7% | 44 | 2173 |
    | cold-256-single | 83.5–83.5% | 74.0% | 0 | 0 |
    | cold-512-dual | 74.7–75.3% | 69.9% | 506 | 0 |
    | hot-256-dual | 32.0–32.7% | 30.1% | 525 | 0 |
    | network-256-dual | 88.5–88.7% | 78.0% | 598 | 0 |
    | random-256-dual | 15.5–22.1% | 15.2% | 254 | 1734 |

    Case memory is the median of samples 18–33 seconds after all guests are ready; these tables then summarize two pairs. The range is not a confidence interval. “Sampled maximum savings” remains within this window, not the whole-run peak from startup through exit. Cross-session objects establish reuse, not its independent contribution to savings.

## Usage costs: first access and CPU {#performance}

Reclaimed data must be restored. In the extended 2 GiB experiment, the first complete 64 MiB read increases from about 20 ms to 164 ms. It is 7.3–8.8 times slower per pair, with 15.9–17.2 seconds of additional whole-run sampled CPU. Latency-sensitive tasks should consider this cost before enabling the pool. This is neither single-page fault latency nor end-to-end latency for a typical shell command.

The original short-window 2 GiB run has not reclaimed the same amount of data, so its first read does not exercise equivalent cold restoration. Faster readings there do not establish pool acceleration. Faster hot-access and network echo results describe those controlled measurements, not general acceleration.

??? note "Original 35-second performance data and accounting"

    | Profile | CPU seconds off / on | First-read ratio | Repeated read ms off / on | Barrier P95 / max ms | Max block restore ms | Network burst time ratio / range |
    |---|---|---|---|---|---|---|
    | cold-2048-dual | 5.76 / 6.65 | 0.7× | 7.4 / 12.9 | 49.91 / 60.71 | 0.29 | — |
    | cold-256-cpu1 | 1.80 / 5.74 | 9.4× | 5.3 / 63.4 | 3.28 / 52.60 | 15.62 | — |
    | cold-256-dual | 1.90 / 6.59 | 7.6× | 8.3 / 54.3 | 3.58 / 14.80 | 4.72 | — |
    | cold-256-file | 2.06 / 5.72 | 11.2× | 6.2 / 57.8 | 7.51 / 99.05 | 18.23 | — |
    | cold-256-pool1 | 1.93 / 5.50 | 1.1× | 4.6 / 13.5 | 3.12 / 25.82 | 1.04 | — |
    | cold-256-single | 0.96 / 3.30 | 10.8× | 8.0 / 68.1 | 3.02 / 6.21 | 14.71 | — |
    | cold-512-dual | 2.63 / 7.75 | 7.9× | 6.5 / 7.5 | 3.71 / 17.02 | 10.18 | — |
    | hot-256-dual | 2.49 / 5.04 | 0.6× | 7.5 / 49.3 | 3.09 / 24.75 | 14.07 | — |
    | network-256-dual | 5.06 / 8.89 | 8.6× | 5.7 / 68.6 | 3.33 / 13.35 | 10.67 | 0.58× (0.50–0.66) |
    | random-256-dual | 2.17 / 5.79 | 1.2× | 7.0 / 12.9 | 3.23 / 34.27 | 23.17 | — |

    CPU sums native counters for CLI, runners, and pool, converted through Mach timebase, including startup and execution. One-second sampling can miss exit tails. It is not total host CPU and excludes the observer. The first read verifies all 64 MiB; later reads are separated by two seconds and may restore cold data again, so they are repeated reads rather than guaranteed warm reads. Barrier P95 is the within-run event P95 of the sum of both barrier calls, then summarized across two runs; publication outside barriers is excluded. Maximum restoration measures one 64 KiB block.

    The network configuration sends data through OverlayNet to a host loopback echo service. Every VM opens three connections, each verifying 32 MiB of returned content. This covers TCP echo, not DNS, Gateway / LLM, UDP, or Internet throughput. Its loopback rule explicitly enables allowlist and `allow_private_ips=true`, keeping authorization failures out of throughput measurements.

    Hot sweeping continues only during the 35-second observation phase and stops before verification. The first read can retain hot state while subsequent reads, two seconds apart, can become cold again. A faster first hot read does not establish accelerated computation from the pool. Network ratios describe only these paired echo runs and do not establish general network acceleration.

## Residency reduction and physical accounting {#physical}

RAM proxy answers whether cold guest RAM has been reclaimed; footprint reflects a different macOS process ledger. File and shared pages have different attribution, while the pool, snapshots and metadata add costs. These metrics cannot substitute for each other or be subtracted to obtain net physical benefits.

At the extended 2 GiB endpoint, RAM proxy is about 309 / 29 MiB off / on, while footprint is about 18 / 52 MiB. **Residency reclamation works, but footprint does not fall with it.** The original matrix also shows footprint increases, so this report promises neither whole-host physical savings nor additional instance capacity. The overview curve displays both outcomes.

Other host applications influence pressure, compressor occupancy and swap. These provide environmental records and stop guards; their changes cannot be attributed to a VM alone. All four extended runs remain at WARN without triggering stop guards. Improved physical pressure still requires independent verification.

??? note "Complete footprint, RSS and host environment records"

    | Profile | Footprint MiB off / on | Footprint change | RSS MiB off / on | Whole-run sampled-max footprint MiB off / on |
    |---|---|---|---|---|
    | cold-2048-dual | 18.0 / 31.9 | +77.7% | 440.7 / 260.0 | 18.0 / 71.5 |
    | cold-256-cpu1 | 17.7 / 50.3 | +185.0% | 343.2 / 135.9 | 17.7 / 228.4 |
    | cold-256-dual | 17.9 / 47.7 | +166.2% | 356.3 / 148.2 | 17.9 / 206.9 |
    | cold-256-file | 17.9 / 41.1 | +129.3% | 361.6 / 149.0 | 17.9 / 214.3 |
    | cold-256-pool1 | 18.0 / 24.3 | +34.9% | 361.6 / 325.1 | 18.0 / 42.8 |
    | cold-256-single | 8.9 / 35.9 | +301.8% | 177.7 / 85.2 | 8.9 / 145.2 |
    | cold-512-dual | 18.0 / 47.2 | +161.3% | 372.6 / 157.4 | 18.0 / 298.3 |
    | hot-256-dual | 18.0 / 40.5 | +125.3% | 360.2 / 282.4 | 18.0 / 108.3 |
    | network-256-dual | 17.9 / 40.3 | +124.5% | 362.1 / 149.6 | 19.4 / 288.7 |
    | random-256-dual | 17.9 / 40.9 | +128.5% | 360.4 / 313.0 | 17.9 / 56.5 |

    | Profile | Pool | Pressure r0 / r1 | Swap change MiB r0 / r1 | Physical compressor change MiB r0 / r1 |
    |---|---|---|---|---|
    | cold-2048-dual | off | NORMAL/NORMAL | +0.0/+0.0 | +73.9/-34.9 |
    | cold-2048-dual | on | NORMAL/NORMAL | +0.0/+0.0 | -115.8/+91.1 |
    | cold-256-cpu1 | off | NORMAL/NORMAL,WARN | +0.0/+0.0 | +322.8/-770.5 |
    | cold-256-cpu1 | on | NORMAL,WARN/NORMAL | -32.0/+0.0 | +465.8/+661.4 |
    | cold-256-dual | off | NORMAL/WARN | -24.0/+0.0 | -155.1/+517.9 |
    | cold-256-dual | on | NORMAL/WARN | +0.0/+0.0 | -9.0/-288.0 |
    | cold-256-file | off | WARN/WARN | +0.0/+0.0 | +189.1/-13.2 |
    | cold-256-file | on | WARN/WARN | +0.0/+0.0 | -675.0/-19.1 |
    | cold-256-pool1 | off | WARN/WARN | +0.0/+0.0 | +633.5/-21.8 |
    | cold-256-pool1 | on | WARN/WARN | +0.0/+0.0 | +353.4/-184.5 |
    | cold-256-single | off | NORMAL/NORMAL | +0.0/+0.0 | -9.2/+1036.7 |
    | cold-256-single | on | NORMAL/NORMAL | +0.0/+0.0 | +56.7/-54.2 |
    | cold-512-dual | off | NORMAL/NORMAL,WARN | -8.0/-8.1 | -122.2/-639.2 |
    | cold-512-dual | on | NORMAL/WARN | +0.0/+0.0 | -58.0/-167.7 |
    | hot-256-dual | off | WARN/WARN | +0.0/+0.0 | -66.1/-121.6 |
    | hot-256-dual | on | WARN/WARN | +0.0/+0.0 | +213.1/-245.7 |
    | network-256-dual | off | WARN/WARN | +0.0/+0.0 | -69.0/+189.8 |
    | network-256-dual | on | WARN/WARN | +0.0/+0.0 | -88.3/+296.0 |
    | random-256-dual | off | WARN/WARN | +0.0/+0.0 | -424.0/+23.1 |
    | random-256-dual | on | WARN/WARN | -24.0/+0.0 | +122.2/+670.3 |

    Separate three facts: RAM proxy reduction, whether process-group footprint falls in the same direction, and whether host pressure changes. These are different ledgers and cannot be added: RSS can count shared pages repeatedly, file-backed RAM footprint accounting differs from residency, and the host compressor includes other applications. In some cases compressor changes exceed 1 GiB, far beyond the 64 MiB × instance-count application dataset, preventing attribution to this VM configuration.

    The pressure table describes the actually observed pressure states and host endpoint changes; **it does not prove that the pool reduces physical pressure**. This run adds no pressure allocator and covers no critical phase. Earlier physical-attribution probes lacking administrator privileges are not recast as successes. These measurements cannot promise how many additional instances fit on this host or pass the original whole-physical-memory savings threshold.

## Scan startup and short-window results {#scan-startup}

**This measurement captures reclamation startup delay, not steady-state savings for a 2 GiB VM.** In both `cold-2048-dual` runs, encoded pool payload was zero throughout the 18–33 second window after ready. The first nonzero payload samples occurred at **34.31 and 34.27 seconds**, respectively. The window shows no observed pool reclamation benefit; paired savings range from −0.6% to 1.3%, so the summarized 0.4% is not a reliable benefit.

The experiment version's pager uses one sequential cursor: its first visit arms access observation; only after traversing the address space does it revisit still-cold blocks to snapshot, publish and reclaim them. The 200 ms threshold is a minimum observation period, not a guarantee of reclamation when that period expires. Blocks are 64 KiB, each iteration processes at most 256 blocks, and iterations sleep for 250 ms. Counting only these sleeps gives the theoretical first-pass durations below; scanning, synchronization and scheduling add overhead.

| Memory per VM | Blocks | Theoretical first-pass duration |
|---|---:|---:|
| 256 MiB | 4,096 | About 4 seconds |
| 512 MiB | 8,192 | About 8 seconds |
| 2,048 MiB | 32,768 | About 32 seconds |

Application data remains 64 MiB at every capacity. Enlarging the guest address capacity increases scanning work even when application residency stays the same. Whole-run encoded payload peaks were about 12 MiB in both 2 GiB runs, below the default 16 MiB budget, with zero capacity rejections. Scan scheduling is therefore the primary limitation here. Shared objects at the end of a run do not establish corresponding benefits inside the measurement window.

Follow-up measurements should report first reclamation time, startup savings and steady-state savings after longer idle periods separately. Establish steady state from stabilizing reclamation and residency, rather than simply choosing a later fixed window. First evaluate timely revisits of observation blocks whose threshold has elapsed, then consider scan quotas. Maintenance barrier P95 was 49.91 ms here, so larger batches may increase latency. **This run establishes neither final steady-state savings for 2 GiB VMs nor physical-memory benefits.**

Evidence comes from `cold-2048-dual-r{0,1}-pool/raw.json` in the frozen matrix: each run has 14 window samples, all with zero payload. Times denote the first nonzero samples and have approximately 1 Hz sampling resolution. Source locations are `Pager::sample` and the maintenance loop in `crates/pvisor/src/executor/vm/pager.rs`; see this page's evidence inventory for version provenance.

## When different capacities reach a plateau {#settling-time}

Existing records establish **reclamation onset and observed plateau times**, not steady-state times validated over a long duration. This comparison uses only the matched `cold-{256,512,2048}-dual` profiles: two VMs, 64 MiB of repeated cold data per VM, 2 vCPUs and the default pool budget. Times start when all guests are ready. RAM proxy is the aggregate of both runners and the pool, not physical memory per VM.

| Capacity per VM | First nonzero pool sample r0 / r1 | Observed plateau and timing | Steady-state evidence limit |
|---|---|---|---|
| 256 MiB | 4.19 / 5.19 seconds | Lower plateau near 26 MiB begins at 22.85 / 22.96 seconds; about 25.6–29.3 MiB thereafter until 35 seconds | Only about 12 seconds observed; long-term steady state is unproven |
| 512 MiB | 8.32 / 8.34 seconds | Plateau near 60 MiB begins at 16.61 / 16.57 seconds; about 59.6–75.6 MiB thereafter until 35 seconds | Intermittent residency increases prevent a strict stability claim |
| 2,048 MiB | 34.31 / 34.27 seconds | No post-reclamation plateau observed before 35 seconds | Idle phase ends too early; steady-state time is unknown |

The 256 MiB profile first falls to about 60 MiB at 11–12 seconds, then continues reclaiming and drops again around 21–23 seconds. Its first short plateau is not final steady state. The 512 MiB profile starts reclaiming later but reaches a roughly 60 MiB plateau earlier. This does not establish faster convergence to the same reclamation level in larger VMs: their observed final residency levels and scan progress differ.

![Reclamation timelines for different memory capacities](assets/startup-timeline.svg)

Top panels show the aggregate RAM proxy across both VMs and the pool; bottom panels show encoded pool payload. Solid blue and dashed orange lines represent the two runs. Green marks the 18–33 second measurement window; purple marks reads, writes and exit after 35 seconds. Shared axis scales support comparison. Curves start at samples at least one second after ready, excluding lagging diagnostics immediately at ready. The 2 GiB payload remains zero throughout the green window; its final drop to zero occurs during exit and does not indicate completed reclamation.

[Download all six time series](assets/startup-timeline.csv) to inspect intermediate plateaus and transient changes. Sampling is approximately 1 Hz; plateau times above are descriptive readings of the curves, not results from a predefined steady-state acceptance threshold. After 35 seconds, full reads, private writes and exit change the workload phase, so those samples cannot estimate idle steady state. Capacity-planning measurements need a longer idle phase, predefined bounds on RAM and pool occupancy variation over sustained windows, and confirmation that no downward trend remains. The original 35-second matrix contains no such long-duration measurements. The 180-second follow-up below extends observation but still does not establish final steady state.

## Unsupported, failed, and uncovered combinations {#compatibility}

| Combination / condition | This run | User implication |
|---|---|---|
| `--vm-memory-pool` + `--vm-ram-compression` | Explicit rejection before VM initialization | Do not stack the two compression paths |
| `--cpu 0` / `--cpu 9` | Reject nonpositive CPU and more than 8 vCPUs respectively | Use 1–8; only 1 / 2 measured here |
| Missing pool socket | Explicit path-resolution failure | Start the owner-private same-UID service first |
| `--vm-ram-compression` alone | Performance unmeasured: an earlier environment check reported the macFUSE extension disabled | Neither zero savings nor a performance pass |
| Initial network arguments | Missing allowlist caused CLI rejection; after correction a two-connection fixture refused later connections | Both were experiment setup errors, corrected with failures retained and excluded from product performance samples |
| Initial echo summary check | Six separate connections succeeded, but the check expected two aggregated connections | Corrected and rerun; failed status was not changed to passed |
| Initial explicit RAM-file path | Relative output path was passed to a child with a different cwd; backing creation failed | Tool switched to absolute paths, retaining the failure and rerunning |
| Earlier matrix without frozen executables | Another build replaced the CLI; an HVF startup failed with errno 22, and replacement time per case is unavailable | All 26 successful measurements excluded from formal performance tables; rebuilt, froze signed executables, and checked hashes per case |
| Process sampling timeout in the fixed matrix | `ps` exceeded the initial 2-second limit; observer aborted the case | Failure retained and rerun with a bounded 5-second sampling timeout; coverage and pressure guards unchanged |
| Other hosts, higher concurrency, TOML / SDK performance, whole-VM offload combined with pool | Unmeasured here | Do not extrapolate compatibility or performance |

Earlier pilot issues with ready/read matching, long socket paths, and CPU units remain separate; uncalibrated pilot CPU values do not enter formal performance tables. Resume reuses only passed cases, verifies signed executables, firmware and runtime source hashes, retains earlier batch metadata, and keeps the original whole-run host guard reference.

## Evidence and reproduction {#evidence}

[Download summary JSON](assets/decision.json) · [Configuration CSV](assets/decision.csv) · [Parameter-boundary JSON](assets/compatibility.json)

| Object | Record |
|---|---|
| Executable pvisor | `37feeb598fec93ae088883c489bf13e0a1daa7dd9c1f5db60081e2ef391e4289` |
| Executable pvisor-memory-pool | `a6ef7d43509e98081cc8b1b1ebd4331175a4e9e9eef8fdf777bf16e6bd4d63ef` |
| libkrunfw SHA-256 | `d6010939236331445d2415152f7d1ffa9f3aed7d473dc410ea85b2943bdc6dca` |
| Initial source HEAD | `2b3cca8add4580a4d6a9175cd95aade96eb350d0` |
| Guest source SHA-256 | `239a82e7017aee968dbb8fc5b372db62a9450793cf11810614f9936829a6a00b` |

Observed pressure levels: [1, 2] (1=NORMAL, 2=WARN, 4=CRITICAL). Host swap at the first / last samples: 9.86 / 9.76 GiB. No pressure guard stopped this matrix.

Complete commands and logs live under repository directory `review_project/06-evidence/macos-memory/cli-decision-frozen-matrix-2026-10-03/`. Every case contains `raw.json` and each VM's stdout/stderr. The summary retains successful raw-record SHA-256 hashes, and failed directories are not overwritten. Historical experiments remain in the evidence parent directory and do not substitute for this run.

After the [reproduction command](methodology.md#reproduce), use this read-only summarizer to recheck contents, pairing and accounting:

```bash
python3 tools/experiments/macos-memory/cli_decision_report.py \
  target/memory-cli-matrix-new/summary.json \
  --output target/memory-cli-report
```
