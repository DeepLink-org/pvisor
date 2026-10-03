# Comparison: agent RL rollout infrastructure

pVisor can supply execution and evidence per rollout: separate environments, changes, trajectories and recovery. Training algorithms, inference, rewards and cluster scheduling remain with the training system. This edition measures local execution and replay contracts, not improved training throughput.

## Scope on 2026-10-04

| Option | Main responsibility | Relationship to pVisor |
|---|---|---|
| OpenHands sandbox/runtime | Agent tools and work environments; Docker sandbox can mount local repositories | Compare workspace/execution boundaries; the replay adapter's pinned contract does not prove compatibility with latest OpenHands |
| SWE-Gym | Repository tasks, executable environments and test verification for agents/verifiers | Task/reward semantics; pVisor can manage attempt execution, but no full dataset integration was measured |
| Training frameworks such as verl | Training, rollout and model/compute coordination | Workers could invoke pVisor; no validated dedicated verl connector is included |
| pVisor | Job lifecycle, staging, Gateway traces, replay and VM snapshot/fork | Recorded execution units; caller owns models, tasks, rewards and scheduling |

Sources: [OpenHands](https://docs.openhands.dev/openhands/usage/sandboxes/docker), [SWE-Gym](https://github.com/SWE-Gym/SWE-Gym), [verl](https://verl.readthedocs.io/en/latest/). Success rates from different papers are not used as a ranking.

## Integration flow

Prepare a pinned task/rootfs, create a separate Run, route inference through Gateway when capturing, grade tests and reward, retain Bundle/stage, and recover failures through tool replay or checkpoint forks. Bind all artifacts to the training sample and declare processes, shares and endpoints.

Tool replay obtains fresh observations by repeating operations. VM checkpoints preserve CPU/RAM and supported device/file state. Neither implies arbitrary remote connections or model state are recoverable. [Replay fidelity](replay-fidelity.md) separates prefix reconstruction from live-model next actions; [VM snapshots](vm-memory/index.md) records costs and limits.

## Planning

Use [density](density.md) for idle-environment cost, then add compilation, tests, inference waits, trajectory I/O and task sizes. Concurrency with a 1-second hold probe is not useful rollouts per second. Compare graded successes, retries, time per useful sample and total resources.

Local execution/staging/snapshots have measurements; adapters have contract tests; full SWE-Gym/verl training throughput remains unmeasured. Keep established pipelines when sufficient; evaluate pVisor for shared recovery and evidence across agents.

## Corrections

Send framework/model versions, task set, concurrency and raw samples to [pVisor issues](https://github.com/DeepLink-org/pvisor/issues), including boundary and reward records.
