# How long does a usable environment take to start?

## Conclusions {#conclusions}

**In the independent-condition retest after the reboot on 2026-10-11, median Ready times are 33.52 ms for pVisor host, 50.59 ms for staged, 130.71 ms for VM, 105.03 ms for Docker, 287.13 ms for Firecracker MMIO, 663.25 ms for QEMU and 260.10 ms for microvm. Ready and normal Exit share sixty launches per condition. Use these tables for the measured artifacts; historical randomized-pair rankings and intervals do not carry over to the new cohorts.**

The selection summary below retains the scope of its historical cohorts.

| Need | Selection implication |
| --- | --- |
| Frequent short commands | Host and staged startup waits are in the tens of milliseconds |
| Frequently created disposable VMs | pVisor VM waits about 0.1 seconds for the first command |
| Complete Agent tool tasks | Use tool and repair results to choose an execution mode |

<!-- measurement-status:start -->

New experiments and historical data are published separately. Each table retains its timing scope and inputs from the design and provenance; distinct cohorts are not pooled.

<!-- measurement-status:end -->

## Motivation {#motivation}

Disposable environments repeatedly pay for the first command. Choosing host, staged, containers or VMs requires knowing their startup waits and the cost of the isolation you need. Short tasks also pay for exit and tools.

## Experiment design {#interpretation}

The historical Ready / Exit comparison uses Linux x86_64 / KVM, AMD Ryzen 7 9700X, Fedora host. The controls are native, pVisor host, pVisor staged, pVisor VM, rootless Docker / overlay2, Firecracker PCI, QEMU q35 and QEMU microvm. Launch trees are pinned to CPUs 0,1; every VM has 2 vCPU / 128 MiB. Processes and containers have no memory limit.

Firecracker, QEMU and microvm use the full Fedora distribution kernel, Linux 7.2.8-200.fc44.x86_64. pVisor's virtualization is based on libkrun and uses its companion libkrunfw project's customized kernel, Linux 6.12.109.

Prepared userspace environments run the same `/bin/sh` output probe. Each execution uses a fresh environment and private workspace; every VM boots afresh, without snapshots or environment pools. Warm page caches, three warmups and sixty measured samples per backend, with randomized interleaving within each measurement group. Environment preparation and input copying are outside startup timing. Kernel, device and storage paths differ, so results cover each complete startup configuration. This probe does not cover full distribution boot, cold images, Agent CLI initialization, concurrent capacity or snapshot restoration.

Historical Ready ends at the checked first shell marker. Exit includes execution and teardown, requiring correct output and a normal zero exit. Firecracker is stopped under control after checking markers and results and confirming no panic; only Ready is reported. Failures, panic, mismatched output or changed artifacts are excluded from timing distributions; valid slow samples are retained.

The historical hyperfine measurements used the independent JSON templates available at the time, executed by semspec bench, with three warmups and sixty samples. They measure the complete command through normal exit, not first-output Ready; input preparation and correctness checks are outside timing. Conditions sample independently, without pooling batches or deriving paired improvement percentages. Those batches validate the observed executor in each Run Bundle: host and staged use `rootless_process` on this machine, and VM uses `vm`; artifact digests and batch provenance are recorded separately.

## Data and analysis {#results}

### Python: Ready / Exit from the same launch {#python-ready-exit}

Each process launch records its start, receipt of the complete `REFERENCE_READY` line and
normal exit. Three warmups and sixty formal samples are fixed; both tables below project
columns from one command's output, without repeated sampling. Inputs are a prepared rootfs,
2 vCPU / 128 MiB, warm caches and CPU 0,1 affinity. Python `perf_counter_ns` excludes
interpreter startup and input preparation. Ready includes process creation, pipe reading and
observer scheduling; Exit uses the same start through observed normal exit. Output, Run Bundle
and unchanged inputs must pass for every sample.
New Firecracker uses the stock kernel, the same initrd and virtio-MMIO. Two PCI preflights failed to exit normally after successful guest markers and were rejected. Historical Firecracker PCI Ready and new MMIO use different configurations; subtracting them does not establish an improvement.

#### Ready

{{ table("startup-timing.ready.render.json") }}

#### Exit

{{ table("startup-timing.exit.render.json") }}

The [paired-sample statistics CSV](../../../overrides/assets/benchmarks/data/startup/startup-timing.csv) and [provenance JSON](../assets/benchmarks/startup-timing.provenance.json)
identify each condition’s latest successful independent batch. Standard deviation uses the formal samples and sample formula. These are
Python event-clock data, independent of the hyperfine completion experiment. Historical results
from different artifacts are not pooled. Rerun the entire table with `just benchmark startup/timing`,
or just VM with `just benchmark startup/timing "pvisor vm"`.
The earlier VM-only cohort remains separate in local evidence.

All eight conditions were remeasured after the reboot on 2026-10-11. The
complete before/after CSV/JSON remains local, retaining full-precision statistics
and provenance digests for both independent cohorts. The measured pVisor binary SHA-256 is
`d3f09687cca60b617629c215fa1359c441ab6ac19f8c63b68f1dcdf7614e6b30`, unchanged across the reboot;
userspace inputs, measured commands, timing and statistics functions are also unchanged.
Failure handling and provenance fields were added to the harness outside timing.
VM median Ready fell from 179.22 ms to 130.71 ms (a descriptive change of -27.07%), and
Exit from 297.79 ms to 243.53 ms. Host median Ready fell from 37.45 ms to 33.52 ms, but
its standard deviation rose from 1.38 ms to 29.70 ms, with the final seventeen formal
samples noticeably slower. All valid slow samples are retained. These independently sampled,
warm-cache cohorts do not establish a causal reboot effect on their own. The historical VM
Ready of about 101 ms used different binary and firmware artifacts; attributing the remaining
gap requires controls with fixed artifacts.

### Startup and exit times {#reference-startup}

<a id="reference-exit"></a>
<a id="legacy-startup"></a>

Historical controls: unit ms, N=60 per backend, zero measured failures. Process/container and VM measurements were collected separately, with each row retaining its own statistics; all four VM controls were randomly interleaved within one group. P95 describes the distribution rather than guaranteeing stable tail latency.

| Runtime | Valid / failed | Ready P50 ms | Ready P95 ms | Exit P50 ms | Exit P95 ms |
| --- | --- | --- | --- | --- | --- |
| Native | 60 / 0 | 1.28 | 1.71 | 1.35 | 1.82 |
| pVisor host | 60 / 0 | 12.72 | 14.18 | 24.03 | 24.71 |
| pVisor staged | 60 / 0 | 25.13 | 26.85 | 33.84 | 44.55 |
| pVisor VM | 60 / 0 | 100.91 | 151.27 | 141.52 | 176.59 |
| Docker | 60 / 0 | 74.48 | 87.13 | 98.29 | 110.22 |
| Firecracker | 60 / 0 | 285.04 | 291.88 | — | — |
| QEMU | 60 / 0 | 702.98 | 789.36 | 725.55 | 818.85 |
| microvm | 60 / 0 | 321.56 | 404.39 | 347.99 | 428.27 |

Host and staged wait tens of milliseconds, making them suitable for frequently launched short commands. VMs also initialize a kernel and devices; choose isolation with complete-task timing in mind. Blank Firecracker Exit cells mean normal shutdown is unmeasured.

| VM control | Wait saved by pVisor ms | 95% interval for difference ms | Wait reduction | Control time / pVisor time |
| --- | --- | --- | --- | --- |
| Firecracker | 184.12 | 181.60–185.84 | 64.6% | 2.82× |
| QEMU | 602.06 | 582.17–635.99 | 85.6% | 6.97× |
| microvm | 220.64 | 195.91–251.74 | 68.6% | 3.19× |

Differences are differences between the two medians, with 95% intervals from 5,000 bootstrap resamples of matching rounds. All three intervals exclude zero, supporting faster pVisor VM startup for these configurations. Dedicated firmware and simplified initialization provide a shorter startup path; this comparison cannot separately quantify kernel, filesystem and VMM contributions.

For cold-image client waiting and content downloads, see the separate [on-demand image startup](lazy-image-startup.md) experiment. Image-service preparation is recorded separately, and its samples are not pooled with these prepared-environment results.

<!-- startup-hyperfine:start -->

### Historical Python: Ready-only data {#python-ready}

This experiment uses a prepared rootfs, 2 vCPU / 128 MiB, warm caches and CPU 0,1 affinity.
Three warmups and sixty formal samples are fixed in the script. Python `perf_counter_ns`
starts immediately before Popen and stops on receipt of the complete `REFERENCE_READY` line.
Process creation, pipe reading and observer scheduling are included; interpreter startup,
input preparation, subsequent exit and cleanup are excluded. Correct output, normal zero exit
and a valid Run Bundle remain required after the clock stops.

|  | N | Ready median ms | Ready mean ms | Ready SD ms | Ready min ms | Ready max ms |
| --- | --- | --- | --- | --- | --- | --- |
| pVisor VM | 60 | 132.95 | 132.51 | 5.09 | 123.00 | 147.53 |

The complete Ready statistics CSV and provenance JSON remain local,
retaining the latest successful batch for this condition. Standard deviation uses formal samples
and the sample formula. These are Python clock measurements, separate from hyperfine completion.
Historical Ready used a different binary artifact; timing boundaries alone cannot explain the difference.
The current Ready / Exit experiment uses `just benchmark startup/timing "pvisor vm"`; this section retains the older Ready-only cohort.

### Historical Hyperfine: direct launch to normal exit {#hyperfine-startup}

Each row uses that condition's latest successful independent batch, without pooling samples. Three warmups and sixty measured samples; median, mean and standard deviation come from the same raw hyperfine export, in ms. Python preparation and checks, semspec processing and external CPU affinity setup are outside timing.

|  | N | Median ms | Mean ms | SD ms |
| --- | --- | --- | --- | --- |
| Native | 60 | 0.42 | 0.42 | 0.02 |
| pVisor host | 60 | 208.79 | 217.94 | 34.13 |
| pVisor staged | 60 | 153.85 | 154.58 | 5.88 |
| pVisor VM | 60 | 278.83 | 305.77 | 63.45 |
| Docker | 60 | 163.92 | 168.87 | 21.38 |
| Firecracker | — | — | — | — |
| QEMU | 60 | 722.08 | 711.24 | 26.75 |
| QEMU microvm | 60 | 339.47 | 355.08 | 86.50 |

Empty conditions have no validated new batch. This measures the command through normal exit, not first-output Ready. Rows show independent levels, without paired improvement percentages or confidence intervals. Commands, extrema and per-row provenance remain in the complete local CSV/JSON.

<details markdown="1">
<summary>Historical complete commands (including wrappers)</summary>

|  | N | Median ms | Mean ms | SD ms |
| --- | --- | --- | --- | --- |
| Native | 60 | 2.13 | 2.17 | 0.23 |
| pVisor host | 60 | 165.12 | 315.84 | 259.74 |
| pVisor staged | — | — | — | — |
| pVisor VM | — | — | — | — |
| Docker | 60 | 167.27 | 381.08 | 509.32 |
| Firecracker | — | — | — | — |
| QEMU | 60 | 748.41 | 896.73 | 319.03 |
| QEMU microvm | 60 | 353.76 | 451.13 | 219.93 |

The local historical CSV includes an outer shell, taskset and timeout. Its original scope is preserved. These are different batches and scopes; subtracting them cannot estimate wrapper cost or establish product performance improvements.

</details>

<!-- startup-hyperfine:end -->

### Sources and reproduction {#run}

Complete CSV/JSON for startup statistics, VM differences and 95% intervals, source and artifact provenance, and raw records remain in their local cohorts or the documentation data archive. See the [startup manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark.new/startup/README.md) to reproduce the current Ready / Exit experiment.

[Method](methodology.md) · [Historical reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#explicit-firecracker-kernel-controls)
