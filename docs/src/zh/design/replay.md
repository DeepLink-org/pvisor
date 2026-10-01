---
status: todo
search:
  exclude: true
---

# 回放设计

!!! warning "规划中"
    本页需要实现负责人撰写。操作方法见[回放](../guides/replay.md)。

## 要回答的问题

工具前缀回放如何用新鲜的 observation 重建 Agent 的原生上下文，再交给实时 Agent 续跑？每个 Agent 适配器有什么限制？

## 需求

- 回放流程：读取轨迹、按 `after_step` 回放完整 tool batch、重建原生上下文、启动实时 Agent；
- 各适配器的机制与限制：Claude Code、Codex、OpenCode 等；
- 回放与续跑的边界：哪些副作用不会被重放。

## 验收标准

- 每个适配器给出代码位置与测试；
- 与[回放保真度（规划中）](../benchmarks/replay-fidelity.md)的数据互相引用。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关代码：`crates/pvisor-replay`
