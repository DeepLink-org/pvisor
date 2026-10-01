---
status: todo
search:
  exclude: true
---

# 环境变量

!!! warning "规划中"
    本页尚无完整参考。投影给 Agent 的变量见[凭据与环境变量](../guides/policies/credentials.md)。

## 要回答的问题

pVisor 读取哪些 `PVISOR_*` 环境变量（例如 `PVISOR_RUN_HOME`、`PVISOR_CACHE_SERVER`），又向 Agent 注入哪些变量？

## 需求

- 从代码中收集全部读取点，自动生成两张表：pVisor 读取的变量、注入给 Agent 的变量；
- 每个变量说明：作用、默认值、适用的执行器与平台、是否稳定。

## 验收标准

- 表格由脚本生成，CI 检查没有遗漏代码中新增的 `PVISOR_*` 读取点。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：[CLI 参考](cli.md)
