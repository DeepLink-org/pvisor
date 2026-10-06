# 集群规模执行与集中证据

通过**外部编排**把执行扩展到多主机，同时明确每次执行的边界与证据。Kubernetes、Ray 或训练框架可以拥有主机选择和业务工作流，pVisor 不用分布式产品控制面替代它们。

## 本机执行基础 {#execution-flow}

普通 `pvisor run` 通过原生执行器、可选暂存与 Gateway 管理 Job/Run/Attempt 并生成执行记录。[单节点 daemon](../daemon/index.md) 则拥有本机原生 VM sandbox 准入、生命周期、TTL 和服务端点。部分 OpenSandbox profile 不自动提供原生 Job 语义、VM restore、stage/apply、offload 或集中 Run Bundle。

原生 node 资源可以在一台主机上保留不可变环境／backing 所有权，独立于 daemon。它们不跨机器共享物理 RAM，也未接入 daemon 后端，见[共享工作集](../daemon/shared-working-set.md)。

早期 Controller/Worker 实现探索了分布式执行和证据交付。其归档测量保留历史范围，不描述新 daemon，也不验证当前分布式产品。

## 产品与外部集成边界 {#boundary}

| 层 | 当前职责 | 待集成／验证 |
| --- | --- | --- |
| 原生 pVisor 执行 | Job 生命周期、执行器控制、暂存、可选 Gateway 与记录 | 编排适配器和代表性工作负载验收 |
| Sandbox daemon | NativeRuntime VM 准入、持久 supervisor 归属、生命周期与真实服务代理 | Bootstrap 提供／验证与 SDK profile；没有 stage/checkpoint API 或密度证据 |
| 原生 node 资源 | 本机不可变所有权、pin 与有界 warming | Daemon 接入、完整瞬时记账和工作负载测量 |
| 外部编排 | 主机选择、队列、依赖、重试与租户 | 显式执行／证据交接及业务副作用核对 |
| 证据／检查点仓库 | 原生完整性、发布与保留合同 | 集中收集、授权、复制与跨主机兼容性 |
| 评审／发布服务 | 消费明确保留的输出 | 基线核对、选择接受、业务核对与发布 |

本机记录路径或上传 JSON 不代表完整文件传输。检查点可下载不证明任意主机兼容恢复。把恢复解释为等价执行前，需定义固定输入、运行时 profile、模型版本与存储授权。

## 待设计实验 {#validation}

将使用 pVisor 执行边界的外部编排与固定版本 Kubernetes/Ray 或训练框架基线比较。固定工作负载、资源、模型和输出检查，测量有效完成吞吐、首个结果／恢复长尾、整组内存与集中评审成本。从[并发密度方法](../../benchmarks/density.md)扩展，不从历史空任务或原生共享机制外推 daemon 收益。

注入主机丢失、daemon／编排停机、分区、磁盘满、收集中断与凭据撤销。分别跟踪编排工作身份、本机 sandbox 或 Job/Run/Attempt 身份、原生观察及产物。超时或控制身份不证明外部副作用停止，重试策略必须核对未知副作用。

该研究方向不建立跨主机恢复、恰好一次外部副作用或 daemon 密度优势。当前 daemon [运维](../daemon/operations.md)只定义本机行为。

## 研究问题 {#research}

- 执行与编排之间怎样的最小交接能保留请求策略、实际控制、观察结果和完整证据？
- 哪些保留输入与运行时兼容检查让恢复可移植，哪些副作用需要业务核对而非 replay？
- 在固定正确性与审计覆盖下，批量评审节省多少监督成本，包括收集与存储成本？
- 在相同总资源下，不可变共享或有界按需加载能否改善首个有效结果和完整完成？

本机流程见[并行 Agent](../../guides/parallel-agents.md)，训练集成见 [RL 执行底座](rl-execution-substrate.md)。研究计划不创建产品调度器，也不替代生产验收。
