# 核心架构

pVisor 是 Operation 的处理核心：接收操作请求，根据策略决定如何处理和放置，调用实际执行机制，再通过 Event 描述发生了什么。

这条主线分成两个职责：**core 提供定义，pvisor 提供实现。** 外部调用方提交执行请求，通过运行句柄控制一次执行，通过 Event 观察它的过程和结果。

## 定义与实现

| 所有者 | 职责 |
| --- | --- |
| `pvisor-core` | Operation、策略决定、Placement、Outcome、Event，以及跨组件交互契约；共享纯校验与策略求值 |
| `pvisor` | 请求解析、能力准入、实际策略改写、放置选择、调度、执行和 Attempt 生命周期 |
| `pvisor-journal` | Event 提交、回执、去重、因果引用校验和恢复 |
| `pvisor-overlay-core` | 文件授权接入、写时复制、preimage、review/apply/recovery/drop |
| `pvisor-overlayfs` | 宿主 FUSE 挂载与文件操作接入 |
| `pvisor-overlaynet` | 网络解析、代理转发和 VM 网络接入；落实 core 定义的策略 |
| `pvisor-guest` | VM 内 PID 1 与命令启动契约 |
| `pvisor-gateway` | 可选的模型协议路由、转换和调用观察 |
| `pvisor-tui`、`pvisor-replay` | 依赖 pvisor 的终端前端与 Agent 轨迹回放工具 |

core 不拥有执行循环，也不启动进程或打开控制 socket。pvisor 实现 AgentCtl 客户端／服务端和审批 socket；驱动实现各自的文件、网络与隔离边界。默认核心不依赖 Gateway、TUI 或 replay，捕获通过 `gateway` feature 启用。

## 一条生产执行路径

```text
CLI／嵌入调用方
  → RunSpec
  → pvisor：准入、策略改写、Placement → 有效 Operation
  → Session：准备驱动与运行资源
  → 提交启动事实
  → RunExecutor::execute
  → 清理、检查控制观察、保存结果、公布终态
```

`RunSpec` 是调用方的执行配置输入；`Operation` 是结构化的操作描述。目前唯一生产操作是 `run.execute`，包含程序、参数和工作目录。执行器实际消费有效 RunSpec 及准备好的驱动附件。`PVisor::resolve_operation` 使用同一准入路径，供启动前审查，不能代替实际执行及安装证据。

准入保留请求与有效操作的快照。实际策略变化记录为 Rewritten，选定的 VM／Overlay 放置记录为 Placed。拦截发生在实际驱动边界：文件操作进入 OverlayFS／OverlayCore，网络流量进入 OverlayNet。它们共享策略定义，但当前并没有将每个文件或网络操作都提升为独立的公共 Operation。

例如，网络驱动把请求的 Ambient 能力收窄为 Deny：Requested 保留原始权限，Rewritten 保存收窄前后快照，Placed 描述最终放置，执行器按有效配置运行。这是实际策略处理的记录，不是通用规则解释器。

## 一个生命周期所有者

当前每次 `PVisor::run` 创建一个 Attempt，由 pvisor 中的 `Session` 统一持有和管理；Job、Run 与 Attempt 的身份区分见[执行模型](../design/execution-model.md)。

Session 负责驱动准备、AgentCtl server、取消与超时、执行后清理、观察检查、Bundle 保存及终态公布。执行器返回 `ExecutorOutput`，不自行分配 Job／Attempt 身份或公布终态。`RunHandle` 提供状态、取消、checkpoint 和事件订阅；取消请求不等于执行已经停止。

Process／VM 清理其受管理的进程组；容器使用 runtime 的终止接口。进程组之外的后代和各平台隔离缺口见[隔离设计](isolation.md)。AgentCtl 只负责工作负载协作和 checkpoint 静默点，本身不是强制控制。

## Event 是观察接口

外部观察的是 Event，不需要依赖 Session 的内部字段。事件链、身份、因果引用与记录边界见 [Operation 与 Event](operations-events.md)。

当前实现先准备驱动，再提交启动事实，最后调用执行器。必要启动事实提交失败会阻止执行器派发并清理准备资源；这不代表准备阶段完全没有文件或 socket 副作用。

## 策略、控制与证据

策略、准入计划与执行后观察是三个独立层次，不能互相替代；`ExecutorPlan`／`ExecutorObservations` 的等级与证据口径见[能力与证据](../concepts/capabilities-and-evidence.md)。

core 共享文件／网络策略求值。user、workspace、session 和执行器基础策略共同约束权限，后面的 allow 不能覆盖前面的显式拒绝。实际授权、拦截和控制安装由 pvisor 与驱动落实。

## 记录与文件应用

| 记录 | 回答的问题 |
| --- | --- |
| `run.json` | 这项 Job 的身份、状态、执行器及本地资源是什么？ |
| Run Bundle | 结果、控制观察、产物及文件／网络摘要是什么？ |
| Event Journal／Trace | 已发布了哪些事实，它们如何关联？ |
| Overlay diff／preimage | 哪些文件仍待审查，应用时应检查什么原始状态？ |

这些记录各有范围。Event 可以重建已观察到的操作过程，无法仅凭日志恢复全部外部状态。Agent 原生轨迹 replay 也不是任意副作用的确定性重放。

暂存文件先审查再应用。OverlayCore 负责目标校验、持久化应用意图、更新及恢复；一个批次不是原子文件系统事务。`apply`、`drop` 与文件检查点的恢复范围及不能撤销的外部副作用见[能力与证据](../concepts/capabilities-and-evidence.md)，操作流程见[审查与应用](../guides/review-apply.md)。
