# Job 用法与存储 {#job-storage}

Job 关联一次受管理的命令、执行证据和暂存改动。CLI `stage` 保留这些状态；
`spawn` 是临时执行，退出后删除 stage。

```bash
pvisor stage --executor host --safe --stage ../stage-001 -- codex
pvisor status --review ../stage-001
```

`pvisor stage` 创建持久 Job；`status`、`kill`、`inspect`、`fork`、`apply`、
`drop` 直接操作它，没有 `job` 子命令。`ps -a` 将已停止的 stage 与容器一同列出，
`rm` 删除实例。ID 沿用 `run-*` 名称。

## 之后怎么再找到它 {#selectors}

后续命令都接受一个 selector：Job ID、暂存目录、`run.json`，或视图内的路径。

`last` 按当前工作区查找已登记的 Job，也包括自定义 stage 存储。
名称和唯一 ID 前缀也可选择实例；名称有歧义时使用 ID。也可以显式给出路径：

```bash
pvisor status --review ../stage-001      # stage path
pvisor status --review run-20260102-abc  # or the Job ID
```

找不到当前工作区的 Job 时，pVisor 会报错，而不是拿别的项目的 Job 顶上。

## 存储位置 {#storage}

| 调用方式 | 存储 |
| --- | --- |
| 默认，包括 `--safe` | `~/.pvisor/runs/run-<uuid>/`；用 `PVISOR_RUN_HOME` 覆盖根目录 |
| 显式 `--stage PATH` | `PATH` 本身 |

默认根目录落在工作区下层时，pVisor 改用系统临时 Run 根，避免可写暂存出现在自己的下层里。目录结构见[项目发现](../reference/cli.md#run-项目发现)。

新的 CLI 根目录用不可变的 `instance.json` 保存身份，将 Attempt 状态放在
`attempts/initial/`。每个 Attempt 拥有自己的 Run 记录、Bundle、完成回执、
lease、`upper/`、`work/`、`merged/`、`preimages/` 与逻辑 checkpoint。
原生 VM 的生命周期元数据位于实例根目录，并选择活动 Attempt；resume 在
`attempts/` 下创建新目录。旧的平铺布局仍可读取，不搬迁已有文件。

## 保留与清理 {#cleanup}

- `spawn` 在确认退出后删除实例存储，包括失败和取消；完成状态不确定时保留恢复证据并报告拒绝清理。
- `stage` 在退出后保留，直到显式执行 `pvisor rm ID`。`drop` 丢弃待接受的候选，但不删除实例。
- 合入全部剩余改动或丢弃是终态：一次性的 `upper`／`work` 数据被删除，而紧凑的 Run／Overlay 元数据、`apply-ledger.json` 与 capture 产物保留。
- safe HOME 与显式 `CODEX_HOME` 使用私有暂存，执行后丢弃；不计入工作区 Run Bundle。
- 文件系统 `fork` 创建独立的原生 CoW inode，删除父实例不会破坏子实例。执行状态分支仍保留独立的快照引用，源实例的删除可能需要先移除这些分支。
