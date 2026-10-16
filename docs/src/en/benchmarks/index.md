# Where does pVisor have an advantage over existing workflows?

## Conclusions {#conclusions}

**For frequently created large workspaces with sparse changes, pVisor stage completes the machine workflow sooner: changing twenty of 10,000 files and retaining ten takes 141 ms in stage, 248 ms with Git worktree and 343 ms with btrfs reflink. Native workflows are faster for small workspaces; budget VM tool waiting from each configuration’s independent measurements.**

| Scenario | Selection implication |
|---|---|
| Large workspace, sparse changes, disposable view | Stage has a measured complete-workflow advantage |
| Small workspace or tool execution alone | Compare native workflows and container costs |
| VM or concurrent capacity | Require complete tool and resource measurements |

<!-- measurement-status:start -->

Filesystem measurements use Ready and three resident rounds: five independent environments for each of nine configurations, 45 successful environments, no interference gate. Historical N30 and single-pass tasks remain separate; other topics retain their own cohorts.

<!-- measurement-status:end -->

## Motivation {#motivation}

Selection requires the total cost of producing the same result, beyond startup or a single command. Review/application, execution boundaries and resource use determine which Agent workflows suit pVisor.

## Experiment design {#interpretation}

This reuses separately registered [startup](startup.md), [filesystem](filesystem.md), [repair](agent-tasks.md) and [complete review workflow](supervision-cost.md) experiments. Startup and repair use 60 samples per backend with three warmups; current filesystem measurements use five fresh environments per configuration and three resident rounds, without separate warmups; historical N30 remains separate; review uses thirty samples and three warmups per size/condition/backend. Same host, CPUs 0,1, warm caches, randomized interleaving and complete correctness checks, with no speed-based exclusions. Workloads, resources and timing boundaries differ across topics; distributions are not pooled. Docker uses rootless overlay2; workflow controls are native Git/reflink, not container or VM security rankings.

[Network](network.md) uses thirty independent batches and three warmups per condition; each small-request sample is the median of 256 requests. The origin is outside the payload CPU budget and memory is not identically capped. [Capacity](density.md) uses a shared two-core, 2 GiB, zero-swap budget, five rounds per concurrency and separate idle/useful-tool conditions; every failure, unknown outcome and OOM remains counted.

[On-demand image startup](lazy-image-startup.md) measures cold/warm client-cache waits and content payloads for the same pinned image within separate Ubuntu shell and Python/NumPy cohorts. Both workloads on the current artifacts support shorter lazy cold-client median waits; warm-cache Docker is faster. File content, binary indexes and total responses are counted separately. It uses prepared local registry/cache services, with different networking, Docker image store and memory configurations from the startup controls above; results remain separate rather than forming a single ranking.

## Data and analysis {#results}

The 2026-10-06 startup/filesystem/repair cohorts have 1,440 valid samples and review has 360; the separate 2026-10-07 stock-kernel startup cohort has 240 valid samples. All have zero measured failures. The selected-source filesystem comparison on 2026-10-12 adds 240 valid samples and zero failures, with task-local caches and a common parent resource budget. Cohorts are not pooled; topic pages provide median differences and paired-bootstrap 95% intervals.

| Question | Measured level (timing is P50) |
|---|---|
| [Startup](startup.md) | Stock controls: VM 100.91 ms; Firecracker 285.04 ms; QEMU microvm 321.56 ms; q35 702.98 ms |
| [Repair completion](agent-tasks.md) | staged 0.64 s; Docker 0.81 s; VM 3.25 s; QEMU microvm 1.40 s |
| [Ready and three tool rounds](filesystem.md#quick-results) | N=5, Ready: staged 209.56 ms; Docker 450.94 ms; VM 749.15 ms; Firecracker 854.79 ms |
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

[Coverage CSV](measurement-coverage.csv). Filesystem full-coverage status refers to historical N30 acceptance; current Ready/resident observations have five independent environments per configuration and three rounds within each.

### Downloads and reproduction {#run}

[Per-round statistics CSV](filesystem-resident.csv) · [Same-cohort differences and intervals](filesystem-resident-comparisons.csv) · [Provenance and conditions](filesystem-resident-provenance.csv)

[Historical N30 filesystem statistics](filesystem.csv) · [Same-cohort filesystem intervals](filesystem-comparisons.csv) · [Historical N30 filesystem provenance](filesystem-provenance.csv) · [Historical runtime statistics](runtime-summary.csv) · [Historical runtime intervals](runtime-comparisons.csv) · [Historical runtime provenance](runtime-provenance.csv) · [Workflow statistics](workflow-summary.csv) · [Workflow intervals](workflow-comparisons.csv) · [Workflow provenance](workflow-provenance.csv) · [Method](methodology.md) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
