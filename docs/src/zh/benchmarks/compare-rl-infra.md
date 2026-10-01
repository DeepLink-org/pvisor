---
status: todo
search:
  exclude: true
---

# 对比：Agent RL rollout 基础设施

!!! warning "规划中"
    本页尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

和现有 RL rollout 环境相比如何？

## 需求

- 指标：隔离、轨迹记录、分叉与回放、并发密度、与训练框架的集成方式
- 对照组：OpenHands runtime；SWE-Gym 类环境；RL 框架的沙箱组件
- 工作负载：批量 rollout，含失败重放与检查点分叉
- 环境：固定模型与工具版本

## 验收标准

- 明确集成方式与边界
- 引用 benchmarks/density 与 replay-fidelity
- 提供「更正」入口

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：why/comparisons、benchmarks/methodology
