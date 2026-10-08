# Job 与存储

Job 是 pVisor 里一项持久的工作：一次受管理的命令、它的执行证据和暂存改动。

```bash
pvisor run --safe --stage ../stage-001 -- codex
pvisor status --review ../stage-001
```

`pvisor run` 创建 Job；`status`、`kill`、`inspect`、`fork`、`apply`、`drop` 都直接操作它，没有 `job` 子命令。磁盘上沿用 `run-*` 和 Run Bundle 的名字。

## 之后怎么再找到它

后续命令都接受一个 selector：Job ID、暂存目录、`run.json`，或视图内的路径。

`last` 是便利写法，只在**默认存储**里按当前工作区查找。用 `--stage PATH` 时 Job 存在暂存目录中，不在默认存储里，这时显式给出路径或 ID：

```bash
pvisor status --review ../stage-001      # 推荐：用暂存路径
pvisor status --review run-20260102-abc  # 或者用 Job ID
```

找不到当前工作区的 Job 时，pVisor 会报错，而不是拿别的项目的 Job 顶上。

## 存储位置

| 调用方式 | 存储 |
| --- | --- |
| 默认，包括 `--safe` 与 `--ask` | `~/.pvisor/runs/run-<uuid>/`；用 `PVISOR_RUN_HOME` 覆盖根目录 |
| 显式 `--stage PATH` | `PATH` 本身 |

默认根目录落在工作区下层时，pVisor 改用系统临时 Run 根，避免可写暂存出现在自己的下层里。目录结构见[项目发现](../reference/cli.md#run-项目发现)。

## 保留与清理

- 记录与暂存改动在执行后保留，直到全部合入或丢弃。
- 合入全部剩余改动或丢弃是终态：一次性的 `upper`／`work` 数据被删除，而紧凑的 Run／Overlay 元数据、`apply-ledger.json` 与 capture 产物保留。
- safe HOME 与显式 `CODEX_HOME` 使用私有暂存，执行后丢弃；不计入工作区 Run Bundle。
