# Retired Cluster benchmark plan

**The Controller/Worker experiment plan is retired, not transferred to the daemon.** B-CLUSTER has no active runner or acceptance protocol. The [historical measurements](../benchmarks/cluster-scalability.md) answer questions only about their frozen artifacts; they do not validate the proposals below or current daemon density.

## Existing evidence {#existing}

The [technical archive](cluster-performance-analysis.md) distinguishes growing-budget readiness probes, synthetic history queries and the independent completed-task cohort. Readiness is not completion throughput; fixture RSS is not net control-plane memory; warm replay is not full recovery. Historical tables, CSVs and raw `.data/` remain in their original cohorts, without relabeling or new measurements.

## Startup attribution proposal {#q1}

Comparing direct VM execution with old Cluster dispatch was an unfulfilled attribution proposal, not a current Controller/Worker experiment. Old debug, release and quota configurations cannot be subtracted to infer scheduling cost.

## Marginal-memory proposal {#q2}

Separating shared bases, private working sets and offload recovery remains a measurement question, not a proven Cluster density benefit. Independent B-DENSITY and B-VM-MEMORY probes continue under their own registry entries and receipts. Single-VM reclaimed memory cannot be converted into daemon capacity.

## Fixed-budget useful-work proposal {#q3}

The proposed model-wait/CPU-release A/B was not validated by shell readiness or by the separate Python/Git completion cohort. The retired `release_cpu_on_idle` path is not an active measurement target. No current effective-Agent-throughput or unit-cost claim follows.

## History-scaling proposal {#q4}

Archived count calls and retained-record replay do not establish HTTP scheduling throughput, large active-set behavior or current daemon history costs. No old scheduler example is an active measurement entry.

## Recovery proposal {#q5}

The old Controller/Worker reconciliation experiment is retired. Local warm-log replay cannot establish end-to-end recovery, convergence under partition or exactly-once execution. No safety claim or receipt is weakened by retiring the experiment.

## Sustained-operation proposal {#q6}

Short probes cannot establish bounded long-term resource use. The former persistent-Worker plan is not a current daemon reliability or retention test.

## Requirements for any replacement experiment {#protocol}

A replacement needs its own registered subject and user question, frozen source/binary/input identities, verified whole-tree CPU/memory/zero-swap controls, identical workloads, declared sample counts and stopping rules, and complete correctness, conflict and recovery checks. Retain failures, unknown outcomes and OOMs. Keep independent batches and correlated in-batch observations distinct; use paired uncertainty for A/B claims. Freeze the protocol before formal sampling, and never retrospectively approve a hypothesis from exploratory data.

These requirements do not authorize a new measurement or semantic approval. Human-reviewed claims, receipts, semspec checks and approval records remain unchanged.

## Current scope {#priority}

There is no executable Cluster priority list or reproduction command. Use [local capacity](../benchmarks/density.md) and [VM memory](../benchmarks/vm-memory/index.md) only within their documented native-probe scope. Daemon density, multi-host scalability and matched scheduler comparisons remain unmeasured. The [runner manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md) lists the retired entries and independent active probes.
