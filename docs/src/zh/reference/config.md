---
status: todo
search:
  exclude: true
---

# 配置文件参考

!!! warning "规划中"
    本页尚无完整参考。现有信息见 CLI 参考的[一套配置模型](cli.md)。

## 要回答的问题

`pvisor run --config run.toml` 支持哪些字段，每个字段的类型、默认值、对应的 CLI 参数和覆盖规则是什么？

## 需求

- 从 `RunConfig` 的 serde 定义**自动生成**字段表，不手写，避免与代码漂移；
- 每个字段列出：TOML 路径、类型、默认值、对应 CLI 参数、标量替换还是列表追加；
- 标出当前不按配置值生效的字段（例如部分 CLI 路径上的 `run.inherit_env`）。

## 验收标准

- `just docs-build` 时生成，或由 CI 检查生成结果与代码一致；
- 覆盖 `[run]`、`[filesystem]`、`[overlaynet]`、`[gateway]`、`[record]`、`[policies.*]`、`[container]`、`[vm]`。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：[CLI 参考](cli.md)、[策略字段参考（规划中）](policy.md)
