---
status: todo
search:
  exclude: true
---

# 策略字段参考

!!! warning "规划中"
    本页尚无完整参考。现有信息见[策略模型](../concepts/policy-model.md)、[文件策略](../guides/policies/files.md)与[网络策略](../guides/policies/network.md)。

## 要回答的问题

`policy.toml` 与 Run TOML 中 `[policies.*]` 的 `[network]`、`[filesystem]` 支持哪些字段，合并规则是什么？`--safe` 预设具体生成了哪些规则？

## 需求

- 从策略类型定义自动生成字段表：字段、类型、默认值、取值范围；
- 列出 `--safe` 预设在每个平台、每个 Agent 命令名下生成的完整规则；
- 写明各层合并规则，并给出每条规则对应的测试或语义规格。

## 验收标准

- 字段表由代码生成或经 CI 校验；
- `--safe` 预设表与 `apply_safe_defaults` 的实现一致。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：[配置文件参考（规划中）](config.md)
