# Capabilities, evidence, and guarantees

Evaluate requested permissions, admission plans, installed controls, and observed outcomes separately. `--safe`, a stage directory, or an executor name alone does not prove every capability is enforced.

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

One dimension does not establish another. Staging does not prove network isolation; captured requests do not prove no other connections exist. See [storage](../reference/cli.md#暂存与存储) for write destinations.

## Plans and installed controls

Admission returns an `ExecutorPlan` with these levels:

- `Unsupported`: executor cannot supply the control.
- `Cooperative`: depends on workload protocol/proxy cooperation.
- `Planned`: intends to install a mandatory mechanism; installation is not yet proven.

Teardown returns `ExecutorObservations` with:

- `Unenforced`: mandatory control not confirmed.
- `Cooperative`: control through a cooperative path.
- `Enforced`: installed mechanism supplies a mandatory boundary within its reported scope.

`executor_observations` in the Run Bundle is authoritative; safety summaries are derived from it. Isolation labels, configuration, metadata, and warning strings cannot replace installation receipts. `run.json` records identity and facts without enforcement claims. Old Bundles missing the observation contract are rejected.

Missing required controls reject execution. Allowed degradation must record omissions and warnings. See [CLI limits](../reference/cli.md#安全的第一次运行) for `--strict`.

## What records show

```text
请求的权限 → 准入计划 → 安装控制 → 执行与观察 → 终态结果和 Run Bundle
```

`status --review` displays controls, warnings, and staged changes. File observations cover operations reaching OverlayFS; network observations cover traffic reaching OverlayNet. `null` means unobserved; zero means observed with no hits. Operation counts are not the final diff.

Optional Event Journals record lifecycle and facts actually published by Gateway, linked by identity and causal references. They do not contain every Bundle change, output, artifact, or control. Event order is not a global ordering of external effects across Jobs. See [Operation and Event](../design/operations-events.md).

## Scope

Selective proxies on ordinary host/container execution depend on client cooperation. Host deny-all, macOS `--safe` mode, and VM networking are described in [network boundaries](../guides/policies/network.md#网络边界). AgentCtl is cooperation, not isolation evidence. See [isolation design](../design/isolation.md) for platform gaps.

`apply`/`drop` manage staged files only: they cannot undo applied batches, remote APIs, database writes, or messages. Logical checkpoints store staged files and cooperative quiescent points, not memory or external service state. Local records support review and diagnosis, not cryptographic remote attestation or hostile multi-tenancy.
