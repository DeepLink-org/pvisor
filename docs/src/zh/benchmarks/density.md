---
status: todo
search:
  exclude: true
---

# 并发密度与资源占用

!!! warning "规划中"
    本页尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

单机能同时跑多少个 Job？

## 需求

- 指标：单机并发 Job 数、每 Job 的 CPU 与内存开销、尾延迟
- 对照组：Docker 同等密度
- 工作负载：1、8、32、128 个并发 Job
- 环境：各执行器分别测量

## 验收标准

- 给出单机上限，作为 L2/L3 规划依据
- 尾延迟有数据
- 资源模型可复现

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：benchmarks/methodology、security/known-limitations
