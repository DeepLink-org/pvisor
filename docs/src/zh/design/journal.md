---
status: todo
search:
  exclude: true
---

# Journal 设计

!!! warning "规划中"
    本页需要实现负责人撰写。事件字段与因果关系见 [Operation 与 Event](operations-events.md)。

## 要回答的问题

Event Journal 如何保证追加顺序、持久化与因果引用？写入失败或进程崩溃后会发生什么？

## 需求

- 提交、fsync 与回执的时序；
- Journal position 与 `caused_by` 的语义，以及为什么 `observed_at_unix_ms` 不是顺序依据；
- 去重与尾部恢复；
- 污染（poisoning）：何时把 Journal 标记为不可信，之后的行为。

## 验收标准

- 每条机制给出对应的代码位置与测试；
- 与 Operation 与 Event 页去重：字段契约留在该页，机制写在本页。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关代码：`crates/pvisor-journal`
