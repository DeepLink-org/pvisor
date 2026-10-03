# An execution layer for agentic RL rollouts and evaluation

Treat each rollout as an independent task: prepare a workspace, run the agent, collect its trajectory and file result, calculate reward, then retain or discard its changes. pVisor supplies execution and evidence; the trainer owns sampling, reward, and scheduling.

First validate result collection with an offline task:

```bash
pvisor run --safe --overlaynet-deny-all --stdio capture \
  --stage ../rollout-001 -- /bin/sh -c 'printf "candidate\n" > answer.txt'
pvisor status --review --json ../rollout-001 > rollout-001.bundle.json
pvisor inspect ../rollout-001 -- cat answer.txt
```

Expect a completed task, exit code zero, and a staged `answer.txt`. Have the evaluator read the candidate in the Stage, then run `pvisor drop ../rollout-001` after scoring. Training candidates do not need to be applied to the baseline.

## How to assemble one rollout today

An evaluation sample today can be one command execution, its model traffic capture, and the native agent trajectory. Prepare a clean workspace for the sample, then run the command and keep the Job ID, Run Bundle, Event Journal, and native agent trajectory. Reward computation, the task queue, model training, and cross-node scheduling stay with your existing framework.

| Artifact | Purpose | Does not replace |
| --- | --- | --- |
| Run Bundle | Outcome, controls, and file changes | Training-framework reward and dataset metadata |
| Gateway Journal | Model calls through the Gateway | Uncaptured traffic and native agent sessions |
| Native agent trajectory | Tool-prefix replay for the matching adapter | Process memory snapshots |
| Logical checkpoint | Forking staged file state | Full environment and external service state |

## Pin the sample identity

Record the task ID, pVisor commit, agent/model versions, initial repository commit, image digest, sampling parameters, policy, and executor for every rollout. Track task failure, isolation refusal, recording failure, and continuation quality separately; never record an infrastructure failure as "the model was incapable".

Replay reruns tools in a new environment and repeats their side effects. Use test APIs/databases and a separate output directory; validate the format with `--prepare-only`, validate the tool prefix with `--replay-only`, and then start a real continuation. See [replay](replay.md) for the pinned adapter versions and boundary semantics. Cluster throughput and hostile multi-tenancy are still unproven.

## Score and retain a training sample {#score}

Use this minimal evaluator for the preceding offline task. It checks execution first, then candidate content. The example requires `jq`.

```bash
jq -e '.schema_version == 4 and .run.state == "completed" and .run.exit_code == 0' \
  rollout-001.bundle.json
pvisor inspect ../rollout-001 -- cat answer.txt > rollout-001.answer.txt
reward=0
if [ "$(cat rollout-001.answer.txt)" = candidate ]; then
  reward=1
fi
jq -n --arg task_id sample-001 --argjson reward "$reward" \
  '{task_id: $task_id, reward: $reward, bundle: "rollout-001.bundle.json"}' \
  > rollout-001.score.json
pvisor drop ../rollout-001
```

This reward checks content only; replace it with unit tests, environment scores, or human labels for real tasks. If execution checks or file reading fail, retain diagnostics as a failed sample and stop normal scoring. In CI, use `set -e` or explicitly check every return value.

Configure the agent's native trajectory output using [Agent integrations](agents/index.md); follow [Model traffic capture](capture.md) for Gateway recording. Associate trajectories, Bundles, and scores with one task/attempt ID. For batches, reuse the [parallel workspace workflow](parallel-agents.md#batch) with a fresh Stage for every sample.
