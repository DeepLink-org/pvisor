# Do you need pVisor if you already use Docker or devcontainer?

## Conclusions {#conclusions}

**pVisor staged finishes the tested repair task sooner than Docker, but takes longer for the seven-tool filesystem task. Evaluate stage for retained changes, selective application and conflict refusal; tool speed alone does not support replacing containers generally.**

| Need | Selection implication |
| --- | --- |
| Local execution with retained changes | Evaluate host/staged and the complete review workflow |
| Independent guest kernel | Budget both startup and VM tool waiting |
| Concurrent or idle environments | Require fixed-budget throughput and physical-memory measurements |

## Motivation {#motivation}

Writable bind mounts immediately change host files, whereas private workspaces require creation, review and application. Tool waiting and the complete change workflow both affect costs.

## Experiment design {#interpretation}

Linux x86_64, AMD Ryzen 7 9700X, Fedora 7.2.8-200.fc44.x86_64. Launched process trees and the private Docker daemon are pinned to CPUs 0,1. VMs use 2 vCPU, 128 MiB for the shell probe and 16 GiB for tools. Native/Docker memory is uncapped: this is a CPU-controlled task comparison, not a capacity comparison under identical memory limits. Host/staged use rootless_process.

All backends share offline tools and fixed inputs, with a new workspace per trial. Warm caches, three warmups and 60 measured samples per cell; backends alternate in seeded randomized order. Preparation, builds, image import and fixture resets are outside timing; complete tasks include launch and exit. Docker Engine 29.7.2 uses a private rootless **overlay2** daemon, the classic image store and writable bind mounts. Firecracker 1.13.1 PCI runs without jailer; QEMU 10.2.2 uses q35/microvm and private ext4. pVisor VM uses virtio-fs and its own firmware. Kernels, storage and staging semantics differ: these are task costs for the stated configurations, not pure VMM or security rankings.

The comparison reuses three independent workloads with Docker writable binds and pVisor staging. Docker plus Git review, devcontainer plugins and Docker Desktop are unmeasured. See the [complete review workflow](supervision-cost.md) for native Git worktree/reflink controls.

## Data and analysis {#results}

Measured on 2026-10-06: each backend/workload has 60/60 valid samples and zero measured failures. Outputs, exit and execution records must pass validation; staging also requires unchanged host originals and complete retained changes. Every valid slow sample is kept, with no timing-based exclusions. Tables normally show P50; separated distributions show cluster medians and counts. P95 is descriptive only. Raw reports, binaries and input/source manifests stay in ignored `.data/`; public CSVs retain workload, cohort and provenance associations.

### Complete-task comparison {#reference-comparison}

| Runtime | Ready P50 ms | Repair completion P50 s | Seven-tool completion P50 s |
| --- | --- | --- | --- |
| Native | 1.28 | 0.44 | 0.45 |
| pVisor host | 12.72 | 0.46 | 0.47 |
| pVisor staged | 25.13 | 0.64 | 1.09 |
| Docker rootless / overlay2 | 74.48 | 0.81 | 0.82 |
| pVisor VM | 100.73 | 3.25 | 4.27 |

See [startup](startup.md), [filesystem](filesystem.md) and [repair tasks](agent-tasks.md) for operations, timing boundaries and confidence intervals.

Docker writable mounts directly change the host; pVisor staged applies selected paths only on apply. Combine performance with [isolation scope](isolation-tests.md) and the [complete review workflow](supervision-cost.md).

### Downloads and reproduction {#run}

[Derived statistics CSV](compare-containers.csv) · [All runtime statistics](runtime-summary.csv) · [Differences and 95% confidence intervals](runtime-comparisons.csv) · [Source and artifact provenance](runtime-provenance.csv) · [Method](methodology.md) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
