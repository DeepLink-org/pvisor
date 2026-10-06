# Job、Operation、Attempt 与 Event

**Job** 是 CLI 中持久的一项工作。`pvisor run` 创建 Job；`status`、`kill`、
`inspect`、`fork`、`apply`、`drop` 无需额外命令层级即可操作它。当前每个 Job
对应一条内部 Run 记录。为保持兼容，磁盘上的 `run-*` ID、`run.json` 和 Run Bundle
名称保持不变。

[单节点 daemon](daemon/index.md) 通过 VM-only NativeRuntime 管理 OpenSandbox profile 的 `sb-*` sandbox。私有 supervisor 记录把 generation 绑定到原生 Run/Attempt ID；sandbox ID 不是公开 Job ID。API 不实现 stage/apply、checkpoint 或 Run Bundle 导出。它嵌入原生执行，但不自动暴露下列 Job 工作流；跨主机身份属于外部编排。

## Operation：核心处理对象

Job 描述用户的一项工作，Operation 描述 pVisor 要处理的操作。当前生产操作是 `run.execute`，包含程序、参数和工作目录，以及有效策略决定和 Placement。pvisor 负责准入、实际改写、调度和执行；core 提供这些定义。

Event 是外部观察到的事实，Trace 是这些事实的记录。它们描述请求、实际改写、放置及结果，不等于操作本身，也不保证仅凭日志就能重放外部副作用。字段及因果关系见[Operation 与 Event](operations-events.md)。

## Run：Job 的内部记录

Run 标识命令、配置与执行结果，其 ID 与操作系统 PID 无关。进程退出后，本地记录仍让 `status`、`apply` 和 `drop` 指向同一段工作。使用 `status --review` 查看 Run Bundle 与暂存改动。

## Attempt：一次执行

Attempt 标识由某个执行器完成的一次执行。当前 `PVisor::run` 每次调用创建一个 Attempt。pvisor 中的 Session 负责它的资源准备、取消、清理和终态公布；Session 是生命周期所有者。

准入只解析一次驱动网络配置，应用分层策略后，将最终配置传入 Attempt 准备。Gateway、显式代理和 VM 网络消费这份配置，不重新读取原始配置。Guest workspace overlay 要求执行器显式声明支持；执行器名称是描述性记录，不是能力判断。网络不可绕过标记取决于实际 VM 网络 attachment 路径，而不是后端名称。

Fork 根据逻辑检查点创建带有来源关系的新 Run，不会恢复原进程。

## Effect：执行带来的后果

对暂存文件，当前支持的处理流程是：

```text
运行 → 停止 → 审查 → 应用选中文件 → 审查剩余改动 → 应用或丢弃
```

进程成功退出和文件成功应用是两个不同结果。失败命令产生的文件仍可审查；已经应用的文件不会被后续 drop 撤销。

网络请求和外部服务修改也是执行后果，但文件暂存不会把它们延迟到审批后，也不会回滚它们。

## Checkpoint：文件系统快照

默认 `fork --state workspace` 对已停止的暂存 Run 的 upper 层创建快照。嵌入式 API 还支持 AgentCtl 协作静默点。检查点保留暂存文件、首次修改时的冲突基线和来源关系，不保存进程内存、外部服务状态，也不冻结所有 lower 层。快照内容同步落盘后才发布 manifest。嵌入式调用方通过 `restore_logical_checkpoint(checkpoint, destination_upper, destination_preimages)` 同时恢复文件和冲突基线。

原生 VM 的 `checkpoint --kind execution` 则封存 CPU、RAM、设备和文件系统的一致执行点。`suspend` 以原 VM 的终止回执确认暂停，`resume` 保留 Job ID、生成新 Attempt；execution fork 创建新 Job 和独立写入层。暂停不是可以直接 apply/drop 的普通停止态，须先 kill 撤销恢复权。支持的 profile 与兼容性限制见 [CLI 参考](../reference/cli.md#full-vm-execution-checkpoints)。

操作步骤见[审查与应用](../guides/review-apply.md)，执行保证见[能力与证据](../concepts/capabilities-and-evidence.md)。
