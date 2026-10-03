# VM startup time

## 1. Conclusions

pVisor starts a new VM and returns its first command output with a measured median of about **84 ms** on macOS / Apple M4 and **173 ms** on Linux / Ryzen 7 9700X: on the order of **0.1–0.2 seconds**. Both use 2 vCPU / 128 MiB, prepared images and warm host caches; image download is excluded.

## 2. Motivation

Agent executors may repeatedly create isolated environments. Startup waiting affects the first command's response time and the total cost of short tasks. This work therefore measures the complete path from launching the CLI to executing a guest workload, rather than only one interval in kernel initialization logs.

Earlier investigations covered Run record persistence, runner parameter transport, early random initialization and firmware trimming. A consistent protocol is needed to establish current performance and distinguish validated gains from experiments without stable benefits. This measurement addresses three questions:

- How long does a new VM take from CLI launch to workload readiness, and how long does exit and finalization take?
- With the same CLI, how do official and trimmed firmware differ in median and tail latency?
- How much time goes into host preparation, process loading, VMM construction and guest execution, and where should optimization focus next?

Optimization must preserve isolation, durable Run records and attestation semantics. Successful execution samples must support performance claims; shorter kernel logs, smaller files or less code cannot independently replace end-to-end measurements.

## 3. Experimental design

### Timing boundaries and workload

We measure from launching pVisor until a new VM executes its first shell command and returns output, labeled **Ready** in the tables. This covers host preparation, VM startup and command execution: the startup wait after a user issues a command.

**Exit** runs from the same starting point until pVisor finishes exiting, including result persistence and other finalization work. Every trial creates a new VM; the image and filesystem are prepared in advance, and host caches are warm.

??? note "Precise timing and control settings for reproduction"

    Timing uses the host monotonic clock, starting before CLI process creation. Ready ends when the host receives the guest's standalone `PVISOR_BENCH_READY` output. The payload only executes `/bin/sh -c 'printf "PVISOR_BENCH_READY\n"'`, without running `dmesg` around the marker. Output transport and host reader scheduling are included.

    Exit ends when the waiter observes CLI termination, including guest exit, sync/reboot, VMM teardown and result persistence. Separate threads wait for readiness output and process exit to avoid extra polling delays; thread scheduling still affects observations.

### Environment and configuration

| Item | macOS / HVF | Linux / KVM |
|---|---|---|
| Host | Apple M4, 24 GiB RAM | Ryzen 7 9700X, approximately 30 GiB RAM |
| System / architecture | macOS 27.0.1 / ARM64 | Fedora 44 / x86_64 |
| Guest root | Prepared Alpine 3.22.1 | Prepared Fedora root |
| VM shapes | 1/2/4 vCPU, 128/2048 MiB; latest remeasurement uses 2 vCPU | 2 vCPU, 128/256/2048 MiB |
| Firmware | Official and trimmed libkrunfw 5.6.2 | libkrunfw 5.5.0 |
| Cache and image preparation | Warm host caches; image preparation excluded | Warm host caches; image preparation excluded |

Hardware, guest filesystems, firmware and CLI artifacts are recorded separately. Both datasets share timing boundaries and describe startup in their respective environments; cross-platform figures do not isolate operating-system or virtualization-backend speed.

??? note "Detailed macOS artifacts and environment"

    | Item | Value |
    |---|---|
    | Date | 2026-10-03 |
    | Host | Apple M4, 10 cores, 24 GiB RAM, AC power |
    | OS / filesystem | macOS 27.0.1 (26A434), APFS |
    | Executor | pVisor 0.3.0 release, libkrun 1.19.3, HVF / aarch64 |
    | Source HEAD at measurement | `d6c607e79b8c497d2994731beb2ce2b3dfe628c0`; working tree status preserved in raw data |
    | CLI SHA-256 | `04d4c05fbc706d34738f4a453449f9f670a7a89021f261b133aa855cb9bc19b4` |
    | Guest rootfs | Alpine 3.22.1 aarch64, prepared directory, virtio-fs |
    | Firmware | official libkrunfw 5.6.2 / locally trimmed 5.6.2; Linux 6.12.109 |
    | Samples | 100 per main case + 5 discarded warmups; 20 per diagnostic case + 5 warmups |
    | Load average, before / after | 3.23, 3.35, 4.25 / 3.31, 3.79, 4.29 |

    Official firmware uses the upstream 5.6.2 prebuilt aarch64 `kernel.c`, wrapped into a macOS dylib without rebuilding Linux. Trimmed firmware comes from a local libkrunfw build. Both use the same kernel version, but the local source is `v5.6.2-3-gf6a710f`: this compares actual artifacts, not an experiment changing only one Kconfig option. The CLI's default downloader remains pinned to 5.5.0; this matrix explicitly selects 5.6.2.

    The official dylib is 23.715 MiB and the trimmed dylib 11.307 MiB, about 52.3% smaller on disk. This does not imply 52.3% lower physical memory. Full hashes, parameters, source status and samples are in the [raw JSON](../../assets/benchmarks/startup-20261003.json). Absolute paths identify measured inputs; replace them with local paths when reproducing.

<a id="linux-methodology"></a>

??? note "Detailed Linux artifacts, environment and validation"

    Linux uses the same first-command output readiness boundary and creates a new VM for every trial. The matrix tests 128, 256 and 2048 MiB with 2 vCPUs, alongside native shell and pVisor host controls.

    | Item | Conditions |
    |---|---|
    | Date | 2026-10-03 |
    | Host | AMD Ryzen 7 9700X, 8 cores / 16 threads, approximately 30 GiB RAM; btrfs |
    | System / backend | Fedora 44; Linux 7.2.8-200.fc44.x86_64; KVM / x86_64 |
    | Guest filesystem | Prepared host Fedora root, `--rootfs /`; commands execute with `/bin/sh` |
    | Artifacts | Fixed GNU release CLI from this round's VM validation; libkrunfw 5.5.0, without a firmware-trimming comparison; full SHA-256 values in raw data |
    | Samples | 5 warmups and 100 measured trials per case; five cases shuffled each round; startup diagnostics disabled |
    | Host load | One-minute load average 0.67 → 1.32; ordinary background workloads retained |

    The matrix contains 500 measured samples, including 300 real VM launches, plus 25 warmups. Every pVisor sample requires completed records and a zero exit code; VM samples also verify actual `virtual_machine` isolation. Failures stop measurement with raw logs retained and cannot count as faster samples.

    Linux and macOS share the first-output timing boundary, but hardware, guest filesystems, firmware and CLI artifacts differ. These results describe startup in each environment and do not isolate operating-system or virtualization-backend speed. Linux uses a fixed artifact; subsequent source changes are outside this dataset.

### Sampling, validation and statistics

Each case is warmed up before 100 measured trials, with case order randomized each round. Ready and Exit are summarized separately, and successful tail samples remain included. Main measurements and detailed phase diagnostics run in separate batches; warmups are excluded from results.

Every pVisor sample requires completed records and a zero exit code; VM samples also verify actual `virtual_machine` isolation. Failures or missing readiness output stop measurement and cannot count as faster samples. Percentiles describe the batch rather than guarantee long-term tails or performance on other machines.

??? note "macOS paired statistics and control settings"

    The main matrix contains 1,000 successful samples, with 80 additional diagnostic samples and 70 discarded warmups: 1,150 launches total. After timing, VM trials require a completed Bundle, zero exit status and `virtual_machine` isolation. Failures, missing markers and incorrect isolation abort measurement rather than count as fast samples.

    Case order is randomized each round. Subtract trimmed Ready from official Ready at the same shape and round, then take the median of differences. The 95% interval uses 5,000 fixed-seed bootstrap resamples. It describes this batch's variation, not systematic differences across machines or workloads.

    The original full matrix disables `PVISOR_STARTUP_TIMING`, with diagnostics enabled in a separate batch; the later P0 remeasurement enables routine logging. Each main trial uses a new working directory, skips personal Agent defaults, disables OverlayNet and inherits stdio, without Gateway or TUI. Native shell and host execution provide context; macOS and Alpine use different shell builds, so subtracting their times does not precisely isolate virtualization overhead.

### Routine startup diagnostics

Startup logs help identify which phase accounts for the wait. They are enabled by default; set `PVISOR_STARTUP_TIMING=0` to disable them. This article retains both the original matrix without logs and the latest remeasurement with logs enabled. Use results from the same batch when comparing variants.

??? note "Log fields and collection"

    The current implementation emits `pvisor-startup level=info` checkpoints by default, without a debugging switch. Fields include Unix `timestamp_ms`, `pid`/`ppid`, JSON-quoted `run_id`, `stage`, `monotonic_us` and `process_elapsed_us`. Early process events use `"-"` before Run identity exists; correlate them by PID with later identified events. Parent and runner share the same Run ID. Compute intervals using the host monotonic clock, not the adjustable wall clock.

    Ordinary CLI diagnostics use stderr and can be retained by production collectors. TUI diagnostics use the existing frontend log; a pre-opened dedicated descriptor sends VM runner diagnostics to the same channel without polluting the Agent terminal. Records include no commands, environment variables or credentials. Output is best-effort, adds no fsync and is not recovery metadata.

    `PVISOR_STARTUP_TIMING=0` explicitly suppresses checkpoints for uninstrumented benchmarks; unset and `1` both enable logging. Historical measurements in this article use archived artifacts and do not establish zero overhead from routine logging. `cli.session_started` and `runner.vmm_built` mean Session started and VMM constructed, respectively, not guest or application readiness. There is no generic guest-ready notification yet.

## 4. Experimental data and analysis

Measured median startup for new VMs is on the order of **0.1–0.2 seconds**. macOS data includes firmware comparisons, phase diagnostics and controlled optimization remeasurements; Linux data includes three memory shapes and host controls. Each dataset's complete results and analysis follow separately.

### macOS / HVF {#macos}

The latest Apple M4 remeasurement has median readiness **84.35 ms** and P95 **112.14 ms**. [Controlled remeasurement](#macos-p0) records those results; the full matrix and phase accounting below come from earlier independent batches and remain separate.

#### Full matrix: end-to-end results without diagnostic logging {#macos-results}

![Ready P50, P95 and P99 for official and trimmed firmware across four VM shapes](../../assets/benchmarks/startup-firmware-latency.svg)

Figure 1: Readiness across VM shapes. Panels use different x-axis ranges. P99 retains tail samples; a better median does not mean every launch improves equally.

All values are milliseconds, N=100 per row. `direct` is native shell, `host` is the pVisor host executor, and VM names encode firmware, vCPU count and MiB.

| Case | Ready P50 | Ready P95 | Ready P99 | Exit P50 | Exit P95 |
|---|---:|---:|---:|---:|---:|
| `direct` | 5.16 | 7.36 | 8.15 | 5.44 | 7.66 |
| `host` | 31.49 | 37.34 | 44.08 | 56.63 | 70.56 |
| `official-1cpu-128` | 125.49 | 140.99 | 146.79 | 202.61 | 302.99 |
| `trimmed-1cpu-128` | 81.03 | 99.55 | 127.66 | 138.58 | 200.79 |
| `official-2cpu-128` | 117.77 | 144.38 | 229.53 | 190.21 | 291.47 |
| `trimmed-2cpu-128` | 82.64 | 98.98 | 103.31 | 142.56 | 194.52 |
| `official-4cpu-128` | 108.68 | 121.24 | 132.39 | 188.59 | 262.58 |
| `trimmed-4cpu-128` | 85.15 | 103.62 | 400.23 | 153.01 | 199.25 |
| `official-2cpu-2048` | 125.15 | 143.53 | 156.74 | 211.97 | 314.88 |
| `trimmed-2cpu-2048` | 92.71 | 108.48 | 167.57 | 167.68 | 252.12 |

With 100 samples, P99 is close to the observed maximum and cannot guarantee long-term tail latency. Background load, power, APFS persistence and scheduling affect later batches. The earlier roughly 84 ms observation came from another batch; it is not a fixed performance constant.

The batch also contains substantial tails: trimmed 4 vCPU / 128 MiB has 2/100 trials above 200 ms, maximum 412.91 ms; official 2 vCPU / 128 MiB has 3/100, maximum 389.00 ms. Rounds 15 and 68 each contain multiple slow cases. Main samples lack fine-grained diagnostics, so persistence, scheduling or other host activity cannot be assigned as the cause. All successful samples remain included; outliers were not removed to improve P99.

##### Paired firmware improvement

| vCPU / MiB | Paired saving P50 | Bootstrap 95% CI | Trimmed faster |
|---|---:|---:|---:|
| 1cpu-128 | 44.62 | [42.60, 47.18] | 98/100 |
| 2cpu-128 | 33.79 | [32.35, 37.14] | 99/100 |
| 4cpu-128 | 23.88 | [22.72, 25.15] | 96/100 |
| 2cpu-2048 | 34.94 | [32.21, 37.13] | 98/100 |

Median paired savings need not equal the difference between group medians. More vCPUs or RAM do not necessarily start faster. This matrix uses no snapshots, resident VM pool or memory-sharing scan.

#### Where startup time goes

Diagnostics identify host preparation and guest startup through output arrival as the two main costs, each around **30 ms**; VMM construction takes about **3 ms**. Further reductions should focus on Run records and filesystem preparation, and on the guest startup path.

![Six startup phase means for trimmed firmware at 2 vCPU and 128 MiB, totaling 87.05 ms](../../assets/benchmarks/startup-phase-waterfall.svg)

Figure 2: Phases follow startup order; horizontal position shows cumulative elapsed time. Diagnostic means sum to 87.05 ms; the table also lists P50. The guest phase includes initialization, workload execution and output arrival.

These independent diagnostics use trimmed firmware, 2 vCPU / 128 MiB and N=20. Diagnostic Ready P50 is 87.67 ms. Subtracting it from the main batch does not estimate logging overhead.

| Boundary | P50 (ms) | Mean (ms) |
|---|---:|---:|
| `parent_load` | 7.14 | 7.26 |
| `parent_prepare` | 33.73 | 34.02 |
| `runner_load` | 6.17 | 6.28 |
| `runner_prepare` | 4.78 | 4.74 |
| `vmm_build` | 3.19 | 3.30 |
| `guest_and_output` | 31.08 | 31.46 |

??? note "Phase boundaries and clock checks for verification"

    Boundaries are harness start → parent main → runner spawn begins → runner main → libkrun entry → VMM construction completes → marker received. Host checkpoints and harness share `CLOCK_MONOTONIC`. Each sample's six phases must close exactly to its Ready interval. Phase means also add; independent medians generally do not.

    `guest_and_output` includes guest boot, PID1 initialization, shell exec, virtio console transport and host reader scheduling. Guest dmesg uses a different clock and cannot be subtracted from host timestamps. Earlier auxiliary logs for the same artifacts put official kernel-to-`Run /init.krun` around 42–43 ms and trimmed around 21 ms; that historical batch is excluded from this table.

Nested preparation spans follow. Storage contains record and overlay work; do not add all three.

| Nested span | P50 (ms) |
|---|---:|
| `storage` | 30.72 |
| `record` | 18.02 |
| `overlay` | 12.15 |
| `agentctl` | 0.14 |
| `ram` | 0.18 |
| `spec` | 0.11 |
| `attestation` | 4.12 |

Durable records and overlay preparation dominate parent preparation. Runner preparation also includes device configuration and attestation. JSON parsing or one virtio-fs log line cannot explain the whole guest interval.

#### Validated and retained optimizations

![Ready P50 before and after persistence and boot-entropy changes in two independent historical experiments](../../assets/benchmarks/startup-retained-optimizations.svg)

Figure 3: Independent historical comparisons validate persistence and boot entropy changes. Each uses 50 pairs, with different shapes and batches; they are not a continuous optimization timeline and gains cannot be added.

##### Remove duplicate persistence while preserving Run semantics

Non-Gateway startup combines initial RunRecord writes. Unchanged indexes reuse their inode and contents while retaining required file and directory synchronization; invalid indexes, permissions and moved paths are repaired. Runner specs use private temporary IPC files held by the parent, without long-lived-record fsync requirements.

An earlier 50-pair, no-log experiment at 2 vCPU / 2 GiB with identical trimmed firmware reduced Ready P50 from 163.522 to 136.217 ms. Median paired savings were 24.507 ms, with 48/50 pairs faster. Parent main-to-spawn P50 fell from 54.804 to 34.675 ms. This validates persistence changes within that batch; do not concatenate it with today's numbers as a single experimental curve.

##### Supply fresh boot entropy

The aarch64 FDT supplies a fresh 32-byte `rng-seed` for every new VM from the host OS random source. RNG failure stops boot instead of falling back to a fixed seed. Linux obtains early entropy without removing guest DRBG or entropy health checks.

An earlier 50-pair, no-log experiment at 2 vCPU / 128 MiB reduced Ready P50 from 129.270 to 82.268 ms. Paired savings were 47.614 ms, 95% CI [46.590, 48.750], with 50/50 pairs faster. The diagnostic CPU_ON interval fell from about 49.849 to 4.080 ms. That interval includes its surrounding startup path, not 50 ms of pure PSCI execution.

##### Trim firmware to actual VM and Agent requirements

Unused GPU/display/input hardware, rare filesystems and crypto algorithms, debugging exports and unnecessary memory/power capabilities were removed. Required virtio, virtio-fs, console, networking and crypto remain. Algorithm self-tests and Jitterentropy health checks are separate mechanisms; security checks were not indiscriminately disabled.

Today's paired data validates the combined trimmed artifact, not an exact contribution from each removed driver. Smaller kernel code can reduce mapping, initialization and cache costs, but file size alone does not determine boot time. The customized libkrunfw configuration remains the authority for enabled capabilities.

##### Remove experiments without stable benefits

The compressed init argv transport was removed; guest initialization still reads bounded JSON. The kernel random-capability cache patch remains experimental because additional benefits after FDT seeding were unstable. A 20-pair cleanup comparison showed no stable regression. Detailed per-exit/MMIO and virtio-fs instrumentation was removed; lightweight, default-off host checkpoints remained at that point; this mechanism now provides the routine diagnostics described above.

#### Controlled remeasurement of both P0 changes {#macos-p0}

The baseline is fixed commit `52e77c60d6352960a4d2ab4ef8661f3d5b1b2797`. Changes are applied sequentially, excluding concurrent HVF/VMM working-tree edits. All release artifacts use one isolated source and target directory: `baseline` is unchanged, `attestation` removes temporary receipt synchronization, and `both` additionally deduplicates directory barriers. Firmware and prepared rootfs remain identical; routine checkpoints are enabled throughout (`1` is equivalent to default behavior). Official firmware was not remeasured; its earlier numbers are not a same-round control for these results.

Round one retains complete receipt writes, normal-exit validation and failed-entry truncation, removing only two `sync_data()` calls. Round two creates the full directory tree before syncing the first new directory's parent and each new directory once. N new levels require N+1 directory barriers instead of 2N. Existing-directory behavior and sync-error propagation retain their contracts; RunRecord and index publication remain synchronous atomic operations.

At 2 vCPU, each artifact has 100 formal samples and 5 warmups at 128 and 2048 MiB: 600 formal samples and 30 warmups. All six cases are randomized each round. Values are milliseconds; Ready still runs from before process creation to workload stdout marker, while Exit is measured separately. See the [P0 raw JSON](../../assets/benchmarks/startup-p0-20261003.json) for samples and hashes.

| MiB | Variant | Ready P50 | Ready P95 | Ready P99 | Exit P50 |
|---:|---|---:|---:|---:|---:|
| 128 | `baseline` | 90.50 | 107.99 | 717.92 | 155.10 |
| 128 | `attestation` | 84.69 | 101.89 | 149.32 | 144.91 |
| 128 | `both` | 84.35 | 112.14 | 193.52 | 144.60 |
| 2048 | `baseline` | 97.27 | 116.15 | 183.06 | 182.45 |
| 2048 | `attestation` | 94.36 | 108.69 | 503.98 | 177.42 |
| 2048 | `both` | 94.55 | 110.57 | 145.46 | 176.42 |

| MiB | Comparison | Paired saving P50 | Bootstrap 95% CI | Faster |
|---:|---|---:|---|---:|
| 128 | `baseline → attestation` | 6.61 | [2.98, 8.80] | 70/100 |
| 128 | `attestation → both` | 1.30 | [-0.98, 3.35] | 53/100 |
| 128 | `baseline → both` | 5.64 | [4.00, 8.46] | 73/100 |
| 2048 | `baseline → attestation` | 3.61 | [2.01, 6.35] | 66/100 |
| 2048 | `attestation → both` | 2.31 | [-0.31, 4.42] | 56/100 |
| 2048 | `baseline → both` | 4.24 | [1.61, 5.77] | 66/100 |

| Variant (128 MiB) | Attestation P50 | Storage P50 | Parent preparation mean | Ready mean |
|---|---:|---:|---:|---:|
| `baseline` | 4.03 | 33.33 | 48.66 | 104.74 |
| `attestation` | 0.04 | 33.63 | 42.32 | 91.34 |
| `both` | 0.04 | 32.57 | 37.42 | 88.73 |

Round one has positive end-to-end paired intervals at both memory sizes. Its attestation span falls from about 4.03 to 0.04 ms; all 100 pairs at 128 MiB improve that span. Round two's independent end-to-end intervals still cross zero, so an additional stable millisecond gain is not established; redundant directory calls are demonstrably reduced. The combined changes improve the median in this batch, but 128 MiB Ready P95 changes from 107.99 to 112.14 ms, so tail latency does not improve universally.

An earlier batch used 50 samples per case, with 1-minute host load average falling from 18.38 to 8.66 and all end-to-end intervals crossing zero. Its [complete raw JSON](../../assets/benchmarks/startup-p0-20261003-first.json) remains archived and is not pooled with this batch. The second batch's load average is 5.67 → 6.87: lower, but not a fully isolated idle host. All successful tail samples remain included.

Savings are median same-round differences, with 5,000 bootstrap resamples. Median gains from the two rounds cannot be added. Nested-span medians do not sum to total latency either. Intervals crossing zero do not establish stable improvement; exit tails and readiness tails remain separate observations.

Checks cover four sync targets for three new directory levels, no additional barriers for existing directories, and immediate sync-error propagation. All artifacts also pass real-VM success, nonzero guest exit and deadline checks: the first two retain enforcement evidence; deadlines leave it unknown. Temporary receipts are not recovery state. These checks validate runtime semantics, not actual power-loss behavior.

#### Next optimization boundaries

Temporary attestation synchronization and duplicate barriers within directory creation are addressed by the P0 experiments above. Next investigate barriers across RunRecord/index publication and preparation steps, then process loading and PID1 work, preserving failure, cancellation and attestation semantics.

RunRecord is authoritative state before execution. Required persistence cannot become an unawaited background task. Merge redundant barriers and parallelize independent work where safe, then validate with paired measurements and failure/recovery checks.

### Linux / KVM {#linux}

#### New VM startup results {#linux-results}

At 2 vCPU / 128 MiB, median readiness is **172.69 ms** and P95 is **180.37 ms**. Median readiness is approximately **175 ms** at 256 MiB and **220 ms** at 2 GiB. This workload only prints one line; additional configured RAM does not reduce startup waiting.

Values below are milliseconds, with 100 measured samples per row. Ready ends at the first command output; Exit includes finalization. Host controls run in the same rounds, and all VM samples pass isolation checks.

| Case | Ready P50 | Ready P95 | Ready P99 | Exit P50 | Exit P95 |
|---|---:|---:|---:|---:|---:|
| Native shell | 0.66 | 0.75 | 0.86 | 0.72 | 0.83 |
| pVisor host | 6.62 | 7.08 | 8.00 | 13.18 | 13.57 |
| KVM / 2 vCPU / 128 MiB | 172.69 | 180.37 | 182.39 | 233.97 | 254.22 |
| KVM / 2 vCPU / 256 MiB | 175.41 | 183.74 | 186.86 | 233.99 | 254.13 |
| KVM / 2 vCPU / 2048 MiB | 220.47 | 228.20 | 231.22 | 284.08 | 294.39 |

In this batch, new VMs become ready in about **0.2 seconds**, with exit completion around **0.23–0.28 seconds**. Evaluate those boundaries separately. These are fresh VM launches; [snapshot restoration](vm-memory/index.md#linux-snapshot) is measured separately. Linux has no detailed startup phase accounting in this batch, so macOS phase proportions cannot be applied to it.

## 5. Reproduction and raw data

### macOS / HVF

Prepare the release CLI, Alpine rootfs and both firmware directories, then run:

```bash
python3 benchmark/pvisor/vm_ready.py \
  --binary target/release/pvisor \
  --rootfs target/guest-init-benchmark/rootfs \
  --official target/firmware-official-compare-20261003/official \
  --trimmed target/firmware-official-compare-20261003/trimmed \
  --output target/vm-startup-new \
  --samples 100 --warmups 5 --profile-samples 20
```

The output directory must not exist. The harness uses the Python standard library and existing percentile/Bundle validation helpers. Main and diagnostic batches are separate, warmups are excluded, and outputs include `results.json`, incrementally flushed `samples.jsonl`, input hashes and per-trial stdout/stderr. Its schema is `pvisor-vm-readiness/v1`; do not mix it with `startup.py` command-completion measurements.

Local logs are in `target/vm-startup-20261003/`; the raw JSON link above contains the report. Historical persistence, entropy and firmware experiments remain under `review_project/03-modules/`. The older C/Rust guest-init runner comparison remains in `benchmark/pvisor/README.md`; it excludes full CLI preparation and is not directly comparable with this table.

The measured harness snapshot is retained in `review_project/06-evidence/vm-startup-20261003/`; the reusable harness subsequently added explicit pipe closure and `PVISOR_STARTUP_TIMING=0` for uninstrumented samples; the snapshot preserves the measured implementation.

### Linux / KVM {#linux-reproduce}

Use a prepared guest root and firmware directory with a new output directory. The script reuses first-output timing and run validation, copying the CLI and firmware to pin artifact identity.

```bash
python3 benchmark/pvisor/linux_vm_ready.py \
  --binary target/release/pvisor \
  --rootfs / \
  --firmware /path/to/libkrunfw-5.5.0-directory \
  --output target/vm-startup-linux-new \
  --samples 100 --warmups 5
```

[Linux raw samples, environment and artifact hashes](../../assets/benchmarks/startup-linux-20261003.json). Per-trial stdout/stderr, warmup logs and fixed binaries remain in local `target/local-vm-validation-20261003/startup-linux-100/`. Changing artifacts or rootfs requires a new measurement batch with earlier records retained.

## 6. Unmeasured scenarios

Docker, Firecracker, a3s and real Agent readiness remain unmeasured. First image preparation, cold disk caches, TUI, Gateway, concurrent density and service health checks require separate matrices.
