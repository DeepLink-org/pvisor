---
status: todo
search:
  exclude: true
---

# 单机多 Agent 并行与批量审查

!!! warning "规划中"
    本页尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

在一台机器上同时跑多个 Agent 时，如何隔离、如何批量审查、单机上限是多少？

## 需求

- 指标：并发 Job 数、每个 Job 的 CPU／内存开销、尾延迟
- 对照组：顺序运行；Docker 同等密度
- 工作负载：8／32／128 个并发 Job 的固定任务集
- 环境：Linux（KVM/FUSE）与 macOS（HVF/macFUSE）各一组

## 验收标准

- 给出单机并发上限与资源模型
- 批量审查流程可复现（按 workspace 聚合、批量 apply）
- 与 benchmarks/density 对齐

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：why/use-cases、benchmarks/density
