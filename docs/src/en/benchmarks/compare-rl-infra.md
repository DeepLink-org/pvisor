# Comparison: Agent RL rollout infrastructure

## Main conclusions {#conclusions}

**pVisor supplies execution, staging, trajectories and recovery units per attempt, without evidence of higher complete RL-training throughput.** Local startup, tools and snapshots support budgeting, not inferred effective rollouts per second or percentage training savings.

Existing OpenHands/SWE-Gym/verl pipelines remain useful. Consider pVisor for common workspaces and execution evidence across Agents. Training systems still own models, tasks, rewards and scheduling.

## Motivation {#motivation}

A rollout includes tool execution, model waiting, verification, retries and sample retention, beyond sandbox creation. Infrastructure comparisons need total cost per useful result.

## Experiment design {#interpretation}

This chapter measures local execution and replay contracts, not complete SWE-Gym/verl training comparisons or success rates across papers. Official projects describe responsibilities; unaccepted dedicated integrations are not claimed compatible.

| Tool | Responsibility and scope |
|---|---|
| OpenHands runtime | Agent tool environments; Docker sandbox can mount local repositories |
| SWE-Gym | Repository tasks, executable environments, verification and Agent/verifier training |
| verl | Training, rollout and model resource coordination |
| pVisor | Jobs, stages, trajectories, replay and VM checkpoint/fork; not a training algorithm |

Sources: [OpenHands](https://docs.openhands.dev/openhands/usage/sandboxes/docker), [SWE-Gym](https://github.com/SWE-Gym/SWE-Gym), [verl](https://verl.readthedocs.io/en/latest/).

## Data and analysis {#results}

| Cost | Evidence | Supported use |
|---|---|---|
| [Startup](startup.md) | Local VM about 0.1 s | Disposable-environment waiting budget |
| [Tool tasks](agent-tasks.md) | Staged short repair about 0.7 s, VM about 4 s | Tool budget by execution boundary |
| [Prefix preparation](replay-fidelity.md) | Six pinned formats about 5–5.5 ms | Prepare recorded history; not identical model next actions |
| [Complete snapshots](vm-memory/index.md#linux-snapshot) | Raw save/restore about 0.71/0.93 s | Fixed recovery-branch costs |
| [Concurrency density](density.md) | Idle-environment probes | Baseline occupancy, not effective rollout throughput |

Tool replay reexecutes operations; VM checkpoints recover CPU/RAM and associated device/file state, without restoring every remote connection. Real training still needs success rates, retries, total resources and time per useful sample. Current evidence does not establish faster execution than complete RL pipelines.
