# Why pVisor

Use pVisor when you want an agent to finish a task without watching every command. It limits what the agent can touch, holds its file changes until you decide what to merge, and records what ran. A mistake stays bounded, a failed attempt is cheap to drop, and a result can be checked after the run.

## Reduce intervention during execution

An agent can run only as long as you are willing to watch it. Approve every command and your attention becomes the ceiling. Switch to full auto and you accept that it may corrupt files, read what it should not, or send requests to the wrong host.

Approval cost scales with steps; review cost scales with results. Deferring review lets you sample results, batch them, and machine-check them. A complete record can be verified in one pass after the run instead of intercepted during it.

## Decouple supervision from execution volume

Unattended execution needs three properties at once:

| Property | Meaning | What it means for you |
| --- | --- | --- |
| Bounded | The blast radius of a mistake is known in advance | No need to approve every command |
| Reversible | Changes can be selectively merged, discarded, or forked before they are applied | Failure is cheap, so you dare to let go |
| Checkable | Execution leaves a record you can verify | Review can sample, or even be handed to a machine |

Each property has a mechanism: [file and network policies](../guides/policies/files.md) installed at runtime, [staging](../concepts/staging.md) that holds changes until you merge them, and [records](../concepts/capabilities-and-evidence.md) written for every Job.

At scale, environment preparation, tool execution, model waiting and failure rework enter the bill too. [Agent task lifecycle cost](../design/research/agent-lifecycle-cost.md) accounts for them across pVisor's execution paths and isolation choices, including host-side I/O takeover while a guest is offloaded during a wait.

## Responsibilities of the execution layer

pVisor is the **semantic layer for each execution**: it defines boundaries, installs controls, and leaves evidence.

Executors provide isolation through host namespaces/Seatbelt, containers, or VMs. pVisor manages execution policy, lifecycle, and records; external systems such as Kubernetes and Ray handle cross-node orchestration.

## Today, and after {#trust-ladder}

Autonomous execution falls into four levels, by when and by how much a human intervenes:

| Level | How humans intervene (trust axis) | What carries it (scale axis) | Status |
| --- | --- | --- | --- |
| L0 | Approve each command before it runs | A single interactive session | Common in agents, now with model-assisted approval |
| L1 | Review each change after the run | Local, individual Jobs | **pVisor today** |
| L2 | Policy and evidence determine exemptions; humans spot-check | Multiple local Jobs / one pipeline | Next |
| L3 | Audit afterward; humans handle exceptions only | Clustered: cross-node scheduling, centralized evidence | pVisor's scale target |

pVisor targets L1 today: enforce access boundaries at runtime, stage file changes, then inspect records and results. Use the `--ask` approval channel when an individual operation needs a human decision. The [single-node daemon](../guides/daemon/index.md) manages local sandbox lifecycle through an API separate from native Job review; Kubernetes, Ray and similar systems own cross-node scheduling, with pVisor supplying execution semantics and checkable records.

- **One person, one agent.** An unattended refactor with the changes reviewed before merge: available today.
- **One person, many agents.** Parallel runs with batch review: next ([parallel agents](../guides/parallel-agents.md)).
- **Teams and platforms.** The L1 workflow in CI: available today ([running in CI](../guides/ci.md)).
- **Agentic RL and evaluation.** A recorded, forkable, replayable batch substrate: direction ([RL rollouts](../guides/rl-rollouts.md), [research directions](../design/research/index.md)).

!!! note "Under construction"
    Supervision cost per unit of agent work has no measurements yet; requirements and acceptance criteria are in [supervision cost](../benchmarks/supervision-cost.md). L2/L3 entry criteria are not final: see the [roadmap](../community/roadmap.md) for progress and [concurrency density](../benchmarks/density.md) for the capacity rationale.
