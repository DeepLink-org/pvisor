# Why pVisor

Because letting an agent finish a task unattended takes three execution-layer things, not a stronger model: limit the files and network it can touch, hold its changes short of the target until you decide, and leave a verifiable record of what ran. pVisor is the runtime that builds these three into every execution.

## Reduce intervention during execution

How long an agent can run depends on how long you are willing to watch. Approve every command and human attention becomes the ceiling; go fully automatic and you must accept that it may corrupt files, read what it should not, and send requests where they should not go.

More tasks take more attention when every command needs approval. Execution boundaries and post-run review can reduce intervention while tasks run. The difference is amortization: process review adds one decision per step and needs a human present throughout; deferred review amortizes—results can be sampled, batched, and machine-checked, and once process facts are recorded completely they can be verified after the fact in one pass instead of being intercepted during execution.

## Decouple supervision from execution volume

To let go, every execution needs three things at once:

| Property | Meaning | What it means for you |
| --- | --- | --- |
| Bounded | The blast radius of a mistake is known in advance | No need to approve every command |
| Reversible | Changes can be selectively merged, discarded, or forked before they are applied | Failure is cheap, so you dare to let go |
| Checkable | Execution leaves a record you can verify | Review can sample, or even be handed to a machine |

pVisor implements the three concretely: access boundaries installed at runtime ([file and network policies](../guides/policies/files.md)), file changes staged in a private upper until you merge them ([staging and apply](../concepts/staging.md)), and identity, event and result records written for every Job ([capabilities, evidence, and guarantee boundaries](../concepts/capabilities-and-evidence.md)).

Scaling also calls for controlling environment preparation, tool execution, model waiting, and repeated work after failures. [Agent task lifecycle cost](../design/research/agent-lifecycle-cost.md) connects these costs to pVisor's execution paths, choice of isolation mechanisms, and the core insight and engineering challenge of letting the host take over I/O while the guest is offloaded during waits.

## Responsibilities of the execution layer

pVisor is the **semantic layer for each execution**: it defines boundaries, installs controls, and leaves evidence.

Executors provide isolation through host namespaces/Seatbelt, containers, or VMs. pVisor manages execution policy, lifecycle, and records; external systems such as Kubernetes and Ray handle cross-node orchestration.

## Today, and after {#trust-ladder}

By the timing and degree of human intervention, autonomous execution falls into four levels:

| Level | How humans intervene (trust axis) | What carries it (scale axis) | Status |
| --- | --- | --- | --- |
| L0 | Approve each command before it runs | A single interactive session | Common in agents, now with model-assisted approval |
| L1 | Review each change after the run | Local, individual Jobs | **pVisor today** |
| L2 | Policy and evidence determine exemptions; humans spot-check | Multiple local Jobs / one pipeline | Next |
| L3 | Audit afterward; humans handle exceptions only | Clustered: cross-node scheduling, centralized evidence | pVisor's scale target |

The four levels are a reference; pVisor's choice is L1: enforce access boundaries at runtime, stage file changes, then inspect records and results—use the `--ask` runtime approval channel when individual operations need approval. The [single-node daemon](../guides/daemon/index.md) manages local sandbox lifecycle through an API separate from native Job review; external systems such as Kubernetes and Ray handle cross-node scheduling, with pVisor providing execution semantics and checkable records within them.

The same capabilities solve different problems at different scales: a fully automated refactor with one person and one agent works today; parallel runs and batch review across many agents are next (see [parallel agents](../guides/parallel-agents.md)); teams and platforms can adopt the L1 workflow in CI today (see [running in CI](../guides/ci.md)); agentic RL and evaluation need a recorded, forkable, replayable batch execution substrate (see [RL rollouts](../guides/rl-rollouts.md) and [research directions](../design/research/index.md)).

!!! note "Under construction"
    "Human supervision cost per unit of agent work" has no measured data yet—requirements and acceptance criteria are in [supervision cost](../benchmarks/supervision-cost.md). The entry criteria and gap list for L2/L3 are not final; see the [roadmap](../community/roadmap.md) for progress and [concurrency density](../benchmarks/density.md) for the capacity rationale.
