# 原生控制、推理等待与分叉

Cluster 将控制意图、原生执行与成功观察分开。管理请求返回已接受的记录；只有 Worker 的精确身份 ACK 才能证明对应原生动作成功，并触发资源记账变化。

## 控制协议 {#control}

`ControlRequest` 以 task 内的 `request_id` 幂等，命令再绑定完整 LeaseKey 和单调递增 revision。相同请求重复读取同一记录；同 ID 不同 action 冲突。

```text
Pending（意图已提交）
  → Issued（已记账，允许发送/重发同一命令）
  → Succeeded / Failed（原生观察已接受）
  → 或 Aborted（取消/终止等撤销未完成控制）
```

| action | 成功观察与生命周期效果 |
| --- | --- |
| `pause` | vCPU 已暂停；接受 ACK 后 CPU charge 为零 |
| `offload` | 原生 offload 成功；保留全部 RAM/槽位，不能宣称零驻留 |
| `resume` | 先重新准入并增加 CPU charge，再发送；ACK 确认 Running |
| `checkpoint` | 完整 CPU/RAM/设备/拥有的文件系统状态已封存；源执行继续 |
| `suspend` | 封存完整状态并停止冻结的源；先 Suspending，原生终止与交付确认后 Suspended |

同一 task 的未完成控制必须按顺序收敛后才能发冲突动作。Worker 验证 lease/revision、保存控制观察并重试 ACK；旧命令不能控制新 incarnation/generation。取消或原生终止会终结未完成控制。

暂停与 offload 的 RAM 观察、资源语义见[offload 设计](../offload/index.md)和[预留表](scheduling.md#reservations)。控制失败不会乐观地回退已增加的预留。

## Attempt Gateway 与推理等待 {#inference}

Gateway 是 Worker 创建的 Attempt 局部服务，按任务要求匹配模型能力与授权。模型供应商凭据来自 Worker 的宿主配置；Agent 不继承 Controller 服务凭据。协议报告、路由捕获与实际网络边界见[Gateway](../gateway.md)和[OverlayNet](../overlaynet.md)。

VM Worker 的 `[gateway]` 同时启用 `enabled` 和 `release_cpu_on_idle`，且 Agent 发送 `x-pvisor-inference-idle: true` 时，可以协调释放等待推理期间的逻辑 CPU。该声明表示整个 guest 空闲，包括后台工作；Worker 不从一次 HTTP 请求自动推断全 VM 空闲。转发前移除这个本地 header。

```text
Agent 声明 quiescent call
  → Gateway 聚合 wait group（最多 64 个并行合作调用）
  → begin → 请求 Pause → 原生 ACK → 释放 CPU
  → 转发模型请求
  → 回复达到可交付点 → ready → Resume 重新准入
  → 原生 Resume ACK + 新鲜租约确认
  → delivery_ready → Agent 收到回复
```

普通缓冲回复等待 body EOF，SSE 等待首个非空 chunk，不使用提前到达的 HTTP headers 作为恢复触发。组内首个 ready 回复请求恢复，后续 ready 回复不再暂停已经处理回复的 guest。

`InferenceWaitKey` 绑定 lease、单调 wait revision 和授权 call ID；意图为 `begin`、`ready`、`observe`。每个 task 保存一个当前 wait 和最多四个自动控制回执，自动控制不消耗 4096 项手工控制历史，但共用单调命令 revision。低频转换仍占用日志，有限内存历史不等于 WAL 压缩。

如果 Ready 先于不确定的 Begin，到达的 Ready tombstone 阻止迟到 Begin 再暂停。未 issued 的 Pause 可以取消；已 issued 则先收敛其 ACK，再配对 Resume。重启保留意图，但 `observe` 及其他推理等待进展都要求新的 Worker 租约报告。

手工 pause/offload/resume/checkpoint/suspend 撤销自动暂停所有权，自动 wait 不覆盖人工决策。取消、原生结束和最后一个等待成员退出触发受执行生命周期限制的清理；终态交付租约不能无限延长推理清理。`pvisor-inference-` request-ID 前缀为内部保留命名空间。

## 环境、检查点与恢复 {#checkpoint}

环境模板由架构和有序、版本固定的基础/工作区/工具链层组成，以不可变 digest 注册。assignment 携带固定 revision handle；Worker 检查支持能力，用本地或 S3 lazy-cache 读取只读层并创建独立 upper。共享下层不共享可写工作区。

完整执行检查点与 offload 文件有不同语义。检查点封存 CPU、RAM、设备与完整 owned-overlay inventory，验证原始 inode 身份、打开句柄与目录 cookie 等恢复条件；继续执行和 fork 为新的 Run/Attempt 建立 lineage。缺失、被篡改或不兼容的快照拒绝恢复。

可选 FS/S3 仓库发布不可变完整检查点及 manifest，Worker 验证并导入，再以独立可写树和私有 COW RAM 恢复。共享已验证只读 lower 与 RAM 帧减少重复内容；稀疏/压缩存储和原生 v5 下层池的细节由执行器负责。

节点兼容性包括执行协议、架构、原生格式/配置与运行环境；指定仓库或拿到 manifest 并不自动证明任意主机兼容。当前已有同主机独立 Worker 冷导入/源删除后的恢复门槛，跨主机运行时兼容与恢复仍需独立验证。

## 封存分叉与运行中分叉 {#fork}

| 模式 | 操作顺序 | 故障边界 |
| --- | --- | --- |
| sealed fork | 验证源完整检查点与 request → 单事务创建分叉回执和全部分支任务 → 分别准入恢复 | 创建幂等不代表分支执行成功 |
| live fork | 单事务保存请求、capture control 和 WaitingCheckpoint 分支 → 原 Worker 捕获并 ACK → 原子绑定分支检查点和创建回执 → 分支排队 | 源结束、取消、租约失效或 capture 失败会结束尚待检查点的分支 |

一个请求允许 1–64 个分支、4 MiB 事务和 task-retention 限额。分支使用独立 Task/Run/Attempt 身份，保持 lineage；每个分支分别计费资源，不能把共享 RAM 当作零预算。live fork 的源可以继续运行，捕获请求及 ACK 以 source/key/revision 绑定，重试不会重复创建分支。

检查点提交、分支建立和冷恢复是不同提交点。跨仓库发布中断由 Worker 终态交付重试处理；创建回执与 snapshot publication 都不能代表原生 restore 已成功。

## 生命周期与集成边界 {#integration}

普通 Job 的检查点 CLI 路径与 Cluster 原生执行恢复不是同一条产品接口，不能用 Cluster 支持推断普通 Job 的全部 fork/restore 已接通。RL 上层还需保存 rollout/scaffold、模型版本和奖励状态，协调 GPU 调度与环境恢复；Cluster 不替其定义训练事务。

实现位置：共享控制类型在 `pvisor-core/src/cluster.rs`；手工控制和分叉在 `scheduler.rs`；自动 wait 在 `scheduler/inference.rs`；原生执行、Gateway lifecycle、环境和快照接入在 Worker 的 `bin/worker/` 模块。验证范围见[验证矩阵](operations.md#validation)。
