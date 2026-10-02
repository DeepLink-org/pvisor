# Capabilities, evidence, and guarantees

To judge what a Job actually guarantees, check the requested permissions, admission plans, installed controls and observed outcomes separately. `--safe`, a stage directory, or an executor name alone does not prove that every capability is enforced.

## Check each dimension

| Dimension | Check |
| --- | --- |
| File reads | Visible host paths, projected root, installed read controls |
| File writes | Workspace stage, directly writable shares, installed write boundary |
| Network | Proxy/VM data path, direct-socket bypass, internal Gateway routes |
| Subprocesses | Tree constraints, namespace/profile, inherited handles, cleanup scope |
| Credentials | Environment projection and shares; no inferred use tracking or expiry |
| Models | Declared model permissions and installed routing policy |
| Tools | Declared tool permissions and calls reaching control points |
| Resources | Requested budgets, executor support, installed limits |

Control in one dimension does not strengthen another dimension's guarantees. Staging does not prove network isolation; captured requests do not prove that no other connections exist. See [Staging and storage](../reference/cli.md#暂存与存储) for write destinations.

## Plans and actual controls

Admission returns an `ExecutorPlan` with these control plan levels:

- `Unsupported`: executor cannot supply the control.
- `Cooperative`: depends on workload protocol/proxy cooperation.
- `Planned`: intends to install a mandatory mechanism; installation is not yet proven.

Executor teardown returns `ExecutorObservations` with these actual control levels:

- `Unenforced`: mandatory control not confirmed.
- `Cooperative`: control through a cooperative path.
- `Enforced`: installed mechanism supplies a mandatory boundary within its reported scope.

`executor_observations` in the Run Bundle is the enforcement evidence for the current execution; safety summaries are derived from it. Isolation labels, configuration, metadata, or warning strings cannot replace installation receipts. `run.json` stores run facts and executor-selection identity, not enforcement claims. Old Bundles missing the observation contract are rejected.

Execution is rejected when a required control cannot be satisfied. Modes that allow degradation must record the missing controls and warnings. See [CLI reference](../reference/cli.md#安全的第一次运行) for the current limits of `--strict`.

## What records show

```text
请求的权限 → 准入计划 → 安装控制 → 执行与观察 → 终态结果和 Run Bundle
```

`status --review` displays the Run Bundle's controls, warnings and staged changes. File observations cover only operations that reach OverlayFS; network observations cover only traffic that reaches OverlayNet. `null` means unobserved; zero means observed with no hits. Operation counts are not a substitute for the final file diff.

Optional Event Journals record lifecycle and facts actually published by Gateway, linked by identity and causal references. They do not contain the full Bundle's file changes, outputs, artifacts, and control inventory. Event order is not a global ordering of external side effects across Jobs. See [Operation and Event](../design/operations-events.md).

## Guarantee scope

Selective proxies on ordinary host/container execution depend on the client using them; the mechanisms for host deny-all, macOS `--safe` mode, and VM networking are described in [network boundaries](../guides/policies/network.md#网络边界). AgentCtl is a cooperation channel, not isolation evidence. See [isolation design](../design/isolation.md) for platform mechanisms and gaps.

`apply`/`drop` manage staged files only: they cannot undo applied batches, remote APIs, database writes, or messages. Logical checkpoints store staged files and cooperative quiescent points, not process memory or external service state. Local records support review and diagnosis, not cryptographic remote attestation or hostile multi-tenancy guarantees.
