# 机器可读输出

用 JSON 把任务接入脚本：先判断运行状态，再检查实际边界，最后选择接受哪些文件。下面的查询可以作为 CI 判断的起点。

`status --json` 适合看任务状态；`status --review --json` 返回用于评审的完整 Run Bundle。做准入判断时使用后者，并核对[证据字段](run-bundle.md)。

## 选择合适的输出

| 命令 | 输出对象 | 适合 |
| --- | --- | --- |
| `status --json` | 状态与可选文件系统/网络摘要 | 查看存活与暂存概况；不是完整控制证据 |
| `status --review --json` | 完整、带 schema 版本的 Run Bundle | 结束后的审查与机器消费 |
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
  and all(.filesystem.changes[]; .path == "src" or (.path | startswith("src/")))
' ../review-001.json
```

按任务实际要求选择条件。比如协作代理任务不会满足强制网络这一项；需要这项边界时选择对应执行路径。检查通过后仍要运行项目测试和选择 apply 的文件，这个查询只处理上述记录字段。

`schema_version` 未识别时停止读取该版本；必需字段缺失时停止判断。网络统计 `null` 表示没有提供观察值，`0` 表示记录到的计数为零。展示改动列表可以把缺失值显示为空，但准入检查应像示例一样先验证字段类型。
