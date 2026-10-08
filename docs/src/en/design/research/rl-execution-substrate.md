# An execution substrate for reinforcement learning

Agent training needs traceable attempts: their starting inputs, tool calls, results, and reasons for stopping. pVisor can provide individual execution and evidence collection while the trainer retains reward, sampling, and scheduling in its control layer.

## The execution contract for a rollout {#contract}

Inputs include task ID, attempt ID, repository commit or image digest, workspace, agent command, policy, resource budget, and seed. Outputs include terminal status, exit code, candidate files, Run Bundle, native trajectory, and a Gateway Journal when capture is enabled.

The trainer associates these artifacts with model version, sampling parameters, reward, and evaluator version. Setup failures, permission rejection, and upstream failures have distinct categories; evaluate model task-solving ability using predetermined handling for these infrastructure failures.

## Three kinds of reusable state {#state}

| State | What it reuses | What must be saved separately |
| --- | --- | --- |
| File checkpoint | Candidate files and staged layers over a fixed baseline | Model context and external service state |
| Native agent trajectory | Session lineage, tool calls, and observations | Resettable workspace, agent and model versions |
| VM snapshot | VM runtime state supported by the specific implementation | Scheduler metadata, model service, and remote side effects |

Re-executing tools produces fresh observations. Reproducibility experiments should fix inputs and versions while recording those differences; fresh replay observations and old session text cannot simply be treated as identical experimental inputs.

## A minimal integration sequence {#integration}

Start with the [rollout guide](../../guides/rl-rollouts.md) to collect and score one offline candidate. Increase concurrency with independent workspaces and verify that every attempt has separate files and records. Then add native agent trajectories and validate prefix preparation, tool re-execution, and model continuation separately.

The evaluator calculates reward. Successful pVisor completion is an execution outcome; task scores require task-specific tests, and file changes must be judged against the task requirements.

## What experiments must establish {#experiments}

Measure completed rollouts per second, tail latency, candidate storage and resident memory, fork/checkpoint cost, tool replay cost, and reward delta. Fix model-service capacity and distinguish substrate bottlenecks from model-service bottlenecks.

Asynchronous queues, cross-node recovery, reward deduplication, shared-prefix benefits, and large-scale sampling need integration and measurement. See [Replay fidelity](../../benchmarks/replay-fidelity.md), [Concurrent density](../../benchmarks/density.md), and [Cluster execution](cluster-execution.md) for methods.
