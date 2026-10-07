# Trust ladder and scale

Autonomous execution can be viewed along two axes: **how humans intervene** (the trust axis) and **what carries execution** (the scale axis).

## Post-run review

**Enforce access boundaries during execution, then review changes and records together.**

During execution, file and network policies limit access, and staging retains changes for review. After execution, review the process record and results together, reducing the need to approve each command.

## Batch review

Process review does not amortize. Every extra step is another decision, and a human must be present throughout—how fast the agent runs depends on how closely you watch. That is today's ceiling.

Deferred review does amortize. Results can be sampled, batched, and machine-checked; once process facts are recorded completely, they can be verified after the fact in one pass instead of being intercepted during execution.

## Four levels are a reference; the choice is the last one

The four levels describe the timing and degree of human intervention. L0 requires approval before each command; L1 through L3 use post-run review and progressively introduce policy- and evidence-based exemptions. The table lists the usage scenarios for each level.

| Level | How humans intervene (trust axis) | What carries it (scale axis) | Status |
| --- | --- | --- | --- |
| L0 | Approve each command before it runs | A single interactive session | Common in agents, now with model-assisted approval |
| L1 | Review each change after the run | Local, individual Jobs | **pVisor today** |
| L2 | Policy and evidence determine exemptions; humans spot-check | Multiple local Jobs / one pipeline | Next |
| L3 | Audit afterward; humans handle exceptions only | Clustered: cross-node scheduling, centralized evidence | pVisor's scale target |

pVisor focuses on post-run review: enforce access boundaries, stage file changes, then inspect records and results. Use the `--ask` runtime approval channel when individual operations need approval.

## Review process and results together

Execution records retain observed file access, denials, and network activity. After the task finishes, review these records alongside the file changes.

## Where we are today

What ships today is this stage at single-machine scale: individual Jobs on one machine—run unattended, then review changes and evidence afterward, and merge only what you want. L2's exemption decisions and L3's cluster scale are not here yet.

The [single-node daemon](../guides/daemon/index.md) manages local sandbox lifecycle through an API separate from native Job review. External systems such as Kubernetes and Ray handle cross-node scheduling; pVisor provides execution semantics and checkable records within those systems.

!!! note "Under construction"
    The entry criteria and gap list for L2/L3 are not final. See the [roadmap](../community/roadmap.md) for progress, [concurrency density](../benchmarks/density.md) for the capacity rationale, and [cluster execution](../design/research/cluster-execution.md) for the cluster direction.
