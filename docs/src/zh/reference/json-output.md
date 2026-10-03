# 机器可读输出

用 JSON 把任务接入脚本：先判断运行状态，再检查实际边界，最后选择接受哪些文件。下面的查询可以作为 CI 判断的起点。

`status --json` 适合看任务状态；`status --review --json` 返回用于评审的完整 Run Bundle。做准入判断时使用后者，并核对[证据字段](run-bundle.md)。

## 选择合适的输出

| 命令 | 输出对象 | 适合 |
| --- | --- | --- |
| `status --json` | 状态与可选文件系统/网络摘要 | 查看存活与暂存概况；不是完整控制证据 |
| `status --review --json` | 完整、带 schema 版本的 Run Bundle | 结束后的审查与机器消费 |
| `review --json` | schema-4 Bundle 加 `review_context` | 同一审查入口；`--checkpoint ID` 选择保存的工作区 |
| `kill --json` | schema 1，operation 为 `kill` | 区分终止请求与已经停止的 Job |
| `checkpoint create/list/show/delete/gc --json` | schema 1，operation 为 `checkpoint.*` | Job 内的 workspace checkpoint 管理 |
| `extensions` | 已安装伴随命令的 JSON 数组 | 检查 TUI/replay/cache 可用性 |

`--review --json` 与 `--diff` 互斥。机器处理时不要解析人读审查文本。

## 常用查询

```bash
pvisor status --json ../stage-001
pvisor status --review --json ../stage-001 > ../review-001.json
jq '.safety.network_non_bypassable' ../review-001.json
jq '.filesystem.changes // [] | map({path, kind})' ../review-001.json
jq '.network.intercepted // null' ../review-001.json
jq '[.filesystem.changes[]? | select(.path != "src" and (.path | startswith("src/") | not))]' ../review-001.json
```

最后一条列出 `src` 之外的改动；空数组只证明保留的改动清单在所选范围内，不证明命令没有外部副作用。

网络计数的字段包括 `requests_seen`、`policy_allowed`、`policy_denied`、`failures`、`tcp_flows_opened`、`tcp_flows_denied` 与 `targets`。没有 `intercepted` 是未提供计数，不是“零流量”。文件访问拒绝需看 `run_observation.filesystem`；净改动需看 `filesystem.changes`，两者不能混用。

安全判定需要同时检查 schema、必需字段、执行状态、能力观察与警告；不要用 `// false` 或空数组把未知状态变成通过。读取器应识别格式版本，并容忍与所需判断无关的额外字段。

## 一个可用于脚本的判断 {#gate}

如果任务要求成功结束、文件访问与网络都受到强制控制，并且只修改 `src/`，可以用下面的检查。示例依赖 `jq`，返回零表示匹配要求，非零表示检查未通过。

```bash
jq -e '
  .schema_version == 4
  and .run.state == "completed"
  and .run.exit_code == 0
  and .safety.filesystem_read_non_bypassable == true
  and .safety.filesystem_write_non_bypassable == true
  and .safety.network_non_bypassable == true
  and (.filesystem.changes | type == "array")
  and all(.filesystem.changes[]; .path_bytes == null and (.path == "src" or (.path | startswith("src/"))))
' ../review-001.json
```

按任务实际要求选择条件。比如协作代理任务不会满足强制网络这一项；需要这项边界时选择对应执行路径。检查通过后仍要运行项目测试和选择 apply 的文件，这个查询只处理上述记录字段。

`schema_version` 未识别时停止读取该版本；必需字段缺失时停止判断。网络统计 `null` 表示没有提供观察值，`0` 表示记录到的计数为零。展示改动列表可以把缺失值显示为空，但准入检查应像示例一样先验证字段类型。

## JSON 版本与命令响应结构 {#envelopes}

schema 数字属于输出格式，而非整个 CLI。`status --json` 没有顶层 schema 版本，它是概况输出；不要在其中寻找 `schema_version = 4`。其字段如下：

| 字段 | 含义 |
| --- | --- |
| `run` | 已存储的 RunRecord，包含生命周期与存储引用 |
| `live` | 布尔值，存活观察 |
| `checkpoint_capability` | `workspace`、`workspace_capture_requires`、`execution`、`execution_blocker` |
| `checkpoints` | workspace checkpoint 记录数组 |
| `workspace_generation` | 整数；没有 overlay 时为 null |
| `apply_history` | 之前的 apply 事务记录 |
| `observations.filesystem/network` | 可用的文件访问/流量观察，或 null |
| `filesystem` | state、changed_files、whiteouts、sample_paths；没有暂存时为 null |

审查输出在 Bundle 上增加 `review_context`：`job_id`、`attempt_id`、可为 null 的 `checkpoint_id`、`workspace_generation`、`file_view`、`execution_evidence`。执行证据保留历史结果；所选文件视图会在审查时刷新，包括 apply/drop 之后。审查 checkpoint 选择它保存的文件，不会重跑任务。

checkpoint 的成功响应都包含 `schema_version = 1`、`operation`、`job_id`：

| Operation | 额外字段 |
| --- | --- |
| `checkpoint.create` | `request_id`（可为 null）、`checkpoint_id`、`kind = "workspace"`、`reused`、`checkpoint` |
| `checkpoint.list` | `kind_filter`（可为 null）、`supported_kinds = ["workspace"]`、`checkpoints` |
| `checkpoint.show` | `kind = "workspace"`、`branch_references`、`checkpoint` |
| `checkpoint.delete` | `checkpoint_id`、`deleted = true` |
| `checkpoint.gc` | `scope = "job_workspace_transactions"`、`root`、`removed_transactions`、`shared_content_store = null` |

`kill --json` 对已经停止的 Job 返回 `already_stopped = true` 与已记录的 state；其他情况下返回 `state = "stopping"`、`termination_requested = true`，只确认终止请求，不表示已经退出。轮询 status 获取后续状态。

`extensions` 的条目包含 `name`、`description`、可执行程序的 `path`。replay 等伴随命令各自拥有自己的格式。普通 `run`、`apply`、`drop` 不输出 JSON 成功响应；读取 Bundle/status 或保留退出码。普通任务的 execution checkpoint 和 `suspend` 即使指定 `--json` 也仍返回能力不支持错误；错误写到 stderr，不会生成 checkpoint 成功对象。

## 可下载的真实任务输出 {#samples}

以下输出使用 Linux host 执行器、safe 暂存和 deny-all 网络采集。任务创建 `src/result.txt` 并删除 `obsolete.txt`。路径、标识与 wall-clock 字段已规范化；时长和机制只描述这一次执行，不代表整个平台的性能。

- [Review Bundle JSON](../../assets/examples/json/run-bundle.json)
- [Status JSON](../../assets/examples/json/status.json)
- [Checkpoint list JSON](../../assets/examples/json/checkpoint-list.json)
- [对已停止 Job 的 kill JSON](../../assets/examples/json/kill-stopped.json)
- [采集来源](../../assets/examples/json/provenance.json)

上面的 `src/` 检查会拒绝这个样例，因为它还删除了 `obsolete.txt`。命令成功且边界受到强制控制，也不能单独授权所有文件变更。非 UTF-8 路径的身份由 `path_bytes` 保留；这个按文本路径判断的检查拒绝此类条目，留给能处理字节路径的审查器。

贡献者构建 CLI 后可在仓库根目录重新采集。脚本建立独立的临时工作区与 Stage，不执行网络任务：

```bash
cargo build --locked -p pvisor --bin pvisor
python3 scripts/record-doc-json.py --binary target/debug/pvisor
```
