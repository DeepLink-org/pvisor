---
status: todo
search:
  exclude: true
---

# Journal 设计

!!! warning "规划中"
    实现细节仍待负责人撰写。事件字段与因果关系见 [Operation 与 Event](operations-events.md)。

## 要回答的问题

Event Journal 如何保证追加顺序、持久化与因果引用？写入失败或进程崩溃后会发生什么？

## 需求

- 提交、fsync 与回执的时序；
- Journal position 与 `caused_by` 的语义，以及为什么 `observed_at_unix_ms` 不是顺序依据；
- 去重与尾部恢复；
- 污染（poisoning）：何时把 Journal 标记为不可信，之后的行为。

## 验收标准

- 每条机制给出对应的代码位置与测试；
- 与 [Operation 与 Event](operations-events.md) 去重：字段契约留在该页，机制写在这里。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关代码：`crates/pvisor-journal`

## 当前提交路径

实现集中在 `crates/pvisor-journal/src/lib.rs`。文件 Journal 使用独占锁和单写入者；日志文件为 0600，打开时拒绝符号链接。共享字段定义留在 [Operation 与 Event](operations-events.md)。

一次 `Journal::append` 依次执行：验证 Event → 计算内容摘要 → 取得写入锁 → 检查去重与因果环 → 分配连续位置 → 写入完整 JSON 行并 `sync_all` → 更新索引并通知订阅者 → 返回回执。磁盘回执是 `LocalSync`，内存 Journal 是 `Volatile`；收到实时通知不提供比回执更强的持久化保证。

| 情况 | 行为 |
| --- | --- |
| 同一个事件 ID、相同内容 | 返回原位置，不重复追加 |
| 同一个事件 ID、不同内容 | `Rejected`，不能把它当成新事件覆盖 |
| 已知因果图形成环 | `Rejected`；前向引用可以暂时未解析，不能用时间戳替代引用 |
| 写入或同步失败 | `Unknown`，句柄被污染；后续 append 与历史读取拒绝，必须释放并重新打开以恢复 |
| 异步等待方取消 | 已交给阻塞写入任务的 append 仍可能完成；不能以等待取消推断未提交 |

## 重新打开与尾部恢复

`scan` 验证格式版本、事件、连续位置、事件 ID 唯一性与因果图。重新打开可以截去末尾没有换行的未完成记录并同步；已经完整结束但损坏的 JSON 行、位置断裂或不支持的头版本会报错，不跳过。

事件提交顺序是该 Journal 的位置顺序，不是跨 Job 的副作用顺序。写入结果未知时，先恢复并按稳定事件 ID 检查记录；不要重复执行远程请求来“补日志”。

```bash
just test pvisor-journal
```

同文件中的 `write_error_requires_recovery_before_another_receipt` 与 `cancelling_waiter_does_not_cancel_accepted_append` 覆盖失败污染及异步取消。完整的故障矩阵与实现负责人审查仍待完成。
