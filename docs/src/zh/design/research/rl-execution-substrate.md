---
status: todo
search:
  exclude: true
---

# 作为 Agentic RL 执行基座

!!! warning "规划中"
    尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

pVisor 作为大规模、不可信执行的基座，用于 Agentic RL rollout 与评测时需要什么？

## 需求

- 指标：rollout 吞吐、隔离有效性、轨迹可复现率、分叉成本
- 对照组：现有 RL 框架的沙箱组件
- 工作负载：固定任务的批量 rollout，含失败重放与检查点分叉
- 环境：固定模型与工具版本

## 验收标准

- 给出集成方式与边界
- 轨迹记录、分叉、按前缀回放的行为有测试
- 与 guides/rl-rollouts 分工明确

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：guides/rl-rollouts、design/replay
