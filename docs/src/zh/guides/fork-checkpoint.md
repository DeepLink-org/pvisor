# 逻辑检查点与分叉

`fork` 要求源 Job 已停止，并在启动子 Job 前创建逻辑文件系统快照：

```bash
pvisor fork last -- codex
```

快照保存文件系统上层和派生关系，不保存进程内存，也不是所有底层宿主文件的不可变副本。嵌入式调用方可配合 AgentCtl 静默协议。

!!! note "TODO"
    补充内嵌 API（RunHandle::checkpoint）用法、恢复语义与限制。
    与 guides/review-apply 去重。

