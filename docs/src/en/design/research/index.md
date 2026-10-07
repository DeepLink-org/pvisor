# Research directions

The OS/MLSys research question is whether bounded executions and retained evidence can reduce the cost of running and evaluating many agents under fixed resources. External orchestration integration remains integration research; the Agentic RL substrate remains planned. Both concern L2–L3 on the [trust ladder](../../why/trust-ladder.md), with workload acceptance and measurements still outstanding.

## Execution and orchestration ownership

pVisor owns individual execution boundaries and observations. External orchestration owns host selection, queues, dependencies and retries; a trainer also owns sampling, reward and evaluator versions. The [single-node daemon](../daemon/index.md) manages local VM sandboxes and service access, without exposing native Job staging, checkpoints or Run Bundle export. Its API profile therefore needs an explicit execution/evidence handoff before it can support those research workflows.

A proposed integration binds the orchestrator's work identity to local Job/Run/Attempt or sandbox identity, pins inputs and runtime versions, and collects outcomes with the corresponding artifacts. Unknown outcomes require reconciliation before retry: a timeout does not prove that tools or remote effects never ran. [External orchestration research](cluster-execution.md) develops this boundary; it does not propose a product-owned cluster scheduler.

## Reuse and evaluation

A file checkpoint reuses candidate files and conflict baselines; an agent trajectory preserves context and tool history; a supported VM execution checkpoint preserves the machine state captured by its profile. These are separate inputs to an experiment. Re-executing tools produces fresh observations, while remote service state and trainer metadata require separate handling. The [RL substrate proposal](rl-execution-substrate.md) keeps successful execution separate from task-specific reward.

Start with one independently recorded and evaluated attempt, then increase concurrency with separate workspaces and records. Evaluate prefix preparation, tool replay and model continuation independently before claiming reusable training prefixes. RL framework interfaces, rollout throughput and shared-prefix benefits remain integration and measurement work.

## What would establish a benefit

Fix the task, inputs, output checks, model-service capacity and total resource budget. Measure correct completions, first-result and restore tails, whole-group memory, checkpoint/fork costs and review effort. Include preparation, collection, storage and recovery overhead rather than ranking mechanisms by readiness or compression ratio alone.

Sharing immutable state can lower duplicate costs, but private writes, cold faults and restoration buffers can erase the gain. Failure experiments must retain unknown outcomes and distinguish local cleanup from business-effect reconciliation. Historical Controller/Worker measurements and native mechanism probes do not establish current daemon density or cross-host recovery.

[Public research outputs](publications.md) retain their source and experiment scope separately from implementation guarantees. No formal papers, external reports or talks are registered yet.
