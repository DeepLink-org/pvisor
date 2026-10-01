---
status: todo
search:
  exclude: true
---

# 作为 Agentic RL rollout 与评测的执行层

!!! warning "规划中"
    本页尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

能否把 pVisor 当作大规模、不可信执行的基座，用于 Agentic RL rollout 与评测？

## 需求

- 指标：rollout 吞吐、隔离有效性、轨迹可复现率、分叉成本
- 对照组：现有 RL 框架的沙箱组件；OpenHands runtime 类方案
- 工作负载：固定任务的批量 rollout，含失败重放与从检查点分叉
- 环境：集群或多机环境；固定模型与工具版本

## 验收标准

- 给出与训练框架的集成方式与边界
- 轨迹记录、分叉、按工具前缀回放的行为有测试
- 明确与 design/research/rl-execution-substrate 的关系

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：design/research/rl-execution-substrate、guides/replay
