# VM memory experiments: methodology and accounting boundaries

[Usage decisions](index.md) · [Detailed results](results.md)

## Scope of this run {#scope}

Run on 2026-10-03 after freezing a build snapshot, using the real `pvisor run` and
`pvisor-memory-pool`, built and signed by `just build release`. Historical SDK
fixture results do not substitute for CLI measurements. Each VM runs the same
static Linux program and actually allocates and accesses 64 MiB of data. This
is a controlled memory-mechanism workload, not an end-to-end benchmark of
Ubuntu, Python agents, LLM inference, or project compilation. Image downloads,
builds, and rootfs preparation are outside the execution samples.

| Environment | Conditions |
|---|---|
| Host | Apple M4, 24 GiB RAM; macOS 27.0.1 / 26A434 |
| pVisor | 0.3.0; commit, runtime worktree sources, and signed executable SHA-256 recorded |
| VM backend | Locally patched libkrun / krun-hvf 1.19.3; HVF |
| Firmware | Existing libkrunfw 5.5.0 cache; actual dylib SHA-256 recorded |
| Guest program | Rust 1.98.0, aarch64 Linux musl, optimization level 2 |
| Host pages | 16 KiB; pager blocks 64 KiB |
| Observation | Process and host samples every second; experimental diagnostics enabled equally for successful cases |
| Pressure control | No additional pressure allocator; record actual pressure levels and stop at critical |

Measurements target the build snapshot fixed for this run. Later source changes or rebuilds from concurrent work do not enter the dataset. Evidence retains binary hashes, the build-time runtime worktree diff, and experiment source files; this report does not validate all subsequent code.

## What the two datasets answer {#datasets}

| Dataset | Runs | Question | Main observation windows |
|---|---:|---|---|
| 35-second parameter matrix | 40 | How parameters and workloads affect reclamation and restoration at equal task duration | 18–33 seconds after ready |
| Extended 2 GiB observation | 4 | Whether larger VMs continue reclaiming after scanning starts | 60–90, 120–150 and 150–175 seconds after ready |

Both use the same frozen CLI, pool and firmware; they do not combine into one savings percentage. The 175–178 second endpoint was added descriptively after inspecting the first curve, with only three samples per case. It does not replace predefined windows or steady-state acceptance. The overview rounds representative results; complete evidence retains original precision.

## Parameter and workload matrix {#matrix}

Each configuration compares the pool disabled and enabled, twice. The second
repetition reverses paired order. The formal plan is 10 configurations, 20
pairs, and 40 successful cases. Two repetitions cannot support reliable
across-run p95/p99 or confidence intervals. Preserve paired ranges; multiple
one-second samples are not independent experimental repetitions.

| Variable | Settings |
|---|---|
| Memory per VM | `--memory 256MiB` / `512MiB` / `2048MiB` |
| vCPU | `--cpu 1` / `2` |
| VM count | One / two simultaneous VMs; the latter share one pool |
| Shared pool | Unset / `--vm-memory-pool SOCKET` |
| Pool payload limit | Default 16 MiB / `--max-bytes 1048576` |
| RAM file | Automatic temporary backing / `--vm-ram-backing FILE` |
| Repeated cold data | 64 MiB periodic byte sequence, initially identical in both VMs |
| Random cold data | Independently seeded xorshift content per VM; no compressibility or sharing assumption |
| Hot data | Scan every 4 KiB page every 50 ms, using a compiler barrier to prevent eliminated reads |
| Network | Same cold data plus OverlayNet TCP loopback echo; three checked 32 MiB bursts per VM |

Use seconds 18–33 after all guests become ready as the fixed observation
window. A common time window **does not guarantee steady state**; scanning
and reclamation may still be progressing in larger VMs. Then read and check
all 64 MiB three times, two seconds apart, retaining first and subsequent
access times. Later reads can trigger renewed cold restoration and are not
automatically “warm reads.”

The main checksum workload is single-threaded. The 1 / 2 vCPU comparison covers these settings and this workload, not CPU scalability of multithreaded applications. First-read times take a median across VMs, then across two runs; ratios are calculated per pair and can differ from dividing aggregated times.

This run compares **startup behavior at the same task duration**, not steady state across all capacities. The 2 GiB configuration had zero pool payload within the window, with its first nonzero sample about 34.3 seconds after ready. The [results page](results.md#scan-startup) gives the scan mechanism, timing evidence and requirements for subsequent steady-state measurements.

## Memory, pressure, and performance metrics {#metrics}

| Metric | Calculation / scope | What it does not establish |
|---|---|---|
| RAM plus pool proxy | Runner RAM residency diagnostics + pending file reclamation + staging snapshots + encoded pool payload | Not all process memory or unique host physical pages; not all metadata |
| Process-group footprint ledger | Sum native `proc_pid_rusage` footprint for CLI processes, runners, and pool | File-cache and shared accounting differs from the RAM proxy; they are not substitutes |
| Process-group RSS sum | Sum resident size for the same processes | Shared pages may count repeatedly; not unique physical memory |
| Host pressure | `kern.memorystatus_vm_pressure_level`: 1 NORMAL / 2 WARN / 4 critical | Pressure is not caused only by pVisor |
| Host compressor | `vm_stat` occupied pages × actual page size | Stored pages are logical uncompressed pages, not physical occupancy |
| Swap change | Host swap-used difference between first and last case samples | Cannot be attributed directly to this configuration |
| CPU seconds | Native counters for CLI processes + runners + pool, converted with Mach timebase | Sampling can miss exit tails; excludes the Python observer |
| First read | Guest checksum over all 64 MiB | Not a single-page fault or general shell-command latency |
| Maintenance barrier | Sum of barrier calls for each two-phase iteration; retain per-run P95 and maximum | Excludes publication outside barriers; not total access latency |
| Maximum cold restore | Pager's maximum single-block restore time | Not full restoration time for all 64 MiB |

Raw CPU values must not be treated directly as nanoseconds. This machine's
Mach timebase is 125/3, checked against `time.process_time()` with a short
owned CPU workload. The kernel field assignment is in
[Apple XNU `fill_task_rusage`](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/kern/bsd_kern.c).
Formal records retain raw ticks, converted ns, and the calibration ratio.

Calculate savings as `1 - pool / baseline` for each pair, then summarize the
two pairs. Within-run P95 comes from that run's maintenance events. Summaries
of two runs' P95 values are not the pooled event P95. Retain medians, sampled
maxima, first wakeups, and restore peaks instead of showing only quiet-period
best results.

Barrier metrics measure wall time of both calls, including possible scheduling and waiting; they are not exact accumulated vCPU stopped time. Single / dual VMs also change each VM's share of the common budget, preventing independent attribution of deduplication benefits.

One-second observations are not atomic across processes: RAM diagnostics, pool payload, and host counters have sampling skew. RAM records must be available and no more than three seconds old. Peaks are observed sample or logged-event maxima, not hard bounds for unobserved intervals.

## Reproduce {#reproduce}

Set the existing firmware directory first. The output directory must not
exist, preserving historical data. This command requires Apple Silicon macOS
with usable HVF and permission for host sockets and Hypervisor execution.

```bash
just build release
FIRMWARE_DIR=/path/to/pvisor/firmware/5.5.0/macos-aarch64
EXPERIMENT_BIN=$(mktemp -d /tmp/pvisor-memory-bin.XXXXXX)
cp target/release/pvisor target/release/pvisor-memory-pool "$EXPERIMENT_BIN/"
codesign --verify --strict "$EXPERIMENT_BIN/pvisor"
python3 tools/experiments/macos-memory/cli_decision_matrix.py \
  --firmware "$FIRMWARE_DIR" \
  --binary-dir "$EXPERIMENT_BIN" \
  --output target/memory-cli-matrix-new \
  --repeats 2
```

Raw data uses the separate `pvisor-memory-cli-matrix/v1` schema, not the
existing host microbenchmark's `pvisor-benchmark/v1`. Every case retains full
arguments, stdout, stderr, process identities and clock counters, host
`vm_stat`, pool statistics, and validation results. The summarizer rechecks
pairs, input contents, CPU units, RAM proxies, and reference reclamation.
Failed cases remain in their original directories and are excluded from
successful performance comparisons.

Copy signed executables to isolate concurrent builds. Formal cases check CLI SHA-256 at startup and exit; every pair must match the single digest recorded by the matrix. Do not keep launching samples from a `target/release` directory that another build may replace.

## Limits and stop conditions {#limits}

Other applications share this host; background load was not isolated. Host
compressor, swap, and pressure changes describe environment and stop
conditions. This run does not pass full physical-memory acceptance. Two
repetitions also cannot establish long-term stability, application coverage,
or a production SLA.

Stop at critical pressure, at least 2 GiB swap growth from the run's initial
snapshot, or less than 4 GiB free disk. No other applications were closed,
administrator password requested, macFUSE automatically enabled, or existing
physical-memory acceptance threshold lowered.

Pairs run sequentially on one host, but pressure levels and background load are not guaranteed identical. The pressure table exposes each mode and repetition; this is not a causal pressure experiment on an isolated host.

## Reproducing the extended 2 GiB observation {#long-idle-2048}

Use the same frozen executables in a separate output directory, retaining two pairs and reversed order. The default remains 35 seconds; the follow-up only extends waiting and matching timeouts. Replace the executable, firmware and output paths below. The [results page](results.md#long-idle-2048) separates predefined windows from the descriptive endpoint.

```bash
python3 tools/experiments/macos-memory/cli_decision_matrix.py \
  --binary-dir /path/to/frozen-binaries \
  --firmware /path/to/firmware \
  --output /path/to/evidence/cases \
  --only cold-2048-dual --idle-seconds 180 --repeats 2
python3 tools/experiments/macos-memory/long_idle_report.py /path/to/evidence
```
