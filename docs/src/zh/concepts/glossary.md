# 术语表

| 术语 | 含义 | 权威页 |
| --- | --- | --- |
| Job | CLI 中一项持久的工作：一次受管理的命令、它的证据和暂存改动 | [Job 与存储](jobs.md) |
| Run | Job 的内部记录；磁盘上的 ID 形如 `run-<uuid>` | [Job 与存储](jobs.md) |
| Attempt | 由某个执行器完成的一次执行 | [执行模型](../design/execution-model.md) |
| Session | 一次 Attempt 的生命周期所有者，负责资源准备、取消、清理与终态公布 | [执行模型](../design/execution-model.md) |
| Operation | pVisor 要处理的操作；当前生产操作是 `run.execute` | [Operation 与 Event](../design/operations-events.md) |
| 执行器（executor） | 提供执行环境与边界的后端：host、container、VM | [选择执行环境](../guides/executors/index.md) |
| 暂存（stage） | 写时复制的工作区上层，apply 前不影响项目 | [暂存与 apply 语义](staging.md) |
| apply | 把选中的暂存改动写入目标工作区，冲突时拒绝 | [暂存与 apply 语义](staging.md) |
| drop | 丢弃尚未 apply 的暂存改动；终态 | [暂存与 apply 语义](staging.md) |
| 逻辑检查点（checkpoint） | 暂存文件系统的快照，不含进程内存 | [逻辑检查点与分叉](../guides/fork-checkpoint.md) |
| fork | 从已停止 Job 的检查点创建带来源关系的新 Job | [逻辑检查点与分叉](../guides/fork-checkpoint.md) |
| Run Bundle | `run-bundle.json`：结果、安全摘要、改动、执行器观察与产物 | [能力、证据与保证边界](capabilities-and-evidence.md) |
| 能力维度（capability） | 文件读取、文件写入、网络、子进程、凭据、模型、工具、资源 | [能力、证据与保证边界](capabilities-and-evidence.md) |
| 准入计划（`ExecutorPlan`） | 执行器在启动前声明能提供什么，最高为 `Planned` | [能力、证据与保证边界](capabilities-and-evidence.md) |
| 执行器观察（`ExecutorObservations`） | 执行器收尾时返回的实际控制，是强制力的唯一证据 | [能力、证据与保证边界](capabilities-and-evidence.md) |
| Enforced / Cooperative / Unenforced | 实际控制的等级：强制、协作、未强制 | [能力、证据与保证边界](capabilities-and-evidence.md) |
| `--safe` | 预设：暂存工作区、默认敏感路径保护、按 Agent 放行模型 API，并要求执行器落实隔离 | [CLI 参考](../reference/cli.md#safe-参数预设) |
| `--strict` | 要求每个请求的能力维度都有不可绕过的强制证据，否则拒绝运行 | [策略模型](policy-model.md) |
| OverlayFS | pVisor 的写时复制文件系统层，执行暂存与文件规则 | [OverlayCore 设计（规划中）](../design/overlayfs.md) |
| OverlayNet | pVisor 的网络策略层：host/container 上是代理，VM 上是 smoltcp 数据面 | [网络策略](../guides/policies/network.md) |
| Gateway | 可选的模型流量网关，负责协议转换、持有上游 Key 与捕获请求 | [Gateway 捕获](../guides/capture.md) |
| AgentCtl | Agent 与 pVisor 之间的协作控制通道；本身不是隔离证据 | [执行模型](../design/execution-model.md) |
| 回放（replay） | 回放一条已有轨迹的工具前缀，再让 Agent 实时续跑 | [回放](../guides/replay.md) |
| 信任阶梯 | L0 到 L3：人介入方式从逐条批准到事后审计的分级 | [信任阶梯](../why/trust-ladder.md) |
