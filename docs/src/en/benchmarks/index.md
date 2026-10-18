# Which workloads fit pVisor, and at what cost?

## Conclusions {#conclusions}

**For frequently created large workspaces with sparse changes, pVisor stage completes the machine workflow sooner: changing twenty of 10,000 files and retaining ten takes 141 ms in stage, 248 ms with Git worktree and 343 ms with btrfs reflink. Native workflows are faster for small workspaces; budget VM tool waiting from each configuration’s independent measurements.**

| Scenario | Selection implication |
|---|---|
| Large workspace, sparse changes, disposable view | Stage has a measured complete-workflow advantage |
| Small workspace or tool execution alone | Compare native workflows and container costs |
| VM or concurrent capacity | Require complete tool and resource measurements |

When choosing across configurations, choose execution modes using measured workloads with matching budgets and timing boundaries; use different configurations to understand their individual performance levels. Startup, tool execution, application and resource occupancy answer different questions and cannot substitute for one another.

| Decision | Required evidence |
|---|---|
| Choose local tool execution | Native, Docker and pVisor host/staged/VM task comparisons |
| Choose an independent guest kernel | Firecracker, QEMU and pVisor startup plus tool data |
| Set capacity or timeouts | Success rates, complete-task waiting and matching resource accounting |

<!-- measurement-status:start -->

Filesystem measurements cover nine configurations and 19 layered workloads, with three independent environments per cell and three resident rounds, without an interference gate. Of 505 executed environments, 501 pass and 4 resource cells time out; 8 subsequent observations are explicitly skipped. Historical N5/N30 and single-pass tasks remain separate; other topics retain their own cohorts.

<!-- measurement-status:end -->

## Motivation {#motivation}

Selection requires the total cost of producing the same result, beyond startup or a single command. Faster startup does not guarantee faster compilation, and writable mounts and staged review provide different workflows; review/application, execution boundaries and resource use determine which Agent workflows suit pVisor.

## Experiment design {#interpretation}

This reuses separately registered [startup](startup.md), [filesystem](filesystem.md), [repair](agent-tasks.md) and [complete review workflow](supervision-cost.md) experiments. Startup and repair use 60 samples per backend with three warmups; current filesystem measurements cover nine configurations and 19 workloads, with N3 fresh environments per cell and three resident rounds, without separate warmups; historical N5/N30 remains separate; review uses thirty samples and three warmups per size/condition/backend. Same host and CPUs 0,1, with cache states defined by each topic and randomized interleaving. Valid timings pass correctness checks; failures and skipped observations remain explicit, with no speed-based exclusions. Workloads, resources and timing boundaries differ across topics; distributions are not pooled. Docker uses rootless overlay2; workflow controls are native Git/reflink, not container or VM security rankings.

[Network](network.md) uses thirty independent batches and three warmups per condition; each small-request sample is the median of 256 requests. The origin is outside the payload CPU budget and memory is not identically capped. [Capacity](density.md) uses a shared two-core, 2 GiB, zero-swap budget, five rounds per concurrency and separate idle/useful-tool conditions; every failure, unknown outcome and OOM remains counted.

[Lazy images](lazy-image-startup.md) covers file loading through the same host staged executor and separate Docker/lazy VM shell and NumPy startup cohorts. For the matched four-file stage probe, pVisor has shorter cold- and warm-client median waits than OverlayBD; the historical VM comparisons favor lazy for cold clients and Docker for warm clients. Both studies use prepared local services and separate content, metadata and total-response accounting. Their cache states, execution boundaries and budgets remain distinct rather than forming a single ranking.

### Same-host controls and timing

B-STARTUP, B-FS-TOOLS and B-AGENT-TASK use one offline tool environment and fixed inputs across native, pVisor host/staged/VM, private rootless Docker, Firecracker PCI and QEMU q35/microvm. Images, tools and daemon are prepared beforehand. Every execution uses the same two-core affinity; tool VMs have matching configured memory. Each job gets a fresh workspace, with seeded randomized runtime order. Complete Ubuntu, macOS and distinct binaries remain separate cohorts.

Downloads, benchmark compilation, image import and input copying are excluded. Timing separates first valid output, internal tools plus validation, returned results and launch-to-process-exit. Successful samples require validated outputs, observed executor and complete staging. Staged modes also verify that original host files remain unchanged.

### JSON tables and hyperfine {#json-tables}

Tables load resolved named-row JSON before Markdown rendering. Rendering reads data and executes no benchmark. Direct-command startup and replay cohorts use hyperfine statistics; their input preparation and Python hooks are outside timing. Conditions run independently, without the randomized paired-order claim used by historical campaigns. Historical distributions, correctness counts, capacity and physical-memory readings keep their original measurement scope. Reproduction and remaining workload prerequisites are listed in the [JSON benchmark manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark.new/README.md).

### Distributions and failures

Cohorts are not pooled, and slow samples are not removed after measurement. Failures and invalid outputs are counted separately and never treated as zero latency. Capacity guards are not completed jobs. P95 is descriptive: it is omitted below 30 samples. Public P99 is omitted; small cohorts do not support stable tail-latency claims.

Separated clusters are reported with counts and individual medians. The descriptive split rule is in the [runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md); it establishes no cause. Engineering A/B percentage claims need a 95% bootstrap interval for the median difference; user pages do not narrate optimization work.

### Isolation and resources {#reference-resources}

Docker writable bind mounts write directly to the host; pVisor staging retains changes until apply. Firecracker/QEMU use private ext4, while pVisor VM uses virtio-fs with different kernels and devices. This is not a pure VMM or production-security ranking. Standalone staging boundaries follow actual records and [isolation checks](isolation-tests.md).

RSS sums sampled process scopes and may double-count shared pages or miss short peaks. Docker must include actual container processes and identify its dedicated daemon. Whole-group memory must include all descendants, backing and charged cache, without double-counting overlapping cgroups. Configured RAM, RSS, macOS RAM proxies and net physical memory are distinct metrics.

## Data and analysis {#results}

The 2026-10-06 startup/filesystem/repair cohorts have 1,440 valid samples and review has 360; the separate 2026-10-07 stock-kernel startup cohort has 240 valid samples. All have zero measured failures. The full startup retest after the reboot on 2026-10-11 is the current cohort (numbers in [Startup](startup.md)); historical randomized-pair cohorts do not carry over. The selected-source filesystem comparison on 2026-10-12 adds 240 valid samples and zero failures, with task-local caches and a common parent resource budget. Cohorts are not pooled; topic pages provide median differences and paired-bootstrap 95% intervals.

The separate layered filesystem cohort executes 505 environments: 501 pass, 4 resource cells time out, and 8 subsequent observations are explicitly skipped. Complete tasks and resource boundaries are reported separately.

| Question | Measured level (timing is P50) |
|---|---|
| [Startup](startup.md) | 2026-10-11 retest: VM 130.71 ms; Firecracker MMIO 287.13 ms; QEMU microvm 260.10 ms; QEMU 663.25 ms |
| [Repair completion](agent-tasks.md) | staged 0.64 s; Docker 0.81 s; VM 3.25 s; QEMU microvm 1.40 s |
| [Layered filesystem tasks](filesystem.md#complete-task) | N=3, three publication rounds: VM 11.271 s; VM + writeback 5.928 s; Docker 2.053 s; Firecracker 4.305 s; QEMU q35/microvm 4.065/3.851 s |
| [Review workflow](supervision-cost.md) | 10,000 files: stage 141 ms; Git 248 ms; reflink 343 ms |
| [Network](network.md) | Eight-thread small requests: native 0.69 ms; host proxy 10.09 ms; VM 2.10 ms |
| [Active capacity](density.md) | 2 GiB: stage/Podman pass all five rounds at 32; VM at 16 |


Network has 510 valid batches. Capacity retains all 320 batches, including 57 failures. These experiments remain separate and do not form one capacity or speed ranking. Retired Controller/Worker results are [historical evidence](cluster-scalability.md), excluded from current comparisons and daemon sizing; daemon throughput, density and history costs are unmeasured.

Net physical-memory savings and useful-task density from compression, trimmed-kernel benefits, full Ubuntu and macOS comparisons have not completed validation with current artifacts; no advantage is claimed for them. Cloud services, gVisor/Kata and complete RL throughput have no matched ranking. Apply, network, isolation and replay require their own evidence and cannot be inferred from these short tasks.

[Network](network.md) · [Apply](supervision-cost.md#apply-cost) · [Density](density.md) · [VM memory](vm-memory/index.md) · [Isolation](isolation-tests.md) · [Replay](replay-fidelity.md)

[End-to-end tasks](agent-tasks.md) combines tool waiting, workspace lifecycle and CLI compatibility to analyze Agent sandbox, container, VM and cloud tradeoffs. [Reinforcement learning](compare-rl-infra.md) maps tool costs, active capacity and historical-prefix preparation to rollouts; full training throughput and reward are unmeasured.

### Prepared environments {#reference-env}

[Startup](startup.md) · [Files and tools](filesystem.md) · [Repair tasks and CLIs](agent-tasks.md). Tables retain cohort identities and counts, with pinned artifacts rather than claims about current third-party releases.

### Complete distributions {#full-ubuntu}

Ubuntu uses its distribution kernel, initrd, systemd and private disks; pVisor reuses prepared tool directories. The comparison answers deployment waiting, rather than isolating VMM cost.

### QEMU complete distributions {#full-ubuntu-qemu}

q35 and microvm use one Ubuntu template. Their samples and percentiles remain separate from other cohorts.

### Tasks and resources {#product-v1}

[Application](supervision-cost.md#apply-cost) · [Network](network.md) · [Concurrency](density.md) · [Isolation](isolation-tests.md) · [Replay](replay-fidelity.md) · [Review](supervision-cost.md). Unmeasured industry alternatives are marked explicitly, without marketing numbers filling gaps.

### Engineering experiments and data retention {#evidence-format}

Engineering A/B, instrumented profiles and diagnostic probes remain in [technical analysis](../lab-notes/filesystem-performance-analysis.md), outside cross-product main tables.

Markdown retains user-facing tables, with shared CSV downloads only for necessary user statistics. Static tables already in articles, per-round/per-environment records, historical tables and engineering intermediate tables do not receive additional exports by default. Complete CSV/JSON, raw reports, individual samples, logs, artifact manifests and frozen harnesses remain in their local cohorts or the `benchmark.new/.data/docs-assets-prune-20261018/` documentation data archive, outside the published site. Dynamic JSON is limited to display tables needed by the build; public engineering figures use SVG. See the [runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md) for reproduction and retention rules.

### Measurement coverage of the new framework {#measurement-coverage}

This table separates new measurements from historical evidence. Data is updated only when metrics, inputs, configurations and correctness gates match. Ready, normal exit, internal tool timings, memory and correctness do not substitute for one another. Unprepared experiments produce no new numbers; historical conclusions apply to their original cohorts.

{{ table("measurement-coverage.render.json", {"markdown":true}) }}

Filesystem coverage refers to the nine-mode, 19-workload N3 protocol, with four incomplete resource cells; historical N5/N30 does not fill its gaps.

### Sources and reproduction {#run}

[Nine-mode layered statistics](../../../overrides/assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-levels.csv) · [Ready](../../../overrides/assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-ready.csv) · [Provenance summary](../../../overrides/assets/benchmarks/filesystem-layered-references/provenance.json)

Complete CSV/JSON and raw records for other topics remain in their local cohorts or the documentation data archive.

[Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
