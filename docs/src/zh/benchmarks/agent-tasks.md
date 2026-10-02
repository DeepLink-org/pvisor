---
status: todo
search:
  exclude: true
---

# 端到端 Agent 任务开销

!!! warning "规划中"
    尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

用 pVisor 跑真实 Agent 任务，成功率和耗时受多大影响？

## 需求

- 指标：墙钟时间、token 用量、任务成功率
- 对照组：同一 Agent 不加 pVisor
- 工作负载：SWE-bench Lite 子集（或自建任务集），Claude Code、Codex 各一组
- 环境：固定模型与工具版本

## 验收标准

- 成功率差异在统计误差内
- 给出开销百分比
- 任务集与配置公开

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：benchmarks/methodology、security/known-limitations
