# Job 与存储

**Job** 是 CLI 中一项持久的工作：一次受管理的命令、它的执行证据和暂存改动。`pvisor run` 创建 Job；`status`、`kill`、`inspect`、`fork`、`apply`、`drop` 直接操作它。Job 在进程退出后仍然存在，所以你可以先让 Agent 跑完，再慢慢审查。

内部每个 Job 对应一条 Run 记录，磁盘上的 ID 形如 `run-<uuid>`，结果保存在 Run Bundle（`run-bundle.json`）中。Job、Run、Attempt 的实现关系见[执行模型](../design/execution-model.md)。

## Job 存在哪里

| 运行方式 | Job 存储位置 |
| --- | --- |
| 默认（包括 `--safe`、`--ask`） | `~/.pvisor/runs/run-<uuid>/`；可用 `PVISOR_RUN_HOME` 改变根目录 |
| 显式 `--stage PATH` | 指定的 `PATH` 本身 |

如果默认根目录落在所选工作区底层之内，pVisor 改用系统临时目录下的 Run 根，避免可写 stage 出现在自己的底层里。目录结构见 [Run 项目发现](../reference/cli.md#run-项目发现)。

## 如何指定一个 Job

生命周期命令接受以下任意一种：

- Job ID：`run-<uuid>`；
- Job 存储目录或暂存目录：例如 `../stage-001`；
- Job 目录中的 `run.json`、`upper` 或 `merged` 路径；
- 项目工作区路径：选择该工作区最新的 Job；
- `last`，或者不传选择器：选择**当前工作区**最新的 Job。

## `last` 的解析规则

`last` 只在默认存储中查找属于当前工作区的 Job。`--stage PATH` 创建的 Job 存储在暂存目录中，不在默认存储里，因此请显式传入路径或 Job ID：

```bash
pvisor run --safe --stage ../stage-001 -- codex
pvisor status --review ../stage-001
pvisor apply ../stage-001 --path src
```

找不到当前工作区的 Job 时，pVisor 会报错，不会回退到其他项目的 Job。多个项目并行使用时，这一点保证 `apply` 不会作用到别的项目。

## 保留与清理

- Job 结束后，记录和暂存改动默认保留，直到你 apply 全部改动或 drop；
- apply 全部剩余改动或 drop 是终态：pVisor 删除 `upper`、`work` 等一次性暂存数据，但保留紧凑的 Run/Overlay 元数据、`apply-ledger.json` 和捕获产物；
- `--safe` 下 HOME（以及显式设置的 `CODEX_HOME`）使用独立的私有 stage，Run 结束后丢弃，不进入 Run Bundle。
