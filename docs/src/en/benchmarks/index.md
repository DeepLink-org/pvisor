# Which workloads fit pVisor, and at what cost?

## Conclusions {#conclusions}

**For frequently created large workspaces with sparse changes, pVisor stage completes the machine workflow sooner: changing twenty of 10,000 files and retaining ten takes 141 ms in stage, 248 ms with Git worktree and 343 ms with btrfs reflink. Native workflows are faster for small workspaces; budget VM tool waiting from each configuration’s independent measurements.**

| Scenario | Selection implication |
|---|---|
| Large workspace, sparse changes, disposable view | Stage has a measured complete-workflow advantage |
| Small workspace or tool execution alone | Compare native workflows and container costs |
| VM or concurrent capacity | Require complete tool and resource measurements |

<!-- measurement-status:start -->

Filesystem measurements cover nine configurations and 19 layered workloads, with three independent environments per cell and three resident rounds, without an interference gate. Of 505 executed environments, 501 pass and 4 resource cells time out; 8 subsequent observations are explicitly skipped. Historical N5/N30 and single-pass tasks remain separate; other topics retain their own cohorts.

<!-- measurement-status:end -->

## Motivation {#motivation}

Selection requires the total cost of producing the same result, beyond startup or a single command. Review/application, execution boundaries and resource use determine which Agent workflows suit pVisor.

## Experiment design {#interpretation}

This reuses separately registered [startup](startup.md), [filesystem](filesystem.md), [repair](agent-tasks.md) and [complete review workflow](supervision-cost.md) experiments. Startup and repair use 60 samples per backend with three warmups; current filesystem measurements cover nine configurations and 19 workloads, with N3 fresh environments per cell and three resident rounds, without separate warmups; historical N5/N30 remains separate; review uses thirty samples and three warmups per size/condition/backend. Same host and CPUs 0,1, with cache states defined by each topic and randomized interleaving. Valid timings pass correctness checks; failures and skipped observations remain explicit, with no speed-based exclusions. Workloads, resources and timing boundaries differ across topics; distributions are not pooled. Docker uses rootless overlay2; workflow controls are native Git/reflink, not container or VM security rankings.

[Network](network.md) uses thirty independent batches and three warmups per condition; each small-request sample is the median of 256 requests. The origin is outside the payload CPU budget and memory is not identically capped. [Capacity](density.md) uses a shared two-core, 2 GiB, zero-swap budget, five rounds per concurrency and separate idle/useful-tool conditions; every failure, unknown outcome and OOM remains counted.

[Lazy images](lazy-image-startup.md) covers file loading through the same host staged executor and separate Docker/lazy VM shell and NumPy startup cohorts. For the matched four-file stage probe, pVisor has shorter cold- and warm-client median waits than OverlayBD; the historical VM comparisons favor lazy for cold clients and Docker for warm clients. Both studies use prepared local services and separate content, metadata and total-response accounting. Their cache states, execution boundaries and budgets remain distinct rather than forming a single ranking.

## Data and analysis {#results}

The 2026-10-06 startup/filesystem/repair cohorts have 1,440 valid samples and review has 360; the separate 2026-10-07 stock-kernel startup cohort has 240 valid samples. All have zero measured failures. The selected-source filesystem comparison on 2026-10-12 adds 240 valid samples and zero failures, with task-local caches and a common parent resource budget. Cohorts are not pooled; topic pages provide median differences and paired-bootstrap 95% intervals.

The separate layered filesystem cohort executes 505 environments: 501 pass, 4 resource cells time out, and 8 subsequent observations are explicitly skipped. Complete tasks and resource boundaries are reported separately.

| Question | Measured level (timing is P50) |
|---|---|
| [Startup](startup.md) | Stock controls: VM 100.91 ms; Firecracker 285.04 ms; QEMU microvm 321.56 ms; q35 702.98 ms |
| [Repair completion](agent-tasks.md) | staged 0.64 s; Docker 0.81 s; VM 3.25 s; QEMU microvm 1.40 s |
| [Layered filesystem tasks](filesystem.md#complete-task) | N=3, three publication rounds: VM 11.271 s; VM + writeback 5.928 s; Docker 2.053 s; Firecracker 4.305 s; QEMU q35/microvm 4.065/3.851 s |
| [Review workflow](supervision-cost.md) | 10,000 files: stage 141 ms; Git 248 ms; reflink 343 ms |
| [Network](network.md) | Eight-thread small requests: native 0.69 ms; host proxy 10.09 ms; VM 2.10 ms |
| [Active capacity](density.md) | 2 GiB: stage/Podman pass all five rounds at 32; VM at 16 |


Network has 510 valid batches. Capacity retains all 320 batches, including 57 failures. These experiments remain separate and do not form one capacity or speed ranking. Retired Controller/Worker results are [historical evidence](cluster-scalability.md), excluded from current comparisons and daemon sizing; daemon throughput, density and history costs are unmeasured.

Net physical-memory savings and useful-task density from compression, trimmed-kernel benefits, full Ubuntu and macOS comparisons have not completed validation with current artifacts; no advantage is claimed for them. Cloud services, gVisor/Kata and complete RL throughput have no matched ranking. Apply, network, isolation and replay require their own evidence and cannot be inferred from these short tasks.

[Network](network.md) · [Apply](supervision-cost.md#apply-cost) · [Density](density.md) · [VM memory](vm-memory/index.md) · [Isolation](isolation-tests.md) · [Replay](replay-fidelity.md)

[End-to-end tasks](agent-tasks.md) combines tool waiting, workspace lifecycle and CLI compatibility to analyze Agent sandbox, container, VM and cloud tradeoffs. [Reinforcement learning](compare-rl-infra.md) maps tool costs, active capacity and historical-prefix preparation to rollouts; full training throughput and reward are unmeasured.

### Measurement coverage of the new framework {#measurement-coverage}

This table separates new measurements from historical evidence. Data is updated only when metrics, inputs, configurations and correctness gates match. Ready, normal exit, internal tool timings, memory and correctness do not substitute for one another. Unprepared experiments produce no new numbers; historical conclusions apply to their original cohorts.

{{ table("measurement-coverage.render.json", {"markdown":true}) }}

Filesystem coverage refers to the nine-mode, 19-workload N3 protocol, with four incomplete resource cells; historical N5/N30 does not fill its gaps.

### Sources and reproduction {#run}

[Nine-mode layered statistics](../../../overrides/assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-levels.csv) · [Ready](../../../overrides/assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-ready.csv) · [Provenance summary](../../../overrides/assets/benchmarks/filesystem-layered-references/provenance.json)

Complete CSV/JSON and raw records for other topics remain in their local cohorts or the documentation data archive.

[Method](methodology.md) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
