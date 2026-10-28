# Which workloads fit pVisor?

## Motivation {#motivation}

Evaluate stage for sparse changes in large workspaces; small workspaces or tool-only execution may suit native workflows. First choose executors that meet your isolation needs, then compare the total cost of producing the same result. If you need an independent guest kernel, budget for VM startup and tool execution separately. Capacity depends on task success and consistent resource accounting, not idle environment counts alone.

Changing twenty of 10,000 files and retaining ten takes 141 ms in stage, 248 ms with Git worktree and 343 ms with btrfs reflink; native workflows are faster on small workspaces. Startup, tool execution, application and resource occupancy answer different questions and cannot substitute for one another.

<!-- measurement-status:start -->

Filesystem covers nine configurations and 19 layered workloads, with three independent environments per cell and three resident rounds; see its [coverage note](filesystem.md). [Lazy images](lazy-image-startup.md) use independent current-V2/native-Docker inventory, cold/warm startup, mmap and memory reruns. Historical cohorts do not fill gaps in new experiments.

<!-- measurement-status:end -->

## Experiment design {#interpretation}

Measure the startup, tool execution, apply and resource occupancy that produce one accepted result as the cost of that result. That cost decides which execution mode fits a workload; lower cost for the same verified result means the workload runs more cheaply.

The numbers come from separately registered experiments. Startup and repair use 60 samples per backend with three warmups; filesystem covers nine configurations and 19 workloads with three fresh environments per cell and three rounds each; review uses 30 samples and three warmups; network uses 30 independent batches with 256-request medians; capacity uses five rounds per concurrency under a shared two-core, 2 GiB, zero-swap budget. Image inventories use three fresh stores, clients/local reads 30 pairs, and whole-group memory five pairs. CPU, budgets and cache states belong to each topic; different topics do not form a matched-configuration ranking.

Valid timings pass correctness checks; failures and skipped observations stay explicit, with no speed-based exclusions. Cohorts are not pooled. P95 is descriptive and omitted below 30 samples, public P99 is omitted, and separated clusters are reported with counts and per-cluster medians.

### Isolation and resources {#reference-resources}

Docker storage drivers are recorded by topic: historical tool environments differ from the current image experiment's rootless containerd overlayfs. Firecracker and QEMU use private ext4; pVisor VM uses virtio-fs and different kernels. These are complete-configuration costs, not a pure VMM or security ranking. Engineering A/B work stays in lab notes, with complete data retained locally.

## Data and analysis {#results}

Lazy images and layered filesystem tasks use independent retests from 2026-10-25, based on main `92a4ce4e`. Other topics retain the cohorts and scopes stated on their pages. Experiments are summarized separately without pooling samples; filesystem N=3 is exploratory and does not establish reliable tails.

| Question | Measured level (sampling scope on each topic page) |
|---|---|
| [Startup](startup.md) | 2026-10-11 retest: VM 130.71 ms; Firecracker MMIO 287.13 ms; QEMU microvm 260.10 ms; QEMU 663.25 ms |
| [Repair completion](agent-tasks.md) | staged 0.64 s; Docker 0.81 s; VM 3.25 s; QEMU microvm 1.40 s |
| [Layered filesystem tasks](filesystem.md#complete-task) | 2026-10-25, N=3, three publication rounds: pVisor VM 11.675 s; pVisor VM + writeback 6.171 s; Docker 1.637 s; Firecracker (stock kernel) 5.074 s; QEMU q35 3.170 s; QEMU microvm 3.433 s |
| [Review workflow](supervision-cost.md) | 10,000 files: stage 141 ms; Git 248 ms; reflink 343 ms |
| [Network](network.md) | Eight-thread small requests: native 0.69 ms; host proxy 10.09 ms; VM 2.10 ms |
| [Active capacity](density.md) | 2 GiB: stage/Podman pass all five rounds at 32; VM at 16 |
| [Image delivery](lazy-image-startup.md) | Four-image shared inventory saves 74%; cold shell responses 2.86 MiB versus native Docker 28.39 MiB; warm clients and complete library scans are slower |

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

Markdown retains user-facing tables, with shared CSV downloads only for necessary user statistics. Static tables already in articles, per-round/per-environment records, historical tables and engineering intermediate tables do not receive additional exports by default. Complete CSV/JSON, raw reports, individual samples, logs, artifact manifests and frozen harnesses remain in their local cohorts or the `benchmark/.data/docs-assets-prune-20261018/` documentation data archive, outside the published site. Dynamic JSON is limited to display tables needed by the build; public engineering figures use SVG. See the [runner manual](https://github.com/DeepLink-org/pvisor/blob/555e750105e8072e0b85030dfb36226c66dae206/benchmark/pvisor/README.md) for reproduction and retention rules.

### Measurement coverage of the new framework {#measurement-coverage}

This table separates new measurements from historical evidence. Data is updated only when metrics, inputs, configurations and correctness gates match. Ready, normal exit, internal tool timings, memory and correctness do not substitute for one another. Unprepared experiments produce no new numbers; historical conclusions apply to their original cohorts.

{{ table("measurement-coverage.render.json", {"markdown":true}) }}

Filesystem coverage refers to the nine-mode, 19-workload N3 protocol, with four incomplete resource cells; historical N5/N30 data does not fill those gaps.

### Sources and reproduction {#run}

[Nine-mode layered statistics](../assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-levels.csv) · [Ready](../assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-ready.csv) · [Provenance summary](../assets/benchmarks/filesystem-layered-references/provenance.json)

Complete CSV/JSON and raw records for other topics remain in their local cohorts or the documentation data archive.

[Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/555e750105e8072e0b85030dfb36226c66dae206/benchmark/pvisor/README.md)

> Historical cohorts retain their pinned sources, artifacts and timing scope. See the [benchmark registry](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/registry.csv) for active experiments and preparation status.
