# How long does a usable environment take to start?

## Motivation {#motivation}

For local API callers repeatedly creating the same size of sandbox, reserving a VM slot cuts startup wait on a hit; an empty pool still requires a cold start. For direct launches with prepared userspace and warm page caches, pVisor VM reaches its first output close to a container and before the tested reference VMs. Host and staged runs wait less.

For serial local API requests, a warm hit took the median wait for `Running` from 77.89 ms down to 12.48 ms, and the median wait for the first successful command from 94.54 ms down to 38.18 ms. In the separate command-line experiment, pVisor VM reached Ready in a median of 130.71 ms and rootless Docker in 105.03 ms.

## Experiment design {#interpretation}

Measure the intervals from sending a create request to receiving `Running` and to completing the first verified command as the API creation and first-use waits. The separate command-line experiment measures environment Ready from process launch to the first verified shell output. Shorter waits let the caller use a fresh environment sooner.

### VM warm pool through the local API {#daemon-warm-pool-design}

The same optimized pVisor daemon runs on an AMD Ryzen 7 9700X with Fedora Linux 7.3.3 and KVM, using a prepared Ubuntu 24.04 image and 4 vCPUs with a 256 MiB process-tree memory limit per instance. The two daemons, observer and VMs share host CPUs 4–7, a four-CPU quota, a 2 GiB memory limit and no swap. Reserving one matching VM is the only configuration difference.

Loopback HTTP timing includes attaching the image and actual service readiness. It excludes daemon startup, image import and waiting for pool refill. Serial cold starts and confirmed hits are interleaved in seeded random order. Each round also sends two matching requests together with only one idle slot and records the hit separately from the fallback. Preflight and three correctness warmup rounds are excluded; every subsequent valid operation is retained, including the sampler's adaptive warmup. Each trial verifies slot ownership, resource limits, command output, private writes and cleanup. Any correctness failure or OOM invalidates the batch.

### Direct executor launches {#direct-launch-design}

The comparison runs on Linux x86_64 / KVM, AMD Ryzen 7 9700X, Fedora host. The controls are native, pVisor host, pVisor staged, pVisor VM, rootless Docker / overlay2, Firecracker with virtio-MMIO, QEMU q35 and QEMU microvm. Launch trees are pinned to CPUs 0,1; every VM has 2 vCPU / 128 MiB. Processes and containers have no memory limit.

Firecracker, QEMU and QEMU microvm use the full Fedora distribution kernel, Linux 7.2.8-200.fc44.x86_64. pVisor's virtualization is based on libkrun and uses its companion libkrunfw project's customized kernel, Linux 6.12.109.

Prepared userspace environments run the same `/bin/sh` output probe. Each execution uses a fresh environment and private workspace; every VM boots afresh, without snapshots or environment pools. Warm page caches, three warmups and sixty measured samples per backend, with randomized interleaving within each measurement group. Environment preparation and input copying are outside startup timing. Kernel, device and storage paths differ, so results cover each complete startup configuration. The probe does not cover full distribution boot, cold images, Agent CLI initialization, concurrent capacity or snapshot restoration.

Ready ends at the checked first shell marker. Firecracker is stopped under control after checking markers and results and confirming no panic. Failures, panic, mismatched output or changed artifacts are excluded from timing distributions; valid slow samples are retained.

## Data and analysis {#results}

### Local API wait for a new instance {#daemon-warm-pool}

| Request condition | N | Create to `Running` P50 / P95 (ms) | Create to first verified command completion P50 / P95 (ms) |
| --- | ---: | ---: | ---: |
| Serial, default cold start | 41 | 77.89 / 85.90 | 94.54 / 102.53 |
| Serial, warm hit | 41 | 12.48 / 16.59 | 38.18 / 40.46 |
| Two simultaneous requests, warm hit | 41 | 14.04 / 17.67 | 48.20 / 55.74 |
| Two simultaneous requests, empty-pool fallback | 41 | 99.67 / 110.73 | 117.99 / 127.79 |

In the serial pairs, the median time saved was 65.63 ms to `Running` (95% bootstrap interval: 63.48–66.76 ms) and 56.72 ms to command completion (54.98–58.59 ms). No request returned `Running` in under 1 ms. Of the 82 requests in the two-request experiment, 41 hit and 41 fell back. This is what happens with one slot available for each burst, not a production hit rate. The difference between the concurrent fallback and serial cold timings cannot be attributed to pooling alone.

Measured on 2026-11-22, with no failures or valid operations excluded. Treat P95 as indicative. These timings cover a local daemon, not SDK or remote network overhead, and **do not form a cross-runtime ranking with the command-line results below**. See the [warm-pool reproduction steps](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/startup/README.md#daemon-vm-warm-pool).

### Environment Ready time {#ready}

Eight conditions measured on 2026-10-11, N=60 formal samples after three warmups. Inputs are the prepared rootfs, 2 vCPU / 128 MiB for VMs, warm caches and CPU 0,1 affinity. Python `perf_counter_ns` starts immediately before process creation and stops on receipt of the complete `REFERENCE_READY` line: process creation, pipe reading and observer scheduling are included, interpreter startup and input preparation are not. Output, Run Bundle and unchanged-input checks must pass for every sample.

|  | N | Ready median ms | Ready mean ms | Ready SD ms | Ready min ms | Ready max ms |
| --- | --- | --- | --- | --- | --- | --- |
| Native | 60 | 0.48 | 0.48 | 0.02 | 0.45 | 0.55 |
| pVisor host | 60 | 33.52 | 50.48 | 29.70 | 30.58 | 128.65 |
| pVisor staged | 60 | 50.59 | 52.77 | 17.87 | 46.18 | 188.29 |
| pVisor VM | 60 | 130.71 | 130.10 | 6.16 | 119.00 | 149.99 |
| Docker | 60 | 105.03 | 105.57 | 7.22 | 90.52 | 126.04 |
| Firecracker | 60 | 287.13 | 287.07 | 5.89 | 269.08 | 301.73 |
| QEMU | 60 | 663.25 | 667.53 | 28.73 | 631.38 | 742.13 |
| QEMU microvm | 60 | 260.10 | 259.53 | 9.57 | 243.28 | 288.39 |

pVisor VM starts at a median 130.71 ms, between rootless Docker at 105.03 ms and the reference VMs at 260–663 ms. Host and staged skip the kernel and device path and stop at 33.52 ms and 50.59 ms.

The table selects the latest validated row for each `condition` from the [statistics CSV](../assets/benchmarks/data/startup/startup-timing.csv); `condition` and `batch` together identify a result. Standard deviation uses the formal samples and the sample formula. Rerun the table with `mise run benchmark -- startup/timing`, or only the VM with `mise run benchmark -- startup/timing "pvisor vm"`.

For cold-image client waiting and content downloads, see the separate [on-demand image startup](lazy-image-startup.md) experiment. Image-service preparation is recorded separately, and its samples are not pooled with these prepared-environment results.

### Sources and reproduction {#run}

Complete statistics, source and artifact provenance, and raw records remain in their local cohorts or the documentation data archive. See the [startup manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/startup/README.md) to reproduce the experiment, and the [provenance JSON](../assets/benchmarks/startup-timing.provenance.json) for the measured artifacts.

[Comparison method](index.md#interpretation)
