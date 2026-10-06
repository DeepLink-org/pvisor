# How long does a complete repair task take?

## Conclusions {#conclusions}

**A fixed repair task through exit takes 0.64 s in pVisor staged, ahead of Docker at 0.81 s. pVisor VM takes 3.25 s, behind Firecracker at 2.20 s and QEMU microvm at 1.40 s. Advantages depend on execution mode and workload.**

| Need | Selection implication |
| --- | --- |
| Local execution with retained changes | Evaluate host/staged and the complete review workflow |
| Independent guest kernel | Budget both startup and VM tool waiting |
| Concurrent or idle environments | Require fixed-budget throughput and physical-memory measurements |

## Motivation {#motivation}

Repairing code also requires search, dependencies, tests and a diff. A fixed tool plan removes inference and public-network variability so you can assess waiting introduced by the environment.

## Experiment design {#interpretation}

Linux x86_64, AMD Ryzen 7 9700X, Fedora 7.2.8-200.fc44.x86_64. Launched process trees and the private Docker daemon are pinned to CPUs 0,1. VMs use 2 vCPU, 128 MiB for the shell probe and 16 GiB for tools. Native/Docker memory is uncapped: this is a CPU-controlled task comparison, not a capacity comparison under identical memory limits. Host/staged use rootless_process.

All backends share offline tools and fixed inputs, with a new workspace per trial. Warm caches, three warmups and 60 measured samples per fixed-plan cell; 30 for environment checks and real CLIs; backends alternate in seeded randomized order. Preparation, builds, image import and fixture resets are outside timing; complete tasks include launch and exit. Docker Engine 29.7.2 uses a private rootless **overlay2** daemon, the classic image store and writable bind mounts. Firecracker 1.13.1 PCI runs without jailer; QEMU 10.2.2 uses q35/microvm and private ext4. pVisor VM uses virtio-fs and its own firmware. Kernels, storage and staging semantics differ: these are task costs for the stated configurations, not pure VMM or security rankings.

The fixed plan inspects/searches the repository, fixes Python, runs Python/Rust/Node tests, installs 32 offline npm packages and generates a diff. Result ends at checked returned results; Completion includes exit. Tests and expected edits must pass. The tool plan does not measure model inference or establish compatibility of real Agent CLIs.

## Data and analysis {#results}

Measured on 2026-10-06: each fixed-plan backend has 60/60 valid samples and zero measured failures; environment/CLI counts and preflight failures are listed separately. Outputs, exit and execution records must pass validation; staging also requires unchanged host originals and complete retained changes. Every valid slow sample is kept, with no timing-based exclusions. Tables normally show P50; separated distributions show cluster medians and counts. P95 is descriptive only. Raw reports, binaries and input/source manifests stay in ignored `.data/`; public CSVs retain workload, cohort and provenance associations.

### Fixed repair and tests {#reference-env}

| Runtime | Valid / failed | Result P50 s | Completion P50 s | Completion P95 s |
| --- | --- | --- | --- | --- |
| Native | 60 / 0 | 0.43 | 0.44 | 0.46 |
| pVisor host | 60 / 0 | 0.44 | 0.46 | 0.48 |
| pVisor staged | 60 / 0 | 0.56 | 0.64 | 0.67 |
| pVisor VM | 60 / 0 | 3.12 | 3.25 | 3.29 |
| Docker rootless / overlay2 | 60 / 0 | 0.74 | 0.81 | 0.83 |
| Firecracker PCI | 60 / 0 | 2.15 | 2.20 | 2.24 |
| QEMU q35 | 60 / 0 | 1.41 | 1.46 | 1.48 |
| QEMU microvm | 60 / 0 | 1.35 | 1.40 | 1.42 |

Staged minus Docker completion median is −175.84 ms, with a paired-bootstrap 95% interval of [−178.85, −174.55] ms. VM minus QEMU microvm is +1849.00 ms, interval [+1834.43, +1855.78] ms. This advantage applies to this prepared repair workload; it does not establish a general tool or concurrent-throughput advantage.

### Real CLI compatibility {#cli-compatibility}

The environment check runs Python, Node, Git, Cargo/Rustc and CLI version checks. Claude Code 2.1.128 and Codex 0.160.0 start the actual clients, execute repair/tests through deterministic local model responses, and verify that tool results return to the client. Each available condition has 30/30 valid samples, zero measured failures and three warmups. Seconds; P50 from launch through exit. These samples do not establish tail latency.

| Runtime | Environment P50 s | Claude Code P50 s | Codex P50 s |
| --- | --- | --- | --- |
| Native | 0.15 | 0.77 | 1.86 |
| pVisor host | 0.17 | 0.78 | 1.87 |
| pVisor staged | 0.19 | 0.99 | 2.14 |
| pVisor VM | 1.10 | — | 9.01 |
| Docker rootless / overlay2 | 0.44 | 1.11 | 6.00 |
| Firecracker PCI | 1.50 | 2.98 | 7.72 |
| QEMU q35 | 0.88 | 2.07 | 6.93 |
| QEMU microvm | 0.80 | 2.05 | 6.86 |


Claude Code in pVisor VM exceeded the 90 s initialization deadline during preflight: zero valid samples, excluded from latency comparisons. Other Claude conditions and every Codex condition completed the controlled tool loop.

Staged minus Docker completion median: Claude Code −127.83 ms, paired-bootstrap 95% interval [−137.91, −116.72] ms; Codex −3864.45 ms, interval [−3948.16, −3804.45] ms. These differences include client initialization, tool calls and exit waiting; they do not identify a single filesystem cost.

Claude uses `--bare` with only Bash allowed; Codex uses `--ephemeral` and `danger-full-access`, with isolation provided by the outer executor. No real inference or public-network requests are measured. Results apply to these controlled configurations, not default client sandboxes, model quality or total real-service latency. Full Ubuntu deployments still lack current-artifact measurements.

<a id="full-ubuntu"></a>

### Downloads and reproduction {#run}

[Derived statistics CSV](agent-tasks.csv) · [All runtime statistics](runtime-summary.csv) · [Differences and 95% confidence intervals](runtime-comparisons.csv) · [Source and artifact provenance](runtime-provenance.csv) · [Method](methodology.md) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
