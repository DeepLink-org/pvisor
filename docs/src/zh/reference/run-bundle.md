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

## 字段类型与出现规则 {#fields}

`必需` 表示 Rust 读取器要求提供该字段；`可省略，有默认值` 表示 serde 接受字段缺失，不表示决策时可以把缺失当成观察到的空结果。`空时省略` 表示 writer 省略的可选值或空集合。文档构建会对照源码检查下方字段名、类型与覆盖范围。这是字段参考，不是涵盖所有嵌套协议的完整生成式 JSON Schema。

<!-- bundle-fields:start -->

| JSON 路径 | Rust 类型 | 出现规则 | 含义 |
| --- | --- | --- | --- |
| `schema_version` | `u32` | 必需 | 当前读取器要求值为 4 |
| `generated_at_unix_ms` | `u64` | 必需 | Bundle 生成时间，Unix 毫秒 |
| `run` | `BundleRun` | 必需 | 执行结果与身份 |
| `lineage` | `Option<RunLineage>` | 空时省略 | fork 时的父 Run 与 checkpoint 身份 |
| `executor_observations` | `ExecutorObservations` | 必需 | 执行器回执；实际安装控制的权威来源 |
| `executor_plan` | `Option<ExecutorPlan>` | 空时省略 | 准入计划，不是强制控制回执 |
| `safety` | `SafetySummary` | 必需 | 从回执与暂存状态派生的布尔值 |
| `filesystem` | `Option<FilesystemSummary>` | 空时省略 | 暂存文件系统；直接写入任务省略 |
| `network` | `NetworkSummary` | 必需 | 基础策略与可用的拦截证据 |
| `environment` | `crate::runtime::EnvironmentProjection` | 可省略，有默认值 | 仅记录变量名；字段见下方 |
| `resources` | `ResourceSummary` | 可省略，有默认值 | 请求/有效额度与实现说明 |
| `agentctl` | `AgentCtlSnapshot` | 必需 | 协作客户端与 directive 快照 |
| `orchestration` | `std::collections::BTreeMap<String, serde_json::Value>` | 空时省略 | 应用特定元数据；空时省略 |
| `operation` | `Option<pvisor_core::operation::Operation>` | 空时省略 | 实际生效的 Operation 契约 |
| `run_observation` | `Option<pvisor_core::operation::OperationObservation>` | 空时省略 | Operation 结果与访问观察 |
| `artifacts` | `Vec<BundleArtifact>` | 可省略，有默认值 | 本地文件引用，不包含文件内容 |

| JSON 路径 | Rust 类型 | 出现规则 | 含义 |
| --- | --- | --- | --- |
| `run.run_id` | `String` | 必需 | Run 身份 |
| `run.parent_run_id` | `Option<String>` | 空时省略 | 提供时的父 Run 身份 |
| `run.task_id` | `Option<String>` | 空时省略 | 提供时的 Task 身份 |
| `run.attempt_id` | `String` | 必需 | Attempt 身份 |
| `run.session_id` | `String` | 必需 | Session 身份 |
| `run.agent` | `String` | 必需 | Agent 标签 |
| `run.command` | `Vec<String>` | 必需 | 实际命令参数数组 |
| `run.executor` | `Option<ExecutorIdentity>` | 空时省略 | 所选执行器身份；不是安装证据 |
| `run.state` | `RunState` | 必需 | 执行状态；completed、failed、cancelled 为终态 |
| `run.started_at_unix_ms` | `u64` | 必需 | 执行开始时间，Unix 毫秒 |
| `run.finished_at_unix_ms` | `u64` | 必需 | 执行结束时间，Unix 毫秒 |
| `run.duration_ms` | `u64` | 必需 | 执行时长，毫秒 |
| `run.exit_code` | `Option<i32>` | 空时省略 | 有结果时的任务退出码 |
| `run.failure` | `Option<RunFailure>` | 空时省略 | 有失败时的分类执行错误 |
| `run.warnings` | `Vec<String>` | 可省略，有默认值 | 执行警告；默认空 |
| `run.output` | `ProcessOutput` | 可省略，有默认值 | 捕获的 stdout/stderr 与截断标志；默认空 |
| `run.metrics` | `std::collections::BTreeMap<String, f64>` | 可省略，有默认值 | 有名称的数值执行指标；默认空 |
| `run.result_artifacts` | `Vec<ArtifactRef>` | 可省略，有默认值 | 执行器结果引用；默认空 |
| `run.event_stream_ref` | `Option<String>` | 空时省略 | 提供时的 Trace Event 流引用 |

| JSON 路径 | Rust 类型 | 出现规则 | 含义 |
| --- | --- | --- | --- |
| `safety.safe_profile_requested` | `bool` | 必需 | 请求的 profile，不证明安装成功 |
| `safety.host_process` | `bool` | 必需 | 未隔离的 host process 身份 |
| `safety.filesystem_changes_staged` | `bool` | 必需 | 当前保留了暂存变更 |
| `safety.filesystem_non_bypassable` | `bool` | 必需 | 读与写维度都被强制执行 |
| `safety.filesystem_read_non_bypassable` | `bool` | 可省略，有默认值 | 读取维度被强制执行；serde 默认 false |
| `safety.filesystem_write_non_bypassable` | `bool` | 可省略，有默认值 | 写入维度被强制执行；serde 默认 false |
| `safety.network_non_bypassable` | `bool` | 必需 | 网络维度被强制执行 |
| `safety.warnings` | `Vec<String>` | 可省略，有默认值 | 边界限制说明；默认空 |

| JSON 路径 | Rust 类型 | 出现规则 | 含义 |
| --- | --- | --- | --- |
| `filesystem.state` | `OverlayState` | 必需 | active、staged、applied、discarded；独立于执行状态 |
| `filesystem.target` | `PathBuf` | 必需 | 宿主 apply 目标 |
| `filesystem.upper` | `PathBuf` | 必需 | 可写 Stage backing 路径 |
| `filesystem.changed_files` | `usize` | 必需 | 变更路径数量，不是文件系统调用次数 |
| `filesystem.whiteouts` | `usize` | 必需 | 删除/不透明目录表示的数量 |
| `filesystem.root_overlay` | `bool` | 可省略，有默认值 | 目标是否为根 `/`；默认 false |
| `filesystem.excluded_paths` | `Vec<PathBuf>` | 可省略，有默认值 | 根 overlay 排除的路径；默认空 |
| `filesystem.access_policy` | `pvisor_core::overlay::FileAccessPolicy` | 可省略，有默认值 | 已解析文件策略；默认空 |
| `filesystem.host_root_device` | `Option<u64>` | 空时省略 | root overlay 原宿主根目录设备身份 |
| `filesystem.host_root_inode` | `Option<u64>` | 空时省略 | root overlay 原宿主根目录 inode 身份 |
| `filesystem.host_uid` | `Option<u32>` | 空时省略 | root overlay 映射的宿主用户 ID |
| `filesystem.host_gid` | `Option<u32>` | 空时省略 | root overlay 映射的宿主组 ID |
| `filesystem.sample_paths` | `Vec<String>` | 可省略，有默认值 | 展示样本，不是完整变更清单；默认空 |
| `filesystem.changes` | `Vec<ChangeEntry>` | 可省略，有默认值 | 已分类的净变更；默认空 |

| JSON 路径 | Rust 类型 | 出现规则 | 含义 |
| --- | --- | --- | --- |
| `network.policy` | `serde_json::Value` | 必需 | 序列化的基础网络策略 |
| `network.interception` | `Option<InterceptionProfile>` | 空时省略 | 提供时的驱动/profile 身份 |
| `network.intercepted` | `Option<InterceptionSnapshot>` | 空时省略 | 提供时的驱动最终计数；省略不等于零 |

| JSON 路径 | Rust 类型 | 出现规则 | 含义 |
| --- | --- | --- | --- |
| `resources.requested` | `ResourceLimits` | 必需 | 请求的 ResourceLimits；可选维度使用字节/毫秒/数量 |
| `resources.effective` | `ResourceLimits` | 必需 | 观察支持的有效 ResourceLimits |
| `resources.mechanisms` | `Vec<String>` | 可省略，有默认值 | 已安装的资源机制；默认空 |
| `resources.limitations` | `Vec<String>` | 可省略，有默认值 | 未强制或平台特定限制；默认空 |

| JSON 路径 | Rust 类型 | 出现规则 | 含义 |
| --- | --- | --- | --- |
| `artifacts[].kind` | `String` | 必需 | 产物用途，例如 run-record |
| `artifacts[].path` | `PathBuf` | 必需 | 本地引用；需单独解析与保留 |

| JSON 路径 | Rust 类型 | 出现规则 | 含义 |
| --- | --- | --- | --- |
| `environment.inherits_host` | `bool` | 可省略，有默认值 | 是否继承宿主环境；默认 false |
| `environment.projected_keys` | `Vec<String>` | 可省略，有默认值 | 传入任务的宿主变量名；默认空 |
| `environment.runtime_injected_keys` | `Vec<String>` | 可省略，有默认值 | 运行时注入的变量名；默认空 |

| JSON 路径 | Rust 类型 | 出现规则 | 含义 |
| --- | --- | --- | --- |
| `lineage.parent_run_id` | `String` | 必需 | 父 Job 身份 |
| `lineage.checkpoint_id` | `String` | 必需 | fork 使用的 checkpoint |

| JSON 路径 | Rust 类型 | 出现规则 | 含义 |
| --- | --- | --- | --- |
| `filesystem.changes[].path` | `String` | 必需 | 展示路径；可能另有字节身份 |
| `filesystem.changes[].path_bytes` | `Option<Vec<u8>>` | 空时省略 | 展示无法表示身份时提供无损 Unix 字节 |
| `filesystem.changes[].kind` | `ChangeKind` | 必需 | added、modified、deleted、type_changed、opaque |
| `filesystem.changes[].old_type` | `Option<ChangeEntryType>` | 空时省略 | 原类型：file、directory、symlink、other |
| `filesystem.changes[].new_type` | `Option<ChangeEntryType>` | 空时省略 | 新类型：file、directory、symlink、other |
| `filesystem.changes[].size_bytes` | `Option<u64>` | 空时省略 | 适用时的当前大小，字节 |
| `filesystem.changes[].mode` | `Option<u32>` | 空时省略 | 适用时的 Unix 数值 mode，例如 420 表示 0644 |
<!-- bundle-fields:end -->

## 区分净变更与访问操作 {#changes}

[真实样例](../assets/examples/json/run-bundle.json) 中，`obsolete.txt` 为 `deleted`，`src` 是新增目录，`src/result.txt` 是新增文件。`filesystem.changes` 描述暂存的净结果；`run_observation.filesystem` 描述实际访问、允许/拒绝决定及计数。一个文件被反复写入，变更清单里仍可能只出现一次；被拒绝的读取可以出现在观察中而不产生文件变更。

`path` 用于展示。如果存在 `path_bytes`，路径身份应使用这组 Unix 字节，不要只按展示文本授权修改。目录变更与 `opaque` 条目有子树影响，选择 apply 路径时需一并考虑。使用 CLI 的选择性 apply，不要把 upper 层的 whiteout 原样复制进项目。

已保存的 `run-bundle.json` 描述采集时的执行结果。`review --json` 额外提供 `review_context` 并刷新所选文件视图，不会重新生成执行器观察。见 [JSON 响应结构与样例](json-output.md#envelopes)。
