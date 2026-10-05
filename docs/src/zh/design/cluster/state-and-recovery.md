# 状态权威、租约与重启对账

Controller 的运行态是 Worker 报告形成的派生视图。低频日志保存已接受意图和结果身份，让未分配任务、控制请求和终态回执能够跨重启存活；心跳期限、节点压力和 Running 确认在内存中更新。

## 状态分类与一致性 {#authority}

| 状态 | 权威来源 | 持久化与重启行为 |
| --- | --- | --- |
| TaskSpec、DAG、环境模板、分叉请求 | 已接受的不可变意图 | 事务日志重放；尚未分配任务仍在队列 |
| 分配 key、generation、控制/取消意图、drain | Controller 已确认决策 | 低频持久化；重启不能改写执行身份 |
| 终态、原生结果、产物回执、证据退休 | 已接受的 Worker 证据或显式管理决策 | 持久化；相同身份可重读/重试 |
| active、deadline、Worker seen、admission、Running ACK | 最新 Worker 报告 | 内存更新；重启等待新报告 |
| ready、expiry、phase counts、租户/Worker 预留、引用索引 | 任务与引用记录的派生索引 | 重放重建，再经 Worker 对账修正 |
| 实际进程、VM、资源控制、节点缓存 | Worker 与原生运行时 | Controller 记录不能替代现场观察 |
| 待交付终态和控制观察 | Worker 本地持久化 outbox/证据 | Worker 重启后精确身份恢复与重试 |

“最终一致性”适用于 Controller 对实际执行的认识。创建任务、分配和回执仍要求本地提交屏障，避免返回成功后遗失排队任务或换掉执行身份。当前没有分布式共识、跨节点同步心跳日志或无状态队列。

Controller 通过 Worker 周期性 `poll` 收敛，不主动扫描任意节点拉取全部状态。收敛条件是原 Worker 可联系、持续报告完整身份清单且本地存储可用；没有给永久分区规定自动收敛期限。

## 身份与租约 {#lease}

`LeaseKey` 的四个字段必须完整匹配：

| 字段 | 作用 |
| --- | --- |
| `task_id` | 不可变提交身份 |
| `worker_id` | 节点逻辑身份 |
| `incarnation` | 一次 Worker 进程实例，重启后使用新值 |
| `generation` | 该任务的分配世代，拒绝旧分配的消息 |

Controller 的 `expires_at_ms` 使用 Unix 毫秒。Worker 用请求开始的单调时间加返回的租约时长设置 watchdog，网络请求等待不会延长本地执行。续租响应太晚不能复活已经过期的运行。停止采用执行器的原生取消路径，可能包含终止宽限时间。

在线且已确认的租约到期后，reaper 持久化结束；未知执行通常为 `Lost`，已知原生结果的交付失败保留结果。取消后未确认停止也不能作为成功。`Lost` 是终态，不能推出外部副作用已结束。

## 任务状态机 {#phases}

| phase | 含义与下一步 |
| --- | --- |
| `waiting_dependencies` | DAG 身份已创建，等待全部前驱聚合成功 |
| `waiting_checkpoint` | live fork 分支已创建，等待匹配的完整封存检查点 |
| `queued` | 等待匹配和资源准入 |
| `leased` | assignment 已提交，尚未被 Worker 活动清单确认 |
| `running` | Worker 已确认执行身份；原生动作通过独立观察描述 |
| `paused` / `offloaded` | 原生暂停/offload 成功已被接受，CPU 预留为零，RAM/槽位仍保留 |
| `suspending` | 完整快照已封存，等待原生终止与交付 |
| `retaining_artifacts` | 原生执行已结束，保留交付租约和有限上传预留 |
| `cancelling` | 取消已接受，等待完成或租约失效 |
| `succeeded` / `failed` / `cancelled` / `lost` / `suspended` | 终态；释放运行预留，继续保存结果和保留证据引用 |

主路径为 `queued → leased → running → succeeded/failed`；控制、取消与交付引入分支。`reconciliation_pending` 是与 phase 正交的标志。重启后即使 phase 显示 `running` 或 `paused`，只要该标志为真，就只能解释为历史状态。

## Controller 重启算法 {#reconcile}

1. 独占打开日志，逐帧验证并重放事务；验证任务保留上限。完整坏帧拒绝启动，只有重放整体成功后才清理不完整末帧。
2. 验证日志与产物仓库的 authority 绑定，重建 ready、计数、资源与保留 manifest pins。
3. 对所有具有租约的非终态任务设置 `reconciliation_pending = true`，放入对账集合，从到期索引移除历史 deadline。资源和引用继续保留。
4. 原 Worker 用相同 incarnation 发送完整 `PollRequest.active`。精确 key 被确认后清除 pending，重建内存 deadline；首次活动确认也可将 `leased` 推进到 `running`。
5. 对未确认的 `leased` assignment，可以向同一 Worker incarnation 重发相同 key。Worker 按活动任务身份去重；完成但尚未收到回执的身份仍必须报告。
6. 等待不可联系的 Worker；不使用旧 deadline 自动回收其预留，不将任务改派到其他 Worker。

`active` 是完整、有界的活动清单，包含终态交付直至 Controller 确认，排除未启动拒绝项。局部清单会让 Controller 误判哪些 assignment 尚未被接收。协议界限为注册槽位数加 64 个交付身份；它不是已结束历史任务的全量上传，也不能重建丢失的 TaskSpec/DAG。

| 待对账期间的请求 | 行为 |
| --- | --- |
| 精确 key 的 poll / 终态 recover | 可重新确认身份并续租 |
| 精确终态、已有 issued control ACK、未启动拒绝 | 可处理历史身份的现场证据；不代表任意新操作获得权限 |
| 新原生控制、遥测、产物上传、推理等待进展 | 先要求新的 Worker 租约确认 |
| Task/Graph/Worker/counts 查询 | 返回派生视图，不触发到期提交；调用方检查 pending |
| 破坏性产物 GC | 只要 shard 仍有待对账租约就拒绝 |

## Worker 重启与终态恢复 {#worker-restart}

Worker 独占本地状态目录。启动时读取绑定 Controller URL 与 Worker ID 的待交付 outbox，先为旧 incarnation 的已知终态调用 `recover`，续租、上传并重投结果，再开始新 incarnation 注册。

这条路径只恢复终态交付，不收养旧进程实例的未知活执行。Controller 重启留下的待对账租约会阻止同一 Worker ID 的替代 incarnation 接管；需要原 Worker 确认或管理方显式处理。若 Controller 未重启且原租约仍按在线时间计时，可按正常到期路径终结旧身份。

已持久化的暂停观察与恢复意图也能重放，但不能单独授权 Gateway 在重启后交付模型回复。推理等待仍要求原 Worker 的新报告。

## 永久失联的显式处理 {#resolve-lost}

管理方从任务记录复制完整 `lease.key`，通过 `POST /v1/tasks/{id}/resolve-lost` 或 `pvisor-cluster resolve-lost key.json` 处理待对账执行。

- 未知原生结果标记为 `Lost`。
- 已知原生结果保留，放弃产物交付后聚合为 `Failed`；已在取消中则为 `Cancelled`。
- 释放 Controller 的运行预留和活动引用，不自动重试；已保留的证据按其保留策略处理。
- 相同完整 key 的终态重读幂等；旧 generation/incarnation 拒绝。

该决定封闭控制面身份，不能远程证明进程、VM 或外部副作用已经停止。提交替代任务前，运维和业务方应核对失联节点及外部效果。任务重试使用新 Task/Run/Attempt 身份，保留旧结果。

## 兼容与实现位置 {#implementation}

旧日志的 `Renew` 帧仍可读，作为历史提示重放后进入上述对账。当前写入路径过滤所有 `Renew`，已移除 `--durable-leases` 和 `Scheduler::open_durable`；没有需要维护的双写模式。

核心实现为 `Scheduler::open`、`reported_key` / `valid_key`、`poll` / `recover`、`resolve_lost`、`maintain_expiry`。`reported_key` 接受待对账的精确非终态身份，`valid_key` 进一步要求已确认归属。Worker 的 watchdog 和 outbox 分别位于 `pvisor-worker.rs` 与 `bin/worker/outbox.rs`。
