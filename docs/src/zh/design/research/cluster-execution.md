# 集群执行与集中证据

pVisor 已实现一个 Controller shard 与多个独立 Worker 的集群执行路径，包括能力匹配、租约、DAG、原生控制、检查点/分叉和集中证据。当前机制与故障合同以[Cluster 架构设计](../cluster/index.md)为准；这里保留研究场景和扩展边界。

## 一个任务如何经过 Worker {#worker-flow}

调用方提交原生 RunSpec 和固定输入要求，Controller 接受任务并在 Worker poll 时分配精确租约。Worker 最终准入后准备独立环境、执行并留下证据，再通过持久化 outbox 上传和交付。完整顺序见[任务路径](../cluster/index.md#task-flow)。

Controller 的运行视图通过 Worker 报告收敛，重启后要求新的归属确认。未知执行不会自动换节点重跑；首次创建任务、低频控制意图与终态回执仍持久化。失联与替代执行的边界见[状态与恢复](../cluster/state-and-recovery.md)。

## 实现与集成层的边界 {#boundary}

| 层 | 当前机制 | 待集成/验证 |
| --- | --- | --- |
| pVisor Worker | 原生执行、环境层、Gateway、控制观察、终态 outbox | 恶意节点信任边界、本地证据 GC、长期节点运维 |
| Cluster Controller | 队列、节点匹配、资源预留、DAG、对账、fencing | 多 shard/HA、每租户身份、在线历史压缩 |
| 证据与检查点仓库 | 本地 CAS、验证 manifest、引用保护、可选 FS/S3 快照发布与导入 | 复制存储、每租户空间、跨主机运行时兼容与恢复 |
| Kubernetes、Ray 或训练框架 | 可作为任务与生命周期请求的上层调用方 | GPU/rollout/scaffold 协调与端到端恢复策略 |
| 评审与发布服务 | 可读取已保留证据 | 基线验证、选择改动、业务核对、合入和部署 |

Run Bundle 本地路径是 Worker 引用；上传 JSON 不代表完整文件上传。检查点可下载也不证明任意节点可恢复；环境、快照和输入版本需要完整的兼容合同。

## 下一阶段实验 {#validation}

先保留现有协议、进程执行和硬件 gate 的验证范围，再在独立主机部署固定工作负载，测量有效执行量、启动/恢复长尾、内存密度和存储成本。方法从[并发密度](../../benchmarks/density.md)扩展，不能从单机空任务外推。

实验注入节点永久失联、Controller 突然终止、长网络分区、磁盘满、上传中断、凭据撤销及并行模型等待。每个结果需能定位输入、Task/Run/Attempt、原生观察与产物；核对未知副作用和显式 lost 处理，不能把控制面 fencing 当作外部效果回滚。

## 研究问题与发布条件 {#research}

进一步缩减 Controller 状态需要明确上层 desired state、Worker 有界 terminal inventory 与保留 manifest 的重建权威，解决未分配意图和已 ACK 历史后再讨论完全可重建 Controller。多 shard 和容灾需要新的 ownership 协议，不能用本地文件锁替代。

生产发布还需每节点/租户身份、代表性负载的长期故障与性能实验、兼容矩阵和运维闭环。分项机制与扩展约束见[Cluster 运维与验证](../cluster/operations.md)。本机用法见[并行 Agent](../../guides/parallel-agents.md)，训练集成方向见[RL 执行基座](rl-execution-substrate.md)。
