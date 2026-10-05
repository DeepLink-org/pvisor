# Where does pVisor fit among existing tools?

## Main conclusions {#conclusions}

**Evaluate rootless host/staged for local tools and reviewed edits. pVisor VM starts in the lightweight-VM range but file-heavy tasks take longer than the measured Firecracker/QEMU configurations. Choose using complete task waiting, isolation and how edits reach the original directory.**

| Need | Selection implication |
|---|---|
| Local tools and retained edits | Evaluate rootless host/staged |
| Independent guest kernel | Budget complete VM tool time |
| Existing container/Git workflow | Compare costs and required review semantics |

## Motivation {#motivation}

Agent costs include tools, dependencies, tests and review beyond startup. Containers, VMs, built-in Agent sandboxes and managed cloud environments offer different boundaries and workflows. These measurements support workload-specific choices.

## Experiment design {#interpretation}

Shared Linux/x86_64 host, AMD Ryzen 7 9700X, Fedora kernel 7.2.8-200.fc44.x86_64. Launch trees and the private Docker daemon are pinned to host CPUs 0,1; guests have 2 vCPU. Host/staged use rootless_process. Shell VMs use 128 MiB; tool VMs use 16 GiB. Native/Docker memory is not capped: this controls CPU and configured guest RAM, not identical resource enforcement. Tools and inputs are prepared; each run gets a fresh workspace, warm caches, three warmups and 60 measured trials, with seeded randomized backend order. Builds, downloads and input copying are excluded.

Docker Engine 29.7.2 uses a private rootless VFS daemon and writable bind mounts. This does not represent overlay2 or Docker Desktop. Firecracker 1.13.1 PCI runs without jailer; QEMU 10.2.2 uses q35/microvm with private ext4. pVisor VM uses virtio-fs and a different kernel. Kernel, storage, devices and staging semantics remain configuration differences; these results do not isolate the VMM or FUSE alone.

Each topic defines correctness and timing. Complete Ubuntu, macOS, apply/network and version-pinned CLIs retain separate cohorts and counts. Unmeasured cloud, gVisor/Kata, memory savings and complete RL throughput do not receive numeric rankings.

## Data and analysis {#results}

### Measured levels

Startup/filesystem: 2026-10-05, repair: 2026-10-06, N=60/backend/workload, failures=0; P50 or separated cluster medians with counts. Apply: 2026-10-04, N=30/10/3 for 10/1,000/100,000 files. Network: 2026-10-04, 30 batches. CLIs: independent pinned versions.

| Question | Measured level | Selection implication |
|---|---|---|
| [Prepared startup](startup.md) | pVisor VM 99.76 ms; Firecracker 74.74 ms; QEMU microvm 86.60 ms | Lightweight-VM startup range |
| [Repair through exit](agent-tasks.md) | staged 0.68 s; VM 3.25 s; QEMU microvm 1.27 s | Complete tool waiting matters |
| [Seven tools through exit](filesystem.md) | staged 1.29 s; VM 6.66 s; Firecracker 2.16 s | VM tool/file costs remain substantial |
| [Apply](apply.md) | 10: 15.01 ms; 1,000: 836.38 ms; 100,000: 330.40 s | Git patch is faster; semantics differ |
| [Network](network.md) | host proxy 1.24 ms; native 0.95 ms / local request | Budget VM bulk transfer separately |
| [Agent CLIs](agent-tasks.md#cli-compatibility) | Pinned Codex passes; Claude/VM initialization timeout | Check the exact client version |

Docker VFS creation is expensive in this configuration, while bind-mount operation times remain useful. These totals do not rank overlay2 or Docker Desktop. Full Ubuntu boot is a different deployment choice. Raw evidence stays local; each topic links derived tables and source summaries.

### Measurement topics

[Startup](startup.md) · [Filesystem](filesystem.md) · [Agent tasks](agent-tasks.md) · [Network](network.md) · [Apply/drop](apply.md) · [VM memory](vm-memory/index.md) · [Density](density.md) · [Cluster](cluster-scalability.md) · [Review](supervision-cost.md) · [Isolation](isolation-tests.md) · [Replay](replay-fidelity.md)

### Existing tool comparisons

[Docker/devcontainer](compare-containers.md) · [Firecracker/QEMU/gVisor/Kata](compare-runtimes.md) · [Agent sandboxes](compare-agent-sandboxes.md) · [E2B/Daytona/Modal](compare-cloud-sandboxes.md) · [Agent RL infrastructure](compare-rl-infra.md)

### Downloads and reproduction {#run}

[Derived table CSV](index.csv) · [Runtime statistics](runtime-summary.csv) · [Sources and artifacts](runtime-provenance.csv) · [Evidence source summary](evidence-sources.csv) · [Method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
