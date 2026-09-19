# Run、Attempt 与 Effect

## Run：一次受管理的调用

Run 标识命令、配置与执行结果，其 ID 与操作系统 PID 无关。进程退出后，本地记录仍让 `status`、`review`、`apply` 和 `drop` 指向同一段工作。

## Attempt：一次执行

Attempt 标识由某个执行器完成的一次执行。当前 `PVisor::run` 每次调用创建一个 Attempt。契约中保留了 Attempt 和租约身份，但 pVisor 不会自动调度分布式重试。

Fork 根据逻辑检查点创建带有来源关系的新 Run，不会恢复原进程。

## Effect：执行带来的后果

对暂存文件，当前支持的处理流程是：

```text
运行 → 停止 → 审查 → 应用选中文件 → 审查剩余改动 → 应用或丢弃
```

进程成功退出和文件成功应用是两个不同结果。失败命令产生的文件仍可审查；已经应用的文件不会被后续 drop 撤销。

网络请求和外部服务修改也是执行后果，但文件暂存不会把它们延迟到审批后，也不会回滚它们。

## Checkpoint：文件系统快照

CLI 对已停止的暂存 Run 的 upper 层创建快照。嵌入式 API 还支持 AgentCtl 协作静默点。检查点保留暂存文件和来源关系，不保存进程内存、外部服务状态，也不冻结所有 lower 层。

操作步骤见[审查与应用](../guides/review-apply.md)，执行保证见[能力与证据](capabilities-and-evidence.md)。
