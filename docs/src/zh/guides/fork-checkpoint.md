# 逻辑检查点与分叉

在全部应用或丢弃暂存内容之前，可以从当前文件系统状态分叉出一个新 Job，让 Agent 换一种方式继续：

```bash
pvisor fork last -- codex
```

## 行为

- `fork` 要求源 Job 已停止，先为它的暂存文件系统创建逻辑检查点，再启动子 Job；
- 子 Job 记录与源 Job 的来源关系，源 Job 的暂存改动不受影响，仍可单独审查、应用或丢弃；
- 传入 `--checkpoint ID` 可复用已有的逻辑检查点；
- 省略 `--` 之后的命令时，子 Job 沿用源 Job 的命令。

## 检查点保存什么

| 保存 | 不保存 |
| --- | --- |
| 暂存工作区的上层（upper） | 进程内存与运行状态 |
| 首次修改时记录的冲突基线 | 外部服务状态、已发生的网络调用 |
| 与源 Job 的来源关系 | 所有底层宿主文件的不可变副本 |

因此 fork 是"从同一份文件改动重新开始"，不是进程级的快照恢复。快照内容同步落盘后才发布 manifest。

## 嵌入式 API

嵌入 pVisor 的宿主可以调用 `RunHandle::checkpoint`：pVisor 通过 AgentCtl 发布 quiesce 指令，要求每个被纳入检查点的 Session 报告匹配的 quiesced 状态，快照 upper 后再发布 `continue`。这让协作的客户端可以在安全点上冻结文件状态，但不会让任意子进程变成可恢复的进程。恢复使用 `restore_logical_checkpoint(checkpoint, destination_upper, destination_preimages)`，同时恢复文件和冲突基线。机制见[执行模型](../design/execution-model.md)。
