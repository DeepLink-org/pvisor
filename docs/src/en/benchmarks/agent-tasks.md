# End-to-end Agent tasks: when should you choose pVisor?

## Conclusions {#conclusions}

**Evaluate pVisor staged when you repeatedly create task views, retain sparse edits and review/apply them: fixed repair takes 0.64 s versus Docker's 0.81 s, but seven-tool completion takes 1.09 s versus Docker's 0.82 s. pVisor VM repair takes 3.25 s, behind Firecracker's 2.20 s and QEMU microvm's 1.40 s; budget the cost of an independent guest boundary.**

| Need | Selection implication |
| --- | --- |
| Large workspace, sparse edits, fresh task view each time | Staged has a complete machine-workflow advantage; also budget tool overhead |
| Small workspace, existing container or native Agent sandbox | Keep existing Git/tool flows; do not replace everything for individual operation speed |
| Independent guest kernel | Validate CLI compatibility and complete tool cost before choosing VM |
| Remote API and managed elasticity | Evaluate clouds by repository synchronization, deployment and operations needs; matched performance and billing are unmeasured |

## Motivation {#motivation}

A task includes environment preparation, model/tool interaction, verification, review/application and cleanup. Startup alone misses tool and result-handling costs; tool speed alone misses repeated workspace creation. A fixed tool plan removes inference and public-network variability, while a separate complete review workflow measures how the same edits reach the original workspace.

## Experiment design {#interpretation}

Linux x86_64, AMD Ryzen 7 9700X, Fedora 7.2.8-200.fc44.x86_64. Launched process trees and the private Docker daemon are pinned to CPUs 0,1. VMs use 2 vCPU, 128 MiB for the shell probe and 16 GiB for tools. Native/Docker memory is uncapped: this is a CPU-controlled task comparison, not a capacity comparison under identical memory limits. Host/staged use rootless_process.

All backends share offline tools and fixed inputs, with a new workspace per trial. Warm caches, three warmups and 60 measured samples per fixed-plan cell; 30 for environment checks and real CLIs; backends alternate in seeded randomized order. Preparation, builds, image import and fixture resets are outside timing; complete tasks include launch and exit. Docker Engine 29.7.2 uses a private rootless **overlay2** daemon, the classic image store and writable bind mounts. Firecracker 1.13.1 PCI runs without jailer; QEMU 10.2.2 uses q35/microvm and private ext4. pVisor VM uses virtio-fs and its own firmware. Kernels, storage and staging semantics differ: these are task costs for the stated configurations, not pure VMM or security rankings.

The fixed plan inspects/searches the repository, fixes Python, runs Python/Rust/Node tests, installs 32 offline npm packages and generates a diff. Result ends at checked returned results; Completion includes exit. Tests and expected edits must pass. The tool plan does not measure model inference or establish compatibility of real Agent CLIs.

These samples do not verify equal Node/npm compile-cache state across backends. A fresh workspace does not establish an empty tool cache. Complete-task differences include each configuration's cache behavior; they cannot all be attributed to staging or the VMM.

Scenario analysis also reuses separately registered [filesystem](filesystem.md), [complete review workflow](supervision-cost.md), [startup](startup.md), [lazy-image](lazy-image-startup.md) and [isolation](isolation-tests.md) experiments. Review has thirty samples per cell and includes view creation, twenty edits, review, application of ten files and cleanup; Git/reflink do not supply the same isolation. Experiments remain separate: do not sum medians to estimate a real Agent task or reuse their differing budgets for one ranking.

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

### Tool speed versus complete change workflows {#workflow-tradeoffs}

The first three tool rows come from the 2026-10-06 task/filesystem experiments; review rows come from separate experiments on that date. Units, P50 and samples per cell are explicit; compare configurations only within the same experiment in each row.

| Workload | pVisor P50 | Control P50 | N per cell | Selection implication |
|---|---:|---:|---:|---|
| Fixed repair through exit | 0.64 s | Docker 0.81 s | 60 | Shorter staged waiting for this repair plan |
| Seven tools through exit | 1.09 s | Docker 0.82 s | 60 | Filesystem-heavy tool costs can outweigh short startup |
| Seven tools through exit | VM 4.27 s | QEMU microvm 1.50 s | 60 | Do not choose a guest boundary from shell-ready alone |
| Edit 20 of 10,000 files, apply 10, including view creation/cleanup | 140.82 ms | Git worktree 248.10 ms; reflink 343.00 ms | 30 | Whole-tree costs exceed stage overhead in a large workspace |
| Edit 20 of 100 files, apply 10, including view creation/cleanup | 109.02 ms | Git worktree 22.02 ms; reflink 23.66 ms | 30 | Native workflows are faster for small workspaces |
| Prepared 20-file view, review/apply/discard only | 27.71 ms | Git worktree 4.69 ms | 30 | Stage has no standalone review-speed advantage after view preparation |

[Seven-tool completion](filesystem.md#complete-task) has a staged-minus-Docker median difference of +268.81 ms, 95% CI [+264.64, +271.15] ms. The [large-workspace workflow](supervision-cost.md#baseline-meaning) has stage-minus-Git −107.27 ms, 95% CI [−109.02, −105.60] ms; the small workspace has +86.99 ms, 95% CI [+77.28, +87.57] ms. Advantages depend on workload and workflow choice and do not justify replacing Docker, worktrees or VMMs universally. The complete workflow applies only ten files; see [apply costs](supervision-cost.md#apply-cost) for bulk changes. Machine timings exclude human reading.

[Startup](startup.md) and [lazy images](lazy-image-startup.md) separately address prepared environments and uncached client images. Cold-client data excludes initial upstream download, unpack and indexing; warm-client Docker is faster. On-demand image reads do not remove subsequent repository-tool costs.

### Integrating existing toolchains {#existing-workflows}

- **Native Agent sandboxes:** keep client permissions and approvals; evaluate pVisor when unified staging, per-path application and records across Agents are needed. Built-in sandboxes combined with Git/worktrees can also review edits. The CLI measurements below use controlled modes; default nested-sandbox compatibility is unmeasured. Capability sources: [Claude Code](https://code.claude.com/docs/en/sandboxing), [Codex](https://developers.openai.com/codex/security/), [Gemini CLI](https://geminicli.com/docs/cli/sandbox/).
- **Docker / devcontainer:** retain dependency environments and existing Git flows; writable bind mounts modify host files directly, requiring independent workspaces and recovery. Tested staged/safe/VM configurations each retain staged writes and block listed outside accesses in 3/3 path/Unix-socket fixtures; OCI/Podman's authorized writable workspaces write through. See the [isolation matrix](isolation-tests.md). This does not establish kernel-escape protection or remote-effect rollback.
- **Firecracker / QEMU / gVisor / Kata:** first choose the guest/container boundary, then budget complete tasks. Kernels, filesystems and hardening differ in the repair controls above. Stock-kernel shell-ready and tool cohorts are separate; matched gVisor/Kata tasks are unmeasured, without a unified security or speed ranking.
- **Cloud sandboxes:** evaluate [E2B](https://docs.e2b.dev/), [Daytona](https://www.daytona.io/docs/en/) and [Modal](https://modal.com/docs/guide/sandboxes) for remote environments and managed capacity. A real task must account for environment construction, repository upload, dependency caches, execution, result download and local merge. Local timings cannot replace region latency, billing or availability tests. Model fees, local hardware and operations also belong in costs; long-lived environments change preparation amortization.

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

[Scenario evidence CSV](task-scenarios.csv) · [Derived statistics CSV](agent-tasks.csv) · [All runtime statistics](runtime-summary.csv) · [Differences and 95% confidence intervals](runtime-comparisons.csv) · [Source and artifact provenance](runtime-provenance.csv) · [Method](methodology.md) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
