# Benchmarks and comparisons

## Main conclusions {#conclusions}

**pVisor offers inexpensive staging and review workflows and fast local VM startup; execution of file-heavy VM tasks remains its main performance weakness.** Choose by total task time, isolation boundary and how changes reach the original workspace.

| User question | pVisor's measured position | Selection implication |
|---|---|---|
| [Launch a prepared environment](startup.md#reference-startup) | VM first output about 86 ms; Docker 90 ms, Firecracker 74 ms, QEMU microvm 88 ms | In the same hundred-millisecond range as lightweight VMs and Docker |
| [Boot a full distribution](startup.md#full-ubuntu) | Image-free VM about 110 ms; Firecracker/QEMU with complete Ubuntu about 5–8 s | Less startup waiting for short tasks; not a ranking of VMMs with identical OS configurations |
| [Repair and run tests](agent-tasks.md#reference-env) | Staged 0.70 s, Docker 0.90 s; VM 3.97 s, QEMU microvm 1.85 s | Staged fits interactive tool tasks; VM tool execution is slower |
| [Access files](filesystem.md) | Seven-tool task: staged 1.11 s, VM 4.08 s; Docker 0.97 s, Firecracker 2.37 s, QEMU microvm 1.77 s | Shorter interactive waits with staged; allow more tool time for a VM with its independent guest kernel |
| [Apply reviewed changes](apply.md) | Ten files about 15 ms, 1,000 about 0.84 s, 100,000 about 5.5 min | Suitable for small interactive submissions; large batches slower than matched Git patch |
| [Network](network.md) | Local host-proxy request 1.24 ms, native 0.95 ms; VM bulk transfer about 155 MiB/s, native 869 MiB/s | Modest small-request proxy overhead; significant VM bulk-transfer gap |
| [CLI compatibility](agent-tasks.md) | Controlled Codex tool loop passes; Claude/VM initialization times out | Check the specific client and configuration before choosing the VM |

Numbers belong to each topic's fixed configuration. The filesystem main table compares native, host staged, VM, Docker, Firecracker and both QEMU configurations on development-tool workloads. Samples and percentiles remain separate across configurations; linked reports pin artifacts.

## Motivation {#motivation}

Agent costs extend beyond startup. Tools read repositories, install dependencies and run tests; their outputs then need review and application. Containers, VMs, built-in Agent sandboxes and cloud environments offer different boundaries. This chapter helps decide whether pVisor's speed and workflow suit a task.

## Experiment design {#interpretation}

Measurements separate first useful output, tool execution, complete tasks and cleanup/exit. Inputs and checks are fixed; image and tool preparation are recorded separately. Failed and unmeasured cases do not become zero-latency samples. Linux local references include native execution, Docker/rootless Podman, Firecracker, QEMU and pVisor modes. macOS/HVF startup and memory results are separate. [Methodology](methodology.md) records configurations, sample sizes and evidence identity.

Full distributions and minimal VMs answer deployment waiting and prepared-environment costs, respectively. Real CLIs use controlled model responses to exclude inference and Internet variance; they do not establish real-model success rates. Isolation and file-change semantics are also comparison conditions.

## Data and analysis {#results}

### Choose by workload

For trusted local tasks needing review, staged is a useful starting point: complete repair fits a subsecond budget near native and costs less than a VM. With an established Docker + worktree/Git review workflow, Docker has the file-access advantage; pVisor's value depends on unified staging, conflict protection and execution records.

When an independent guest kernel is needed, pVisor VM starts in the lightweight-VM range without booting a full distribution. npm, Git, search and file creation still accumulate substantial costs. For reused environments, prioritize tool time. Cloud scaling, matching gVisor/Kata performance and real RL-training throughput lack measurements, so no numeric ranking is offered.

### Measurement topics

[VM startup](startup.md) · [Filesystem](filesystem.md) · [Complete Agent tasks](agent-tasks.md) · [Network](network.md) · [Apply/drop](apply.md) · [VM memory and snapshots](vm-memory/index.md) · [Concurrency density](density.md) · [Cluster scaling](cluster-scalability.md) · [Supervision cost](supervision-cost.md) · [Isolation](isolation-tests.md) · [Replay fidelity](replay-fidelity.md)

### Tool comparisons

[Docker/devcontainer](compare-containers.md) · [Isolation runtimes](compare-runtimes.md) · [Built-in Agent sandboxes](compare-agent-sandboxes.md) · [Cloud sandboxes](compare-cloud-sandboxes.md) · [RL infrastructure](compare-rl-infra.md)

### Evidence and technical analysis

Each topic links samples and artifact digests. Optimization experiments, historical A/B comparisons and reproduction details are retained in [filesystem analysis](../design/filesystem-performance-analysis.md), [startup analysis](../design/vm-startup-performance-analysis.md), [memory analysis](../design/vm-memory-performance-analysis.md) and [protocol evidence](../design/benchmark-methodology-evidence.md).
