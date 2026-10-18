# Which workloads fit pVisor?

## Motivation {#motivation}

Choose a mode by the total cost of producing the same result. Use stage for large workspaces with sparse changes, native workflows for small workspaces or tool-only execution, and a VM only from complete-task measurements. Match evidence to the decision: task comparisons for local execution, startup plus tool data for an independent guest kernel, and success rates with resource accounting for capacity.

Changing twenty of 10,000 files and retaining ten takes 141 ms in stage, 248 ms with Git worktree and 343 ms with btrfs reflink; native workflows are faster on small workspaces. Startup, tool execution, application and resource occupancy answer different questions and cannot substitute for one another.

<!-- measurement-status:start -->

Filesystem covers nine configurations and 19 layered workloads, with three independent environments per cell and three resident rounds; its [coverage note](filesystem.md) records executed, passed, failed and skipped environments. Historical N5/N30 and single-pass tasks remain separate, and other topics keep their own cohorts.

<!-- measurement-status:end -->

## Experiment design {#interpretation}

Measure the startup, tool execution, apply and resource occupancy that produce one accepted result as the cost of that result. That cost decides which execution mode fits a workload; lower cost for the same verified result means the workload runs more cheaply.

The numbers come from separately registered experiments. Startup and repair use 60 samples per backend with three warmups; filesystem covers nine configurations and 19 workloads with three fresh environments per cell and three rounds each; review uses 30 samples and three warmups; network uses 30 independent batches with 256-request medians; capacity uses five rounds per concurrency under a shared two-core, 2 GiB, zero-swap budget. All runs share the host and CPUs 0–1, with cache states defined by each topic and randomized interleaving.

Valid timings pass correctness checks; failures and skipped observations stay explicit, with no speed-based exclusions. Cohorts are not pooled. P95 is descriptive and omitted below 30 samples, public P99 is omitted, and separated clusters are reported with counts and per-cluster medians.

### Isolation and resources {#reference-resources}

Docker uses rootless overlay2, Firecracker and QEMU use private ext4, and pVisor VM uses virtio-fs with different kernels: these are complete-configuration costs, not a pure VMM or security ranking. Engineering A/B work stays in lab notes, and complete data remains in local cohorts.

## Data and analysis {#results}

The 2026-10-06 startup/filesystem/repair cohorts have 1,440 valid samples and review has 360; the separate 2026-10-07 stock-kernel startup cohort has 240 valid samples. All have zero measured failures. The full startup retest after the reboot on 2026-10-11 is the current cohort (numbers in [Startup](startup.md)); historical randomized-pair cohorts do not carry over. The selected-source filesystem comparison on 2026-10-12 adds 240 valid samples and zero failures, with task-local caches and a common parent resource budget. Topic pages provide median differences and paired-bootstrap 95% intervals.

| Question | Measured level (timing is P50) |
|---|---|
| [Startup](startup.md) | 2026-10-11 retest: VM 130.71 ms; Firecracker MMIO 287.13 ms; QEMU microvm 260.10 ms; QEMU 663.25 ms |
| [Repair completion](agent-tasks.md) | staged 0.64 s; Docker 0.81 s; VM 3.25 s; QEMU microvm 1.40 s |
| [Layered filesystem tasks](filesystem.md#complete-task) | N=3, three publication rounds: VM 11.271 s; VM + writeback 5.928 s; Docker 2.053 s; Firecracker 4.305 s; QEMU q35/microvm 4.065/3.851 s |
| [Review workflow](supervision-cost.md) | 10,000 files: stage 141 ms; Git 248 ms; reflink 343 ms |
| [Network](network.md) | Eight-thread small requests: native 0.69 ms; host proxy 10.09 ms; VM 2.10 ms |
| [Active capacity](density.md) | 2 GiB: stage/Podman pass all five rounds at 32; VM at 16 |

Network has 510 valid batches. Capacity retains all 320 batches, including 57 failures. These experiments remain separate and do not form one capacity or speed ranking. Daemon throughput, density and history costs are unmeasured.

Net physical-memory savings and useful-task density from compression, trimmed-kernel benefits, full Ubuntu and macOS comparisons are not yet validated on current artifacts; no advantage is claimed for them. Cloud services, gVisor/Kata and complete RL throughput have no matched ranking. Apply, network, isolation and replay require their own evidence and cannot be inferred from these short tasks.

[End-to-end tasks](agent-tasks.md) combines tool waiting, workspace lifecycle and CLI compatibility to analyze Agent sandbox, container, VM and cloud tradeoffs. [Reinforcement learning](compare-rl-infra.md) maps tool costs, active capacity and historical-prefix preparation to rollouts; full training throughput and reward are unmeasured.

### Prepared environments {#reference-env}

Startup, filesystem and task cohorts share one prepared tool environment, and every table pins its artifacts by digest. A table describes that pinned release, not a current third-party release.

### Complete distributions {#full-ubuntu}

Ubuntu uses its distribution kernel, initrd, systemd and private disks; pVisor reuses prepared tool directories. The comparison answers deployment waiting, rather than isolating VMM cost.

### QEMU complete distributions {#full-ubuntu-qemu}

q35 and microvm use one Ubuntu template. Their samples and percentiles remain separate from other cohorts.

### Tasks and resources {#product-v1}

Apply, network, isolation and replay use their own task sets and cohorts; none of them inherits the numbers in the table above.

### Engineering experiments and data retention {#evidence-format}

Engineering A/B, instrumented profiles and diagnostic probes remain in [technical analysis](../lab-notes/filesystem-performance-analysis.md), outside cross-product main tables.

Markdown retains user-facing tables, with shared CSV downloads only for necessary user statistics. Static tables already in articles, per-round/per-environment records, historical tables and engineering intermediate tables do not receive additional exports by default. Complete CSV/JSON, raw reports, individual samples, logs, artifact manifests and frozen harnesses remain in their local cohorts or the `benchmark.new/.data/docs-assets-prune-20261018/` documentation data archive, outside the published site. Dynamic JSON is limited to display tables needed by the build; public engineering figures use SVG. See the [runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md) for reproduction and retention rules.

### Measurement coverage of the new framework {#measurement-coverage}

This table separates new measurements from historical evidence. Data is updated only when metrics, inputs, configurations and correctness gates match. Ready, normal exit, internal tool timings, memory and correctness do not substitute for one another. Unprepared experiments produce no new numbers; historical conclusions apply to their original cohorts.

{{ table("measurement-coverage.render.json", {"markdown":true}) }}

Filesystem coverage refers to the nine-mode, 19-workload N3 protocol, with four incomplete resource cells; historical N5/N30 data does not fill those gaps.

### Sources and reproduction {#run}

[Nine-mode layered statistics](../../../overrides/assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-levels.csv) · [Ready](../../../overrides/assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-ready.csv) · [Provenance summary](../../../overrides/assets/benchmarks/filesystem-layered-references/provenance.json)

Complete CSV/JSON and raw records for other topics remain in their local cohorts or the documentation data archive.

[Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
