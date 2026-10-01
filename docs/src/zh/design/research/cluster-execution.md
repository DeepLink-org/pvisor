---
status: todo
search:
  exclude: true
---

# 集群化执行（L3）

!!! warning "规划中"
    本页尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

跨节点调度、集中证据与批量委托的边界在哪里，与 Kubernetes / Ray 如何分工？

## 需求

- 指标：跨节点调度吞吐、证据集中后的审计成本、单集群并发上限
- 对照组：Kubernetes、Ray 原生调度
- 工作负载：多节点批量 Agent 执行
- 环境：多机集群；固定调度器版本

## 验收标准

- 明确与调度器的边界（pVisor 是执行语义层，不替代调度器）
- 给出缺口清单与阶段划分
- 与 benchmarks/density 对齐

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：why/trust-ladder、guides/parallel-agents
