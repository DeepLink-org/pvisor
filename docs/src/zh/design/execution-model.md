# Job、Operation、Attempt 与 Event

Job 表示用户的一项持久工作，Run 保存它的内部记录，Attempt 标识一次实际执行，Event 记录处理过程中发生的事实。一次执行结束后，记录和待接受的文件仍有自己的生命周期。

命令如何创建和查找 Job，见[Job 与存储](../concepts/jobs.md)。当前每个 Job 对应一条内部 Run 记录；为保持兼容，磁盘上的 `run-*` ID、`run.json` 和 Run Bundle 名称保持不变。

[单节点 daemon](daemon/index.md) 通过 VM-only NativeRuntime 管理 OpenSandbox profile 的 `sb-*` sandbox。私有 supervisor 记录把 generation 绑定到原生 Run/Attempt ID；sandbox ID 不是公开 Job ID。API 不实现 stage/apply、checkpoint 或 Run Bundle 导出。它嵌入原生执行，但不自动暴露下列 Job 工作流；跨主机身份属于外部编排。

![Job 的执行完成与文件结果接受分别发布](assets/job-lifecycle.svg)

## Operation：核心处理对象

Job 描述用户的一项工作，Operation 描述 pVisor 要处理的操作。当前生产操作是 `run.execute`，包含程序、参数和工作目录，以及有效策略决定和 Placement。pvisor 负责准入、实际改写、调度和执行；core 提供这些定义。

Event 是外部观察到的事实，Trace 是这些事实的记录。它们描述请求、实际改写、放置及结果，不等于操作本身，也不保证仅凭日志就能重放外部副作用。字段及因果关系见[Operation 与 Event](operations-events.md)。

## Run：Job 的内部记录

Run 标识命令、配置与执行结果，其 ID 与操作系统 PID 无关。进程退出后，本地记录仍让 `status`、`apply` 和 `drop` 指向同一段工作。使用 `status --review` 查看 Run Bundle 与暂存改动。

## Attempt：一次执行

Attempt 标识由某个执行器完成的一次执行。当前 `PVisor::run` 每次调用创建一个 Attempt。pvisor 中的 Session 负责它的资源准备、取消、清理和终态公布；Session 是生命周期所有者。

准入只解析一次驱动网络配置，应用分层策略后，将最终配置传入 Attempt 准备。Gateway、显式代理和 VM 网络消费这份配置，不重新读取原始配置。Guest workspace overlay 要求执行器显式声明支持；执行器名称是描述性记录，不是能力判断。网络不可绕过标记由实际 VM 网络 attachment 路径决定，后端名称不作为判断依据。

Workspace fork 从逻辑检查点创建文件分支；execution fork 从支持的机器检查点恢复新 Job。两者保存来源关系，具体恢复范围由检查点种类决定。

持久 Job 服务还负责完成结果的发布。`ManagedJobRun` 将实际 Attempt join 与 `Server::finish` 交给 runtime task，前端只持有等待适配。前端等待失败或被丢弃会请求取消，已接受的完成任务在 Tokio runtime 仍存活时继续核对和发布；runtime 关闭与宿主进程退出仍有自己的恢复边界。

## Attempt 完成协议 {#attempt-completion}

保留存储的 Attempt 使用 `completion.json` 确认执行结果。回执包含 `RunResult` 和已保存 Bundle 字节的 BLAKE3 摘要，并校验 Run／Attempt 身份、终态、退出码及起止时间。Run 和 execution Job 的状态是这份结果的投影；单独看到终态字段、Bundle 文件或进程退出，都不足以确认完成。

正常收尾由 Session 串行推进：

1. 停止受管理的写入者，清理附件并核对观察；暂存文件按 stage 合同同步和 seal。
2. 保存 Bundle，将收尾阶段已知的结果、观察和警告固定下来。
3. 保存 Run 状态投影，提交绑定该结果与 Bundle 的完成回执。
4. 发布 `Completed` 和 `run.completed`、`run.failed`、`run.cancelled` 或 `run.hibernated` 终态事件；持久 Job 服务在 Attempt join 后完成 Job 状态发布。

回执提交后，终态事件写入失败作为审计缺口通过运行返回和日志报告。该失败不改写已绑定的 Bundle，也不补发一个相反终态。执行结果与事件是否完整分别核对；没有终态事件，仍可能已有有效完成回执。

owner 退出且 Attempt 不再被持有时，读取与恢复检查回执。回执有效则按其结果核对 Run 和仍指向该 Attempt 的 execution Job；已经推进到后续 Attempt 或撤销恢复权的 Job 不被旧结果覆盖。缺少回执时保留 stage 并显示 Unknown；`review` 不提供未确认的执行结果，`apply`、`drop` 和需要已停止状态的捕获拒绝执行。损坏、摘要不符或身份不匹配返回错误，不能改写记录来制造成功。

回执把 Attempt 的结果与 Bundle 绑定，不覆盖 Journal、apply ledger、快照发布及外部服务的一笔总事务。内存运行没有这份磁盘回执。旧记录的读取规则见[记录与版本矩阵](records-and-versions.md#execution-records)，响应丢失后的处理见[失败语义](failure-semantics.md#reconciliation)。

源码入口：`pvisor/src/session/lifecycle.rs`、`runtime/attempt.rs`、`runtime/completion.rs`、`runtime/registry.rs` 和 `runtime/job_service/views.rs`。`completion.rs` 的测试覆盖重复提交、身份与摘要校验、缺失回执和状态投影；平台与进程故障覆盖须单独验收。

## Effect：执行带来的后果

对暂存文件，当前支持的处理流程是：

```text
运行 → 停止 → 审查 → 应用选中文件 → 审查剩余改动 → 应用或丢弃
```

进程成功退出和文件成功应用是两个不同结果。失败命令产生的文件仍可审查；已经应用的文件不会被后续 drop 撤销。

网络请求和外部服务修改也是执行后果，但文件暂存不会把它们延迟到审批后，也不会回滚它们。

## Checkpoint：文件系统快照

默认 `fork --state workspace` 对已停止的暂存 Run 的 upper 层创建快照。嵌入式 API 还支持 AgentCtl 协作静默点。检查点保留暂存文件、首次修改时的冲突基线和来源关系，不保存进程内存、外部服务状态，也不冻结所有 lower 层。快照内容同步落盘后才发布 manifest。嵌入式调用方通过 `restore_logical_checkpoint(checkpoint, destination_upper, destination_preimages)` 同时恢复文件和冲突基线。

原生 VM 的 `checkpoint --kind execution` 则封存 CPU、RAM、设备和文件系统的一致执行点。`suspend` 以原 VM 的终止回执确认暂停，`resume` 保留 Job ID、生成新 Attempt；execution fork 创建新 Job 和独立写入层。暂停状态下 apply/drop 不可用，须先 kill 撤销恢复权。支持的 profile 与兼容性限制见 [CLI 参考](../reference/cli.md#full-vm-execution-checkpoints)。

操作步骤见[审查与应用](../guides/review-apply.md)，执行保证见[能力与证据](../concepts/capabilities-and-evidence.md)。
