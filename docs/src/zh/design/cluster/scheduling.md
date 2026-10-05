# 调度、准入与 DAG

Controller 以 Worker 主动轮询为触发点，在有界 ready 窗口中进行匹配。Controller 预留、Worker 上报的剩余量和 Worker 最终准入必须同时满足；其中任何一个都不能替代原生资源限制。

## 任务与节点模型 {#model}

`TaskSpec` 包含版本、不可变 ID、tenant、原生 `RunSpec`、执行类别、`Resources`、标签约束、缓存亲和键及可选环境、恢复、产物、Gateway 和 CPU QoS 要求。原生策略和运行身份仍由 Core 的共享定义解释。

`WorkerRegistration` 包含节点 ID/incarnation、总容量、可执行类别、标签和缓存键，并声明 Gateway、环境、VM control、检查点、产物导出、CPU QoS/观察等协议能力。缺少任务所需能力时不会静默降级。

| 资源 | 记账含义 |
| --- | --- |
| `slots` | 并发原生执行槽位 |
| `memory_bytes` | 准入 RAM 预算，不等于 RSS/PSS |
| `cpu_millis` | CPU 预留；1000 表示一核的逻辑预算 |

Worker 预留是该节点任务当前 reservation 的总和，租户预留是所有节点上该租户任务的总和。显式租户配额限制并发资源；未列入配额表的租户没有该项限制。资源加减和容量计算检查溢出，不使用有符号回绕容忍超额。

## 一次 poll 的调度路径 {#poll}

1. 校验 Worker ID/incarnation、完整 active 清单、重复身份、资源和批量边界；维护已确认租约的到期状态。
2. 对 exact key 续租，处理取消的 stop 指令，确认已收到 assignment。先维护既有执行，再考虑新分配。
3. 重发同一身份的未确认 assignment 和 issued control。Resume 的额外资源先记账，在重发时仍保守扣除，避免重复使用尚未确认的 CPU。
4. 如果 Worker draining，阻止新分配，继续既有执行和交付。对新工作，合并本地 available、Controller 剩余预留和有效 admission 报告。
5. 从 ready 索引取有界窗口，按缓存亲和排序并检查执行类别、标签、环境/恢复兼容性、Gateway 与 QoS 能力、租户配额和产物空间。
6. 在单个调度操作中建立租约、增加 generation 并预留 Worker/租户资源。持久化后返回 assignment；发布被明确配额拒绝时恢复队列位置，不返回未提交的新工作。

默认 `queue_lookahead = 256`、`max_batch = 64`。无法匹配的候选轮转，避免每次从大量终态历史扫描。亲和键是优化提示；有界窗口与轮转不构成严格公平性、优先级、抢占或尾延迟保证。

## 索引与复杂度边界 {#indexes}

ready 使用有序序列及 task→位置索引，终态/取消任务从中移除；phase counts 增量维护。expiry 按 deadline/task 排序，active 按 Worker 索引；DAG 按前驱维护后继和未满足依赖数。待对账租约不进入 expiry。

poll 的候选工作量由窗口与批量限制，依赖推进只访问受影响节点，counts 不遍历全历史。Worker 注册、重放、较大管理响应、GC 根收集和保留历史仍有各自成本；不能把局部有界算法称为整个系统恒定复杂度。

内存中每个 TaskRecord 只保留一份 boxed 主记录，DAG 保存拓扑和任务引用，避免复制完整 RunSpec。默认最多保留 1,000,000 个任务记录；终态证据 GC 不删除任务身份，元数据历史压缩尚未实现。

## 节点最终准入与压力 {#admission}

Worker 在启动前重检条件，未启动 assignment 可通过 `decline` 用完整 key 拒绝。Controller 持久化拒绝、释放预留并回队列，后续分配使用新 generation。拒绝不是对已执行任务的通用重试机制。

可选 Linux admission 采样 CPU/memory PSI、CPU affinity 和可见 cgroup v2 限制。过时压力报告不能作为放宽准入的依据；旧于租约窗口的报告会限制新工作。Worker 最终 available 可以低于其注册容量。

可选 CPU 预留 overcommit 需要有限节点 CPU quota 和显式策略；内核 quota、BE `SCHED_IDLE`、LS core scheduling 与逻辑预留分别作用于不同层次。暂停只释放逻辑 CPU；没有默认 RAM overcommit，也不能把驻留样本变化直接转成可复用 RAM 配额。

物理内存、CPU 使用率及 node-memory 接口提供带租约身份的观察，默认不持久化每个样本。它们目前不是计费、租户成本归集或跨节点容量预测系统。

## DAG 提交与推进 {#dag}

Graph 接受同一租户的 1–256 个节点、至多 4096 条边和 2 MiB 规格。提交前检查唯一 Task/Run ID、图内引用、自环、重复边、无环性及任务总量；不能收养已存在任务。全部节点与拓扑在一帧事务中创建。

无前驱节点进入 `queued`，其余为 `waiting_dependencies`。只有前驱聚合 `Succeeded` 才减少未满足依赖数；所需产物尚未交付时，原生成功不能放行依赖。失败、取消、Lost 等非成功终态阻断相关后继，独立分支仍可继续。传播采用工作队列，避免深链递归。

图取消在一个事务中记录意图并处理节点：未运行节点可以结束，已运行节点进入取消流程并保留预留，等待观察。图查询从原始拓扑和任务状态构造结果，图内节点没有第二套独立运行身份。

Graph 不自动把前驱产物挂载到后继工作区；调用方通过固定输入、环境或显式数据交付约定建立数据依赖。Graph 幂等重试要求相同 ID、节点顺序、依赖与完整规格。

## 资源交接表 {#reservations}

| 场景 | CPU | RAM / 槽位 |
| --- | --- | --- |
| Leased / Running | 全量预留 | 全量预留 |
| 暂停或 offload 请求尚未确认 | 保留原 charge | 全量预留 |
| 原生暂停/offload 成功 ACK 已接受 | 0 | 全 RAM 与槽位仍保留 |
| Resume 已 issued、ACK 未到 | 全量预留 | 全量预留 |
| Resume 失败 | 保留已增加的 charge 至后续结算 | 不乐观释放 |
| Cancelling / 待对账 | 保留当前 charge | 保留当前 charge |
| NativeDone 交接获接受 | 100 millis | 16 MiB、0 槽位；只在原预算可容纳时使用 |
| 原生终态未用交接协议 | 保留此前 charge 至最终接受 | 同左 |
| Suspended 或最终终态已接受 | 0 | 0；证据引用独立保留 |

交付协议最多允许额外 64 个交付身份；当前 Worker 交付并发另设 16 的局部界限。历史/旧协议不能使用交接优化时，安全地保留原预算。

实现主要位于 `scheduler.rs`、`scheduler/indexes.rs`、`scheduler/graph.rs` 与 `admission.rs`；原生策略及压力重检位于 Worker。
