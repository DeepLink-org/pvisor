# 架构

PolicyVisor（pVisor）通过能力准入、执行器、运行时控制和执行记录，管理 Agent CLI、脚本和自动化命令。当前交付围绕本地 Run 及其可审查的结果展开，集群调度仍属于设计方向。

## 组件职责

| Crate | 职责 |
| --- | --- |
| `persisting-pvisor` | CLI、准入、Attempt 生命周期、执行器、Run Bundle、审查／应用／检查点 |
| `persisting-control` | 运行与 Overlay 契约、能力策略、控制消息与客户端、共享事件记录 |
| `persisting-overlay-core` | 写时复制、preimage、review/apply/recovery/drop 语义 |
| `persisting-overlayfs` | 宿主 FUSE 适配器 |
| `persisting-overlaynet` | 解析、代理转发与 VM 网络接入；消费 Control 策略 |
| `persisting-gateway` | 模型路由、协议转换与捕获 |
| `persisting-replay` | Agent 原生轨迹的回放与续跑适配 |

## 执行链路

```text
CLI／配置 → RunSpec → 能力准入 → RunPlan IR → 准备运行时驱动
  → 执行器启动命令 → 退出／取消／超时 → 清理
  → 本地 Run 记录 + Run Bundle + 最终结果
  → 后续审查／应用／丢弃
```

当前每次运行创建一个 Attempt。宿主执行在主进程退出后清理进程组，并限时排空输出，超时和取消路径也遵循此规则。主动脱离进程组的后代不在进程组清理范围内；输出排空期限可以避免其继承的管道阻塞 Run 完成。更强的后代进程隔离取决于所选平台机制。

## Run IR 与观测

准入阶段先解析执行器、应用应用程序策略和网络配置，再从有效 `RunSpec` 编译不可变的 `RunPlan`。计划使用 `run.execute` IR 请求以及有序的 VM／Overlay context rewrite 表示运行落点；每项文件、网络和环境规则带有稳定的 Run 内 ID、目标、动作及该维度的预期控制计划。嵌入调用方可以通过 `PVisor::resolve_run_plan` 在不启动 Attempt 的情况下读取同一份计划。

运行事件携带 IR 请求、重写和完成事实；Run Bundle 保留计划；`run.json` 只保留运行状态、执行器选择身份等运行事实。IR 是运行计划与观测的数据契约，不增加命令行入口。FUSE 文件视图按挂载相对路径和操作记录命中、成功、拒绝、其他失败、成功修改操作次数、失败修改操作可能留下副作用的次数和读写字节数；匹配到的 deny／warn 规则及 stage 规则也有计数。路径表最多保留 8192 个不同路径，其余命中计入 `overflow_hits`。这些是到达 FUSE 的操作计数，不代表唯一文件数或最终文件差异；最终变更仍以 OverlayFS diff 为准。

Run Bundle 还记录经过 OverlayNet 的聚合放行、拒绝、失败及字节量。未经过 FUSE 的文件授权、未经过拦截器的网络流量没有可靠逐条计数，相应字段为 `null`，含义是未观测而不是零。宿主选择性代理只覆盖经过代理的流量，计数不能证明没有绕过代理的连接。

唯一生产派发路径是 `PVisor::run(RunSpec) → Session → RunExecutor::execute`。RunPlan IR 描述放置与证据，
不执行任意表达式，也不提供逐次改写授权或通用披露检查。独立的 Engine/Backend/Admission 解释器已删除。

`ExecutorPlan` 与 `CapabilityEnforcementPlan` 是准入计划类型，最高等级为 Planned，
不能表示 Enforced。执行器在收尾时依据受保护的 sandbox/VMM 安装回执返回控制观察集。
Bundle schema 3 的 `executor_observations` 是唯一权威强制力证据，安全摘要只从它派生；
metadata、隔离标签和 warning 字符串均不能生成或抹去证据。Enforce 模式缺少必需观察时失败。
VM 启动被取消或信号中断且没有确认 runner 退出时，不声明 Enforced。
Completed 与终态事件的 origin 区分运行器失败和后端结果。

## Session 生命周期与策略

核心 `Session`对应一次 Attempt，统一协调 prepare、执行、取消与超时、
驱动清理、证据检查、Run Bundle 持久化和终态公布。后端返回必须包含观测的
`ExecutorOutput`，不能设置 Run/Attempt 身份或提前公布终态。Process 和 VM
共用进程组终止算法；OCI 使用 runtime 的 kill API。新 Attempt 使用新的
Session 身份和审批缓存键。

Session 持有准备后的驱动与 AgentCtl server。`RunHandle` 提供状态查询、取消、
checkpoint 和有序事件订阅。取消只发出请求；等待执行结束后才有最终结果。
AgentCtl 保留工作负载协作与 quiesce/checkpoint 职责；没有另一套 Session 控制协议或生命周期 Hook。

Control 统一拥有网络配置、编译后的授权与地址分类，以及文件策略编译。
Session、workspace、user 与执行器基础网络策略共同约束权限：所有网络层均须放行，
任一层显式 deny 都拒绝。已声明的网络层省略 `default_action` 时默认 deny，
未命中 allow 的目标不会退化为 Ambient；省略整个网络层才不增加约束。
各层匹配的带宽限制全部叠加。文件规则跨层取最严格决策（deny、ask、warn、allow），
因此仓库或 Session 的 allow 无法放宽用户拒绝或基础策略；端口、协议和解析 IP
校验失败也不会被其他层授权覆盖。交互式网络审批可为单次目标扩展 allow，
但仍不能覆盖任一层显式 deny、基础 deny-all 或解析地址安全检查。

文件策略绑定 Run、Attempt、视图和审批端点。FUSE 和 virtio-fs 共用 OverlayCore
的授权与写时复制实现；转换到 VM 根视图时保留策略作用域。`OverlayLayout`
在创建可写状态前校验 apply target 必须是最后一个 lower；apply 规划也检查同一约束。

策略文件路径与 TOML 示例见[网络指南](../guides/network.md)。

## 文件应用

`persisting-control::overlay` 定义审查／应用记录、首次修改状态格式和本地 Run 检查消息。OverlayCore 负责文件指纹、日志、review/apply/recovery/drop；pVisor 处理请求并管理挂载。

OverlayCore 在首次修改时记录目标的原始状态。Apply 将选择扩展到必要的目录和硬链接成员，校验受影响的原始状态，写入持久化意图，更新目标，然后移除已经应用的 upper 条目。

递归删除或替换目录时，除了目录本身，还校验已记录的后代。分批应用保留剩余子文件需要的目录新基线。恢复流程区分 `Prepared`、`TargetApplied` 和 `Committed`，避免把清理了一部分的 upper 当成新的改动。

单文件替换使用临时文件与 rename。整个批次不是一次原子文件系统事务：中断后可能只有部分改动可见，需要恢复。目标锁只串行化 pVisor 的 apply，不会锁住编辑器或其他写入者；应用时应停止并发写入。

## 运行记录与捕获

`run.json` 是本地 Run 记录，`run-bundle.json` 汇总结果、控制、产物及文件系统／网络证据。可选的 Trace Event journal 保存生命周期和 Gateway 事件，与 Bundle 的内容范围不同。

Gateway 使用有界应用队列和共享事实 Journal。进入队列不等于持久化；重启从已提交事实重建投影。捕获和协议转换也不能证明全部网络流量都经过了拦截。

## 扩展点与限制

执行器和 sink 接口支持嵌入集成，不代表已实现自动重试调度、外部动作恰好一次执行、分布式事务或密码学节点证明。后续部署思路见[从本地到集群](local-to-fleet.md)，平台强制执行见[隔离设计](isolation.md)。
