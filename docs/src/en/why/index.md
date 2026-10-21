# What is PolicyVisor?

**PolicyVisor (pVisor) provides execution environments, policy controls, and records for agent CLIs, scripts, and tool calls.** Use it to review a local agent's file changes, or integrate it into inference, evaluation, and agentic RL post-training systems to manage task execution and state.

## Two workflows {#workflows}

| Scenario | Your task | pVisor's role | Who decides the result |
| --- | --- | --- | --- |
| [Local agent workflows](../guides/index.md) | Let an agent modify a project, then choose which changes to accept | With staging enabled, retain candidate files for review, selective apply, and execution evidence | You or a CI review step |
| [Agent execution infrastructure](../guides/infrastructure/index.md) | Prepare environments, run tools, and deliver results for inference requests, evaluation tasks, or rollouts | Provide executors, lifecycle management, and the state and resource mechanisms exposed by the selected interface | The agent service, evaluator, or trainer |

Both workflows can start on one machine. An individual can run several agents in parallel; a platform developer can validate a rollout with one Job. Task volume and human involvement are separate choices.

## Reduce intervention during execution

For local development, define the task's access scope, let the agent execute, then inspect the outcome and candidate changes. Enable staging with `--safe`, `--ask`, or an explicit `--stage`; ordinary host Jobs write directly to the workspace by default. Follow [your first run](../start/first-run.md) for a complete demonstration.

During review, apply selected paths or discard candidates that have not been applied. [Staging and apply semantics](../concepts/staging.md) define file acceptance and conflict handling; file staging does not undo remote API calls, database writes, or messages already sent.

## Decouple supervision from execution volume

The execution layer provides three properties you can inspect:

| Property | Mechanism | What to check |
| --- | --- | --- |
| Bounded | Executors install file, network, and resource controls | Controls actually installed on the current platform, and their gaps |
| Reversible | Unaccepted staged files can be discarded or kept as a branch | Staging scope, applied state, and fork prerequisites |
| Checkable | Jobs retain identity, outcomes, and execution observations | Exit results, installed controls, and file changes |

[Capabilities, evidence, and guarantee boundaries](../concepts/capabilities-and-evidence.md) distinguish requested policy from installed controls. Records support human or programmatic decisions; applications still define their own acceptance criteria.

Platforms also manage environment preparation, tool execution, model waiting, and failure rework. Image-content reuse, supported execution checkpoints, and memory-sharing or reclamation mechanisms address those stages. See the [infrastructure overview](../guides/infrastructure/index.md) for choices and current evidence.

## Responsibilities of the execution layer

The agent or application chooses which tools to call. pVisor manages execution lifecycle and access boundaries through the selected interface. Model sampling, reward computation, task queues, and cross-node placement belong to the caller or external orchestration system.

Native Jobs, the Rust Runtime, the daemon Sandbox API, and the containerd shim have distinct identities and interface scopes. Use the [integration matrix](../guides/infrastructure/integration.md) to choose an entry point; check each capability's exposure even when implementations share underlying mechanisms.

## Today, and after {#trust-ladder}

Human involvement and execution volume are independent axes:

- **Involvement:** approve individual commands, review after a task, spot-check after automated checks, or audit afterward.
- **Volume:** one task, local concurrency, or multiple machines scheduled by an external system.

The original L0–L3 labels remain a reference for supervision: L0 approves commands, L1 reviews individual results, L2 spot-checks using policy and evidence, and L3 focuses on audits and exceptions. Increasing concurrency does not establish L2/L3 acceptance rules; those rules and acceptance criteria remain in the [roadmap](../community/roadmap.md).

!!! note "Validate capabilities and benefits separately"
    Native Job review has an operational workflow; daemon delivery and validation gaps are listed in the [interface matrix](../guides/infrastructure/integration.md#capabilities). Benefits to human supervision time, full RL training throughput, and training quality have not been established. Existing [benchmarks](../benchmarks/index.md) support conclusions only within their own workloads and measurement scopes.

Complete a review through [local workflows](../guides/index.md), or choose an entry point through [execution infrastructure](../guides/infrastructure/index.md). For deciding whether your existing tools suffice, see [fit and comparisons](comparisons.md).
