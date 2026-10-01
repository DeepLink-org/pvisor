---
status: todo
search:
  exclude: true
---

# 退出码与错误

!!! warning "规划中"
    本页尚无完整参考。已知行为：`pvisor run` 原样返回命令的退出码；`--strict` 在缺少强制证据时以 `UnsupportedPolicy` 拒绝运行。

## 要回答的问题

`pvisor` 各子命令在什么情况下返回什么退出码？pVisor 自身的错误如何与被运行命令的退出码区分？

## 需求

- 列出每个子命令的退出码与含义；
- 列出主要错误类型（策略不支持、隔离安装失败、apply 冲突、Job 未找到等）及对应的退出码和提示文字；
- 说明 CI 中如何区分"Agent 失败"和"pVisor 拒绝运行"。

## 验收标准

- 退出码表与代码中的错误类型一一对应，并有测试覆盖。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：[在 CI 中运行 Agent（规划中）](../guides/ci.md)
