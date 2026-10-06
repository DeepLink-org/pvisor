# Trust ladder and scale

Autonomy is not reached in one step. It grows along two axes: **how humans intervene** (the trust axis) and **what carries execution** (the scale axis).

## Our claim

**Relax trust through mechanisms: move review from the process to the results, and defer it to the end.**

No per-command approval up front; during execution, boundaries and reversibility cap the cost of a mistake; after execution, process evidence and results are reviewed together, once. Supervision cost stops rising linearly with the number of steps, and that is what lets scale go up.

## Why defer, not add more gates

Process review does not amortize. Every extra step is another decision, and a human must be present throughout—how fast the agent runs depends on how closely you watch. That is today's ceiling.

Deferred review does amortize. Results can be sampled, batched, and machine-checked; once process facts are recorded completely, they can be verified after the fact in one pass instead of being intercepted during execution.

## Four levels are a reference; the choice is the last one

The four levels describe when and how much a human intervenes. They are coordinates, not steps you must climb in order: the fork is between L0 (approve each command before it runs) and the rest; L1 through L3 all defer review, differing in how much the human steps back (from L2, policy and evidence carry exemption decisions) and in the scale they carry (one machine → a pipeline → a cluster).

| Level | How humans intervene (trust axis) | What carries it (scale axis) | Status |
| --- | --- | --- | --- |
| L0 | Approve each command before it runs | A single interactive session | Common in agents, now with model-assisted approval |
| L1 | Review each change after the run | Local, individual Jobs | **pVisor today** |
| L2 | Policy and evidence determine exemptions; humans spot-check | Multiple local Jobs / one pipeline | Next |
| L3 | Audit afterward; humans handle exceptions only | Clustered: cross-node scheduling, centralized evidence | pVisor's scale target |

L0 is already a commodity and not worth re-investing in. pVisor's choice is to do only the stage after execution: no process gating by default, but solid boundaries, reversibility, and evidence, so review moves to the end. When you really do want approval while it runs, `--ask` provides a runtime approval channel—an exception, not the main line.

## Process review does not disappear

Deferring is not abandoning. Evidence carries the process facts—what was read or written, what was blocked, where the network went—past the run, so process review and result review happen together in one deferred pass. The difference is that these facts are read after execution instead of being approved step by step during it.

## Where we are today

What ships today is this stage at single-machine scale: individual Jobs on one machine—run unattended, then review changes and evidence afterward, and merge only what you want. L2's exemption decisions and L3's cluster scale are not here yet.

The [single-node daemon](../guides/daemon/index.md) adds local sandbox lifecycle, not L3 orchestration or the native Job review contract. pVisor does not provide a cluster-wide Controller/Worker control plane; external systems such as Kubernetes and Ray own cross-node scheduling. The scale target is execution semantics and checkable evidence within those systems.

!!! note "Under construction"
    The entry criteria and gap list for L2/L3 are not final. See the [roadmap](../community/roadmap.md) for progress, [concurrency density](../benchmarks/density.md) for the capacity rationale, and [cluster execution](../design/research/cluster-execution.md) for the cluster direction.
