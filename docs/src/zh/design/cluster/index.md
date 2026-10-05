# Cluster 架构设计

pVisor Cluster 把同一套 `RunSpec` / `RunResult` 执行语义扩展到多 Worker：Controller 接受任务、选择节点并维护执行身份，Worker 准备环境、执行原生操作并交付证据。当前部署单位是一个拥有独占状态目录的 Controller shard；多个 Worker 可以独立运行。

设计基线为 2026-10-05 的当前源码。运行态采用 Worker 对账与最终一致性；任务意图和终态回执仍有低频持久化。部署、协议和验证边界分别在下列文档中定义。

实际搭建与逐步验证见[Cluster 上手指南](../../guides/cluster/index.md)，包括受限资源运行、恢复、VM 与离线模型 Gateway。

| 主题 | 文档与主要问题 |
| --- | --- |
| 状态与恢复 | [状态权威、租约与重启对账](state-and-recovery.md)：什么必须保存？失联后能否重跑？ |
| 调度 | [调度、准入与 DAG](scheduling.md)：节点匹配、资源预留、最终准入和依赖推进 |
| 执行生命周期 | [原生控制、推理等待与分叉](lifecycle.md)：暂停何时释放 CPU？回复何时可以交给 Agent？ |
| 存储 | [元数据、产物与回收](storage.md)：提交屏障、outbox、检查点发布与 GC |
| 共享与惰性加载 | [共享工作集与惰性加载](shared-working-set.md)：现有复用、目标成本、cache预算与先验实验问题 |
| 接口与运维 | [协议、部署与故障处理](operations.md)：API、配置、恢复步骤、验证与扩展方向 |

## 分层与组件职责 {#architecture}

```text
调用方 / 训练系统 / 平台服务
        │ TaskSpec、DAG、取消、控制、分叉；查询结果
        ▼
Controller HTTP API（Admin / Worker 两种角色）
        │ 有界 Dispatcher → 单写者 Scheduler
        ├── 任务 / DAG / 控制意图 / 分配身份 / 终态回执
        ├── Worker 派生运行视图、资源预留、索引
        └── 低频事务日志 + Controller 本地产物 CAS
        ▲
        │ Worker 主动 register / poll / recover / ACK / complete
        ▼
Worker（节点配置、最终准入、租约 watchdog、终态 outbox）
        ├── pVisor 执行器：host / rootless / container / VM
        ├── Attempt Gateway：模型授权、路由、捕获、推理等待
        ├── OverlayFS / OverlayNet：实际文件与网络边界
        └── 环境缓存、原生检查点、可选 FS/S3 检查点仓库
```

| 组件 | 拥有的决策与事实 | 不承担的职责 |
| --- | --- | --- |
| 上层调用方 | 工作负载、输入版本、任务 ID、租户、重试与业务副作用策略 | 根据超时推断一次执行没有副作用 |
| Controller | 已接受意图、执行身份、控制修订、逻辑资源记账、聚合结果与证据根 | 直接决定内核是否已停、控制 vCPU 或保管模型供应商凭据 |
| Worker | 节点最终准入、实际执行、原生控制观察、完整活动身份清单、交付重试 | 修改任务定义、擅自接管旧 incarnation 的未知执行 |
| Core | 跨组件共享的版本化类型和校验规则 | 全局调度或节点运行时 |
| 执行器与驱动 | 平台上的隔离、终止、暂停、快照与资源控制 | 把 Controller 的预留数值当作已安装的限制 |
| 产物与检查点仓库 | 不可变内容、完整性校验、引用和保留 | 仅凭本地路径证明跨节点可恢复 |

Cluster 不把 Kubernetes、GPU 训练调度器或 rollout/scaffold 状态纳入原生控制面。它们可以提交任务和生命周期请求；检查点与 GPU 调度、训练事务之间的协调仍由集成层负责。

## 一次任务的完整路径 {#task-flow}

1. 调用方提交不可变 `TaskSpec`；Controller 验证版本、身份、资源、执行要求及引用。持久化成功后返回记录，相同 ID 与相同内容的重复提交返回原记录。
2. Worker 注册能力，主动轮询并携带完整活动 `LeaseKey` 清单、剩余容量和节点压力报告。
3. Controller 在有界 ready 窗口中选择兼容任务，同时检查 Worker/租户预留与可用量。分配身份和预留持久化后才返回 assignment。
4. Worker 再次检查本机条件。未启动的任务可用精确 key 拒绝并回队列；接受后为任务准备独立状态目录、执行器和可选 Gateway。
5. Worker 启动原生执行，以单调时间 watchdog 约束租约。后续 poll 确认归属并重建 `Running` 和 deadline；纯续租不写日志。
6. 原生执行和本地证据封存结束后，Worker 保存终态 outbox。可选 `native-done` 交接释放执行槽位，保留有界上传预留；所需产物随后上传并校验。
7. Controller 接受精确身份的最终结果，持久化回执、释放预留、推进 DAG。Worker 保存对应回执后移除待交付项；已确认的本地证据仍可能占用磁盘。

执行失败、产物失败和业务效果分别解释。原生命令退出成功但要求的产物交付失败时，保留原生结果，聚合任务不能按成功推进依赖。

## 核心设计不变量 {#invariants}

- 同一任务 ID 的定义不可变；控制和分叉请求使用调用方指定的幂等 ID。
- 每次执行由 `{task_id, worker_id, incarnation, generation}` 标识；身份相同才能更新其记录。传输重试可以发生，未知执行不会自动换节点重跑。
- 新分配、控制意图和最终回执在持久化屏障之后才确认；HTTP 超时或断开不撤销已进入队列的操作。
- Controller 重启后，历史租约不能证明执行仍存活，也不能证明已停止；待对账任务保留资源和产物根。
- Worker 最终准入和原生观察决定实际执行；请求、预留、安装的控制与观察结果分别记录。
- GC 不删除活动租约、已保留证据或下载保护仍引用的对象；待对账期间禁止破坏性回收。

这些不变量保证控制面记录的归属与恢复顺序。它们不提供外部 API、数据库或消息的 exactly-once 执行；需要业务方的幂等键、补偿和核对。

## 源码边界与阅读顺序 {#source-map}

源代码位于仓库根目录下；共享协议只有一份权威定义。

| 路径 | 职责 |
| --- | --- |
| `crates/pvisor-core/src/cluster.rs` | Task/Worker/Lease/Graph/Control、环境、产物及遥测协议 |
| `crates/pvisor-cluster/src/scheduler.rs` | 状态机、事务应用、租约、记账、匹配、恢复 |
| `crates/pvisor-cluster/src/scheduler/{indexes,graph,inference}.rs` | ready/计数索引、DAG 推进、推理等待 |
| `crates/pvisor-cluster/src/server.rs`、`server/dispatcher.rs` | 身份角色、路由、请求界限、单写者和 group commit |
| `crates/pvisor-cluster/src/{journal,artifacts,environment,admission,physical_memory}.rs` | 元数据日志、CAS、环境校验、节点准入、物理内存报告 |
| `crates/pvisor-cluster/src/artifacts/{gc,quota}.rs` | 引用、下载保护、GC、存储配额与发布 |
| `crates/pvisor-cluster/src/{client,main}.rs` | 类型化 HTTP 客户端与 CLI |
| `crates/pvisor/src/bin/pvisor-worker.rs`、`bin/worker/` | 节点循环、watchdog、执行、outbox、Gateway、环境和快照集成 |

修改一个模块后，应回到状态权威和不变量检查影响：例如改变 native-done 不能只优化槽位，还必须检查上传生命周期、租约续期、终态重复提交与 GC 根；改变暂停不能只看 vCPU，还必须检查 CPU 记账、手工控制所有权与 Gateway 回复屏障。

当前只支持单 shard 独占写入。跨主机兼容性、HA、在线元数据压缩、每租户身份和长期密度证据的边界见[运维与扩展](operations.md#evolution)。
