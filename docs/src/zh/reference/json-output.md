---
status: todo
search:
  exclude: true
---

# 机器可读输出

!!! warning "规划中"
    本页尚无完整参考。`status --review --json` 的用法示例见[网络策略](../guides/policies/network.md)的"检查运行结果"一节。

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
