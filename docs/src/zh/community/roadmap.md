# 路线图

主线的分级与规模轴见[信任阶梯](../why/trust-ladder.md)。下表列出 L1（本机逐个 Job）的持续工作与验收标准。

| 工作 | 验收标准 |
| --- | --- |
| 本地生命周期与暂存可靠性 | macOS/Linux 的完成、取消、超时、fork 和分批 apply 路径有回归；失败后可检查记录及剩余改动 |
| 边界与证据一致 | 按执行器验证文件和网络控制，Bundle 区分计划、安装回执、未观测与零计数；缺失控制不被标签掩盖 |
| Gateway 捕获可靠性 | 有界队列、提交失败、关闭和恢复有回归；明确入队与持久提交的差别 |
| Replay 兼容性 | 每个适配器固定受支持版本，验证完整工具批次和第一次续跑请求边界；实验报告记录样本及局限 |
| 文档与分发一致 | 入口示例可运行，默认行为只有一处权威定义 |

新增公开功能前，应具备实现入口、验证场景、限制说明和发布记录。改变数据契约、执行边界或公开命令时，先明确兼容策略和验收方式。

## 单机 daemon {#daemon}

[Daemon](../guides/daemon/index.md) 通过部分 OpenSandbox 1.1.0 profile 管理本机镜像沙箱。Controller/Worker 调度与 Cluster 任务 SDK 已退役。原生 Job、VM checkpoint/local fork 和 node/cache/memory-pool 服务独立保留；daemon 与原生 VM 的集成尚未交付。prepared execd/egress 镜像契约仍需端到端验证，包括 `cap-drop=ALL` 下的无 capability egress。

## L2 与 L3 里程碑

!!! note "建设中"
    L2/L3 的进入条件与缺口清单尚未定稿；下面汇总已写下的要求与验收标准，不构成排期或完成度承诺。级别定义见[信任阶梯](../why/trust-ladder.md)，容量依据见[并发密度](../benchmarks/density.md)。

### L2：本机多个 Job／一条流水线

进入条件（待定稿）：本机并发上限与资源模型有数据，作为容量规划依据，见[并发密度](../benchmarks/density.md)。

验收标准：

- 批量审查流程可复现：按 workspace 聚合、批量 apply，见[单机多 Agent 并行与批量审查](../guides/parallel-agents.md)。
- CI 接入给出可复制的 workflow 示例，明确 `apply` 的语义（谁审、何时合），失败与超时路径有回归，见[在 CI 中运行 Agent](../guides/ci.md)。

### L3：集群化执行、集中证据

进入条件（待定稿）：明确与调度器的边界——pVisor 提供执行语义与证据，跨节点编排交给 Kubernetes、Ray，见[集群化执行](../design/research/cluster-execution.md)。

验收标准：

- 给出外部跨节点调度器集成及证据集中后审计的缺口清单和阶段划分。pVisor 不提供集群总控；单机 daemon 没有全局 DAG 或分布式 lease。
- 容量依据与 L2 共用[并发密度](../benchmarks/density.md)。

