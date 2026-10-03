# Run Bundle 格式

Run Bundle 是一次任务的交接记录：执行了什么、如何结束、留下哪些改动，以及哪些控制实际生效。评审、CI 和批量任务都可以读取同一份记录。

先用 `pvisor status --review STAGE` 看摘要；自动化使用 `--json`，按 `schema_version` 选择读取逻辑。完整文件位于任务输出提示的 `run-bundle.json`。

## schema 4 的顶层记录

| 字段 | 来源与用途 |
| --- | --- |
| `schema_version` / `generated_at_unix_ms` | 格式版本与生成时间；当前版本 4 |
| `run` | Run/Attempt/Session 身份、命令、终态、时间、退出码、失败、输出、指标和结果产物 |
| `executor_plan` | 可选准入计划；不能作为控制已安装的证明 |
| `executor_observations` | 必需的实际执行器观察；强制力证据唯一来源 |
| `safety` | 从实际观察派生的文件/网络边界布尔值与警告 |
| `filesystem` | 有暂存时的目标、upper、终态、净改动清单、删除与样例路径；没有暂存时可省略 |
| `network` | 策略、可选拦截 profile 与可选最终计数 |
| `environment` | 是否继承宿主、投影/注入的变量名；不记录变量值 |
| `resources` | 请求额度、有效额度、机制与限制 |
| `agentctl` | 协作通道快照；不是控制安装回执 |
| `lineage` / `orchestration` | 可选分叉来源及编排元数据 |
| `operation` / `run_observation` | 可选的有效操作与结果/规则/边界观察 |
| `artifacts` | 捕获等本地文件引用；引用不等于文件内容被嵌入 |

`run.state` 的执行终态为 `completed`、`failed` 或 `cancelled`；它与 `filesystem.state` 的应用生命周期不同。执行成功不代表文件已 apply。

## 读取与分享

```bash
pvisor status --review --json ../stage-001 > ../bundle-001.json
jq '{schema_version, run: {id: .run.run_id, state: .run.state}, safety, resources}' ../bundle-001.json
```

Bundle 以 `0600` 写入。当前读取器要求 schema 恰为 4；不把旧格式补成看似完整的证据。未知版本或缺少必需的观察契约会失败，不能把缺失字段解释为安全。

命令参数、路径、stdout/stderr、捕获载荷仍可能含秘密；变量值未记录不代表整个 Bundle 已脱敏。分享前检查 Bundle 和它引用的产物。源码类型见 `crates/pvisor/src/runtime/bundle.rs`；目录与清理规则见 [Job 与存储](../concepts/jobs.md)。
