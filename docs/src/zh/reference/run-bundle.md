---
status: todo
search:
  exclude: true
---

# Run Bundle 格式

!!! warning "规划中"
    本页尚无完整参考。字段的含义与证据口径见[能力、证据与保证边界](../concepts/capabilities-and-evidence.md)，目录结构见 [Run 项目发现](cli.md#run-项目发现)。

## 要回答的问题

`run-bundle.json`（当前 schema 版本 4）包含哪些字段？哪些是强制力证据，哪些是派生摘要？跨版本如何兼容？

## 需求

- 从 Bundle 类型定义生成 JSON Schema，并发布在本页；
- 每个顶层字段说明：来源（准入计划、执行器观察、OverlayFS、OverlayNet、Gateway）、是否可能为 `null`、`null` 与零的区别；
- 写明 schema 版本策略：何时升级、旧 Bundle 是否可读（当前缺少观察契约的旧 Bundle 拒绝读取）。

## 验收标准

- JSON Schema 文件随版本发布，CI 校验实际输出符合 schema；
- 本页给出一个最小示例 Bundle 与逐字段注释。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：[机器可读输出（规划中）](json-output.md)、[稳定性承诺（规划中）](stability.md)
