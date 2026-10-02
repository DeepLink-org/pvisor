# Glossary

| Term | Meaning | Authoritative page |
| --- | --- | --- |
| Job | A persistent CLI unit of work: one managed command, its evidence and staged changes | [Jobs and storage](jobs.md) |
| Run | The internal Job record; disk IDs look like `run-<uuid>` | [Jobs and storage](jobs.md) |
| Attempt | One execution by an executor | [Execution model](../design/execution-model.md) |
| Session | Owns an Attempt's lifecycle: resource preparation, cancellation, cleanup and terminal publication | [Execution model](../design/execution-model.md) |
| Operation | An operation pVisor processes; the current production operation is `run.execute` | [Operation and Event](../design/operations-events.md) |
| Executor | Backend providing the execution environment and boundary: host, container or VM | [Choose an executor](../guides/executors/index.md) |
| Stage | Copy-on-write workspace upper layer that leaves the project unchanged before apply | [Staging and apply semantics](staging.md) |
| apply | Write selected staged changes to the target workspace; reject conflicts | [Staging and apply semantics](staging.md) |
| drop | Discard staged changes that have not been applied; a terminal state | [Staging and apply semantics](staging.md) |
| Logical checkpoint | A staged filesystem snapshot without process memory | [Checkpoints and forks](../guides/fork-checkpoint.md) |
| fork | Create a new Job with lineage from a stopped Job's checkpoint | [Checkpoints and forks](../guides/fork-checkpoint.md) |
| Run Bundle | `run-bundle.json`: result, safety summary, changes, executor observations and artifacts | [Capabilities, evidence and guarantees](capabilities-and-evidence.md) |
| Capability dimension | File read, file write, network, subprocess, credentials, model, tools or resources | [Capabilities, evidence and guarantees](capabilities-and-evidence.md) |
| Admission plan (`ExecutorPlan`) | An executor's declaration before launch; its strongest level is `Planned` | [Capabilities, evidence and guarantees](capabilities-and-evidence.md) |
| Executor observations (`ExecutorObservations`) | Actual controls returned at completion; the sole evidence of enforcement | [Capabilities, evidence and guarantees](capabilities-and-evidence.md) |
| Enforced / Cooperative / Unenforced | Actual control levels: mandatory, cooperative or not enforced | [Capabilities, evidence and guarantees](capabilities-and-evidence.md) |
| `--safe` | Preset staging the workspace, protecting default sensitive paths, allowing the agent's model API and requiring executor isolation | [CLI reference](../reference/cli.md#safe-参数预设) |
| `--strict` | Refuse execution unless every requested capability has non-bypassable enforcement evidence | [Policy model](policy-model.md) |
| OverlayFS | pVisor's copy-on-write filesystem layer for staging and file rules | [OverlayCore design (planned)](../design/overlayfs.md) |
| OverlayNet | Network policy layer: proxy on host/container, smoltcp data plane in a VM | [Network policy](../guides/policies/network.md) |
| Gateway | Optional model gateway for protocol conversion, upstream keys and request capture | [Gateway capture](../guides/capture.md) |
| AgentCtl | Cooperative control channel between an agent and pVisor; not isolation evidence | [Execution model](../design/execution-model.md) |
| Replay | Replay a recorded tool prefix, then continue the agent live | [Replay](../guides/replay.md) |
| Trust ladder | L0–L3 levels moving human involvement from individual approval to retrospective audit | [Trust ladder](../why/trust-ladder.md) |
