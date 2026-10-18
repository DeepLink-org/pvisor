# How long does a usable environment take to start?

## Motivation {#motivation}

Agent workloads arrive in bursts of short-lived calls: a task finishes and its environment is discarded, then the next task builds a new one. Each call pays the startup wait again, so the cost accumulates with call volume. Startup latency decides how soon the first command runs and how often a developer is willing to create an isolated environment for one task.

pVisor's VM work brings startup close to a container: median Ready 130.71 ms against Docker rootless at 105.03 ms, and below the reference VMs (Firecracker MMIO 287.13 ms, QEMU microvm 260.10 ms, QEMU q35 663.25 ms). Host and staged runs are faster still, at 33.52 ms and 50.59 ms.

## Experiment design {#interpretation}

Measure the interval from launch to the first checked shell output as environment Ready time. That interval is how quickly the system answers a caller: a shorter Ready time means the system handles bursts of large-scale requests better.

The comparison runs on Linux x86_64 / KVM, AMD Ryzen 7 9700X, Fedora host. The controls are native, pVisor host, pVisor staged, pVisor VM, rootless Docker / overlay2, Firecracker with virtio-MMIO, QEMU q35 and QEMU microvm. Launch trees are pinned to CPUs 0,1; every VM has 2 vCPU / 128 MiB. Processes and containers have no memory limit.

Firecracker, QEMU and QEMU microvm use the full Fedora distribution kernel, Linux 7.2.8-200.fc44.x86_64. pVisor's virtualization is based on libkrun and uses its companion libkrunfw project's customized kernel, Linux 6.12.109.

Prepared userspace environments run the same `/bin/sh` output probe. Each execution uses a fresh environment and private workspace; every VM boots afresh, without snapshots or environment pools. Warm page caches, three warmups and sixty measured samples per backend, with randomized interleaving within each measurement group. Environment preparation and input copying are outside startup timing. Kernel, device and storage paths differ, so results cover each complete startup configuration. The probe does not cover full distribution boot, cold images, Agent CLI initialization, concurrent capacity or snapshot restoration.

Ready ends at the checked first shell marker. Firecracker is stopped under control after checking markers and results and confirming no panic. Failures, panic, mismatched output or changed artifacts are excluded from timing distributions; valid slow samples are retained.

## Data and analysis {#results}

### Environment Ready time {#ready}

Eight conditions measured on 2026-10-11, N=60 formal samples after three warmups. Inputs are the prepared rootfs, 2 vCPU / 128 MiB for VMs, warm caches and CPU 0,1 affinity. Python `perf_counter_ns` starts immediately before process creation and stops on receipt of the complete `REFERENCE_READY` line: process creation, pipe reading and observer scheduling are included, interpreter startup and input preparation are not. Output, Run Bundle and unchanged-input checks must pass for every sample.

{{ table("startup-timing.ready.render.json") }}

pVisor VM starts at a median 130.71 ms, between rootless Docker at 105.03 ms and the reference VMs at 260–663 ms. Host and staged skip the kernel and device path and stop at 33.52 ms and 50.59 ms.

The table selects the latest validated row for each `condition` from the [statistics CSV](../../../overrides/assets/benchmarks/data/startup/startup-timing.csv); `condition` and `batch` together identify a result. Standard deviation uses the formal samples and the sample formula. Rerun the table with `just benchmark startup/timing`, or only the VM with `just benchmark startup/timing "pvisor vm"`.

For cold-image client waiting and content downloads, see the separate [on-demand image startup](lazy-image-startup.md) experiment. Image-service preparation is recorded separately, and its samples are not pooled with these prepared-environment results.

### Sources and reproduction {#run}

Complete statistics, source and artifact provenance, and raw records remain in their local cohorts or the documentation data archive. See the [startup manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark.new/startup/README.md) to reproduce the experiment, and the [provenance JSON](../assets/benchmarks/startup-timing.provenance.json) for the measured artifacts.

[Comparison method](index.md#interpretation)
