# How long do repair tasks take, and do Agent CLIs finish?

## Main conclusions {#conclusions}

**Rootless host/staged provide shorter repair/test waiting than the measured pVisor VM. Fast VM boot does not remove tool cost. Pinned CLI tests pass controlled Codex loops, while Claude initialization fails in the measured pVisor VM configuration.**

| Need | Selection implication |
|---|---|
| Local tools and retained edits | Evaluate rootless host/staged |
| Independent guest kernel | Budget complete VM tool time |
| Existing container/Git workflow | Compare costs and required review semantics |

## Motivation {#motivation}

An Agent edits files and runs tests after startup. Fixed repair plans isolate environment cost; real client loops separately check compatibility before real-model variance.

## Experiment design {#interpretation}

Shared Linux/x86_64 host, AMD Ryzen 7 9700X, Fedora kernel 7.2.8-200.fc44.x86_64. Launch trees and the private Docker daemon are pinned to host CPUs 0,1; guests have 2 vCPU. Host/staged use rootless_process. Shell VMs use 128 MiB; tool VMs use 16 GiB. Native/Docker memory is not capped: this controls CPU and configured guest RAM, not identical resource enforcement. Tools and inputs are prepared; each run gets a fresh workspace, warm caches, three warmups and 60 measured trials, with seeded randomized backend order. Builds, downloads and input copying are excluded.

Docker Engine 29.7.2 uses a private rootless VFS daemon and writable bind mounts. This does not represent overlay2 or Docker Desktop. Firecracker 1.13.1 PCI runs without jailer; QEMU 10.2.2 uses q35/microvm with private ext4. pVisor VM uses virtio-fs and a different kernel. Kernel, storage, devices and staging semantics remain configuration differences; these results do not isolate the VMM or FUSE alone.

The plan inspects/searches a repository, fixes Python, runs Python/Rust/Node tests, installs 32 offline npm packages and generates a diff. Result ends at checked returned results; Completion includes process exit. Tests and expected edits must pass. Real-model success, large repositories and persistent-pool throughput are unmeasured.

## Data and analysis {#results}

### Fixed repair/test plan {#reference-env}

Measured 2026-10-06, 60/60 valid trials per backend, zero measured failures. Outputs and exits are checked; staged trials additionally check unchanged originals and complete retained edits. All valid slow samples are retained; no timing-based exclusions. P95 is descriptive. Separated clusters show each median and count out of 60 instead of one P50, using the predefined rule in [methodology](methodology.md).

| Backend | Valid / failed | Result P50 s | Completion P50 s | Completion P95 s |
|---|---|---|---|---|
| Native | 60 / 0 | 0.45 | 0.45 | 0.47 |
| pVisor host | 60 / 0 | 0.46 | 0.47 | 0.56 |
| pVisor staged | 60 / 0 | 0.57 | 0.68 | 0.73 |
| pVisor VM | 60 / 0 | 3.11 | 3.25 | 4.14 |
| Docker rootless / VFS | 60 / 0 | 4.03 | 5.15 | 6.38 |
| Firecracker PCI | 60 / 0 | 2.01 | 2.06 | 3.00 |
| QEMU q35 | 60 / 0 | 1.29 | 1.33 | 1.65 |
| QEMU microvm | 60 / 0 | 1.23 | 1.27 | 1.77 |

Full completion includes tool execution and exit; Docker VFS creation is included. Individual tools are in [filesystem comparisons](filesystem.md).

### Real CLI compatibility {#cli-compatibility}

Independent 2026-10-04 cohorts: Claude Code 2.1.128 / Codex CLI 0.160.0, N=30 successful trials per available cell, three warmups, two cores / 16 GiB. Units: launch-to-result P50 seconds. Local deterministic responses and fake credentials exclude inference. Codex uses `danger-full-access`, not default nested-sandbox behavior.

| Backend | Claude loop P50 s | Codex loop P50 s |
|---|---|---|
| Native | 0.82 | 1.97 |
| pVisor host | 0.85 | 1.94 |
| pVisor staged | 1.07 | 2.25 |
| pVisor VM | FAILED / N=0 | 10.93 |
| Docker rootless | 1.23 | 6.26 |
| Firecracker PCI | 3.03 | 7.83 |
| QEMU q35 | 2.67 | 7.69 |
| QEMU microvm | 2.71 | 7.67 |

Codex passes all eight groups (30/30 each). Claude passes available groups (30/30), but pVisor VM exceeds the 90 s initialization deadline in preflight and has no formal latency samples. These are version-pinned observations, not claims about every newer client.

### Complete Ubuntu deployment {#full-ubuntu}

Independent 2026-10-04, two cores / 16 GiB, N=10 and three warmups. Different OS initialization/tools/storage; P50 launch-to-result seconds, without a pure VMM ranking.

| Deployment | Repair result P50 s |
|---|---|
| pVisor VM / host tools | 4.61 |
| Firecracker / Ubuntu | 8.51 |
| QEMU q35 / Ubuntu | 8.12 |
| QEMU microvm / Ubuntu | 10.33 |

### Downloads and reproduction {#run}

[Derived table CSV](agent-tasks.csv) · [Runtime statistics](runtime-summary.csv) · [Sources and artifacts](runtime-provenance.csv) · [Evidence source summary](evidence-sources.csv) · [Method](methodology.md) · [Runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
