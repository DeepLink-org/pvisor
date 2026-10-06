# Where does pVisor have an advantage over existing workflows?

## Conclusions {#conclusions}

**For frequently created large workspaces with sparse changes, pVisor stage completes the machine workflow sooner: changing twenty of 10,000 files and retaining ten takes 141 ms in stage, 248 ms with Git worktree and 343 ms with btrfs reflink. Native workflows are faster for small workspaces; pVisor VM tools still take longer than the tested Firecracker/QEMU configurations.**

| Scenario | Selection implication |
|---|---|
| Large workspace, sparse changes, disposable view | Stage has a measured complete-workflow advantage |
| Small workspace or tool execution alone | Compare native workflows and container costs |
| VM or concurrent capacity | Require complete tool and resource measurements |

## Motivation {#motivation}

Selection requires the total cost of producing the same result, beyond startup or a single command. Review/application, execution boundaries and resource use determine which Agent workflows suit pVisor.

## Experiment design {#interpretation}

This reuses separately registered [startup](startup.md), [filesystem](filesystem.md), [repair](agent-tasks.md) and [complete review workflow](supervision-cost.md) experiments. The first three use 60 samples and three warmups per backend; review uses thirty samples and three warmups per size/condition/backend. Same host, CPUs 0,1, warm caches, randomized interleaving and complete correctness checks, with no speed-based exclusions. Workloads, resources and timing boundaries differ across topics; distributions are not pooled. Docker uses rootless overlay2; workflow controls are native Git/reflink, not container or VM security rankings.

[Network](network.md) uses thirty independent batches and three warmups per condition; each small-request sample is the median of 256 requests. The origin is outside the payload CPU budget and memory is not identically capped. [Capacity](density.md) uses a shared two-core, 2 GiB, zero-swap budget, five rounds per concurrency and separate idle/useful-tool conditions; every failure, unknown outcome and OOM remains counted.

## Data and analysis {#results}

2026-10-06; 1,440 valid startup/filesystem/repair samples and 360 valid review samples, all with zero failures. Topic pages provide median differences and paired-bootstrap 95% intervals.

| Question | Measured level (timing is P50) |
|---|---|
| [Startup](startup.md) | host 12.72 ms; staged 25.13 ms; VM 100.73 ms; Docker 74.48 ms |
| [Repair completion](agent-tasks.md) | staged 0.64 s; Docker 0.81 s; VM 3.25 s; QEMU microvm 1.40 s |
| [Seven-tool completion](filesystem.md) | staged 1.09 s; Docker 0.82 s; VM 4.27 s; Firecracker 2.29 s |
| [Review workflow](supervision-cost.md) | 10,000 files: stage 141 ms; Git 248 ms; reflink 343 ms |
| [Network](network.md) | Eight-thread small requests: native 0.69 ms; host proxy 10.09 ms; VM 2.10 ms |
| [Active capacity](density.md) | 2 GiB: stage/Podman pass all five rounds at 32; VM at 16 |


Network has 510 valid batches. Capacity retains all 320 batches, including 57 failures. These experiments remain separate and do not form one capacity or speed ranking. Retired Controller/Worker results are [historical evidence](cluster-scalability.md), excluded from current comparisons and daemon sizing; daemon throughput, density and history costs are unmeasured.

Net physical-memory savings and useful-task density from compression, trimmed-kernel benefits, full Ubuntu and macOS comparisons have not completed validation with current artifacts; no advantage is claimed for them. Cloud services, gVisor/Kata and complete RL throughput have no matched ranking. Apply, network, isolation and replay require their own evidence and cannot be inferred from these short tasks.

[Network](network.md) · [Apply](apply.md) · [Density](density.md) · [VM memory](vm-memory/index.md) · [Isolation](isolation-tests.md) · [Replay](replay-fidelity.md)

[Docker/devcontainer](compare-containers.md) · [Firecracker/QEMU/gVisor/Kata](compare-runtimes.md) · [Agent sandboxes](compare-agent-sandboxes.md) · [E2B/Daytona/Modal](compare-cloud-sandboxes.md) · [Agent RL infrastructure](compare-rl-infra.md)

### Downloads and reproduction {#run}

[Runtime statistics](runtime-summary.csv) · [Confidence intervals](runtime-comparisons.csv) · [Runtime provenance](runtime-provenance.csv) · [Workflow statistics](workflow-summary.csv) · [Workflow intervals](workflow-comparisons.csv) · [Workflow provenance](workflow-provenance.csv) · [Method](methodology.md) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
