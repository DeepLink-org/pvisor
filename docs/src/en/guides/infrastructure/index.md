# Agent execution infrastructure

pVisor runs tools on an execution node and returns exit status, files, and execution records to an inference service, evaluator, or trainer. Start with one sample on one machine. Check that the caller can retrieve a successful result and distinguish an environment that failed to start from a task that produced a wrong answer. Then use the same workload to evaluate concurrency and state reuse.

Use the [execution infrastructure quick start](../../start/infrastructure.md) to run a SWE tool-call excerpt and learn how to submit tasks and collect feedback through a CLI adapter.

## What needs to survive between tool calls? {#agent-native}

An agent edits a source file, waits for the next model response, then runs a test. The test needs the edited file. If it also relies on a running service, saving files alone is not enough. A VM can retain its memory throughout the wait even when its CPU is idle. Choose whether to keep the environment running, save the workspace, or offload the VM on a supported configuration based on what the next call needs.

Before choosing the CLI, embedded Runtime, daemon API, or containerd shim, check the [integration matrix](integration.md) for the state that interface can preserve and the results it can return.

## Responsibilities across a task {#lifecycle}

1. **Prepare inputs:** the caller pins the task, agent/model versions, workspace baseline, image identity, and resource budget.
2. **Create and execute:** start the environment through the selected interface, check admission and installed controls, and run the workload.
3. **Wait or save:** keep the running environment, a file branch, or a supported execution checkpoint according to the state required.
4. **Deliver results:** associate exit results, candidate files, and available records with the upstream task and execution.
5. **Release resources:** reconcile operation outcomes, retain required artifacts, and clean up through that interface's lifecycle.

The surrounding system owns task queues, model sampling, reward computation, retry policy, and cross-node placement. Successful pVisor execution establishes that the corresponding operation completed. An evaluator or business rule determines task correctness and whether the result is a valid training sample.

## Preserve context for inference services {#inference}

An inference request may need several consecutive tool calls that accumulate results in one workspace. Identify the state a session needs, then choose an interface that supports the required calls. Host process memory, guest files, agent history, and external service data have distinct owners.

| State to retain | Purpose | What to verify |
| --- | --- | --- |
| Candidate files and conflict baselines | Continue with a different command or accept selected changes | Executor and policy prerequisites for workspace checkpoint/fork |
| CPU, RAM, devices, and filesystem state | Continue or branch from a saved execution point | Profile, version, and host constraints for execution checkpoints |
| Agent trajectories and model context | Prepare a tool prefix and continue an agent session | Replay adapter support, repeated side effects, and output differences |

See [workspace checkpoints](../fork-checkpoint.md) for file branches, the [CLI contract](../../reference/cli.md#full-vm-execution-checkpoints) for execution restoration, and the [replay guide](../replay.md) for trajectory continuation. These methods retain different kinds of state.

Trajectory capture and replay also apply to [local Agent workflows](../index.md#trajectories). Validate [capture](../capture.md) and [replay](../replay.md) with one task before integrating the same workflow into inference or evaluation services.

## Deliver samples for evaluation and RL {#rl}

Use the [single-rollout example](../rl-rollouts.md) to validate “execute → read candidates → score → save the sample → clean up.” It collects results through native Sessions and can connect to an existing evaluator on one machine.

As sample volume grows, keep a separate workspace and identity for every execution. Associate task ID, execution identity, input versions, trajectory locations, and score. Record execution failure, access denial, recording failure, and task score separately so environment failures do not become model-capability judgments.

Explicitly enable [capture](../capture.md) when model communication records are needed. External orchestration integrations own multi-node handoff and centralized evidence; see [execution and orchestration research](../../design/research/index.md#ownership) for the design and current gaps.

## Manage resources across the lifecycle {#resources}

During environment preparation, reuse content through the [shared image cache](../../reference/shared-image-cache.md). During tool execution, verify resource limits and actual use. During model waits, evaluate whether memory sharing, compression, or offload suits the workload.

These memory mechanisms differ in platform support, mutual exclusions, and restoration semantics; choose using the [memory subsystem](../../design/memory-optimization/index.md). Short waits, write-heavy workloads, or poorly compressible content may not offset scanning, saving, and restoration costs.

Capacity evaluation should cover valid completions, tool latency, total host and shared-service memory, and failure and cleanup paths. [Density experiments](../../benchmarks/density.md) and [VM memory experiments](../../benchmarks/vm-memory/index.md) describe their own workloads and artifacts; they do not establish concurrency guarantees for every deployment.

## Start from a current interface {#start}

For file results and native Session records, validate the CLI or Rust Runtime first. For HTTP sandbox lifecycle, prepare the service according to the [daemon deployment prerequisites](../daemon/index.md). For an existing containerd call chain, check the [shim's interface scope](integration.md#containerd).

The [integration matrix](integration.md#capabilities) lists daemon bootstrap, end-to-end SDK validation, and capability gaps. Full RL training throughput, reward, and cross-host scaling benefits still require upstream workload experiments; see [RL scenario comparisons](../../benchmarks/compare-rl-infra.md) for existing cost analysis.
