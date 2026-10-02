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

## 单次 rollout 的现有拼装方式

当前可以将一次命令执行、模型流量记录和原生 Agent 轨迹作为一个评测样本。先为样本准备干净 workspace，再运行命令并保留 Job ID、Run Bundle、Event Journal 与 Agent 原生轨迹。reward 的计算、任务队列、模型训练和跨节点调度由现有框架负责。

| 产物 | 用途 | 不能替代 |
| --- | --- | --- |
| Run Bundle | 结果、控制与文件变化 | 训练框架的 reward 与数据集元数据 |
| Gateway Journal | 经过网关的模型调用 | 未捕获流量与 Agent 原生 session |
| Agent 原生轨迹 | 对应适配器的工具前缀回放 | 进程内存快照 |
| 逻辑检查点 | 分叉暂存文件状态 | 完整环境和外部服务状态 |

## 固定样本身份

为每次 rollout 保存任务 ID、pVisor 提交、Agent/模型版本、初始仓库提交、镜像摘要、采样参数、策略和执行器。分别记录任务失败、隔离拒绝、记录失败与续跑质量；不要把基础设施失败记为“模型没有能力”。

回放在新环境中重新执行工具，会再次发生相应副作用。使用测试 API/数据库和独立输出目录；用 `--prepare-only` 先验证格式，用 `--replay-only` 验证工具前缀，再开始真实续跑。固定适配版本与边界语义见[回放](replay.md)。集群吞吐与 hostile 多租户仍未建立保证。
