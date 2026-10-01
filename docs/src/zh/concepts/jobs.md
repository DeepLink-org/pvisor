# Job 与存储

Job 是 CLI 中持久的一项工作：一次受管理的命令、执行证据和暂存改动。`pvisor run` 创建 Job；`status`、`kill`、`inspect`、`fork`、`apply`、`drop` 直接操作它，命令保持扁平。内部仍用 Run 记录保存 Job，磁盘上的 `run-*` ID 与 Run Bundle 名称保持不变。

## `last` 的解析规则

`last` 只在**默认存储**中解析当前工作区的 Run。用 `--stage PATH` 时，Job 的存储就是该暂存目录，不在默认存储中，因此请显式传路径或命令输出的 Job ID（`run-*`）：

```bash
pvisor status --review ../stage-001
pvisor apply ../stage-001 --path src
```

找不到当前工作区的 Run 时，`pvisor` 会报错，而不会回退到其他项目的 Job。

!!! note "TODO"
    补充存储位置、保留策略与跨项目并行建议。
    与 reference/cli 的「暂存与存储」去重。

