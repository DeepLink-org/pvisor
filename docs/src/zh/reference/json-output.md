---
status: todo
search:
  exclude: true
---

# 机器可读输出

!!! warning "规划中"
    完整参考尚未完成；`status --review --json` 的用法示例见[网络策略](../guides/policies/network.md)的"检查运行结果"一节。

## 要回答的问题

`status --json`、`status --review --json` 等命令输出什么结构，脚本和 CI 可以依赖哪些字段？

## 需求

- 为每个支持 `--json` 的命令发布 JSON Schema；
- 标注每个字段的稳定性等级（稳定、实验）；
- 给出常用的 `jq` 查询：是否有被拒绝的访问、网络边界是否不可绕过、改动是否只在指定路径内。

## 验收标准

- Schema 由代码生成，CI 校验输出符合 schema；
- 修复已知问题：`status --json` 的 `sample_paths` 会泄漏内部的 `.wh.d` 路径。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：[Run Bundle 格式（规划中）](run-bundle.md)

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

安全判定需要同时检查 schema、必需字段、执行状态、能力观察与警告；不要用 `// false` 或空数组把未知状态变成通过。字段稳定性与完整 JSON Schema 仍待完成。
