# 元数据、产物与回收

Controller 持久化低频元数据，Worker 持久化待交付终态；大对象用不可变内容引用传递。执行租约、证据保留和下载保护具有各自生命周期，回收必须同时检查这些根。

## Controller 元数据日志 {#metadata}

日志服务于已接受任务、DAG、assignment、环境、控制/取消、分叉、原生结果及最终回执。纯 `Renew` 更新不追加、不消耗日志配额、不 fsync；其历史帧仍可读。这个低频日志不承担每次心跳的强一致复制。

每帧是 `<BLAKE3 checksum> <JSON transaction>\n`，transaction 带版本和一组 changes。默认日志上限 1 GiB，单帧上限 16 MiB。日志以 `0600`、`O_NOFOLLOW` 打开，验证常规文件、独占锁和父目录持久化；完整错误帧拒绝重放，只有整体重放成功后才清理不完整末帧。

HTTP 请求经容量 256 的 Dispatcher 交给一个写线程。写线程收集已经在排队的操作，最多 64 项、4 MiB 已写帧或 2 ms 操作处理时间，组内共享一次 fsync。门槛在操作之间检查，单项不会拆开；空闲请求不人为等待批次。

Scheduler 锁覆盖整组操作及持久化屏障。成功、读取和冲突回复都等屏障成功；外部 GC retirement 与下载资格检查使用同一把锁，不观察未提交状态。同步 Scheduler API 和 GC 回调保留即时提交语义。只读/纯续租组没有新增持久化帧，因此不 fsync。

客户端断开不会撤销已进入队列的请求。若写入/fsync 不确定，整组回复失败、journal poison、Dispatcher 关闭；拒绝后续访问，重启由重放判断幸存事务。不能回滚内存后继续使用一个提交结果未知的写者。

## 满额与 I/O 不确定性的区别 {#quota}

| 故障 | 行为 |
| --- | --- |
| 明确的日志 quota 拒绝 | 操作未发布；poll/recover 可保留已有续租，暂缓新分配/新控制与到期提交 |
| 到期提交被 quota 拒绝 | 保留资源和引用；不能只改内存并假装结束 |
| 只读监控遇到明确 quota 满额 | 读最新派生视图，不触发 reaper；到期可见性是异步的 |
| 首次终态/意图提交遇到满额 | 仍可能失败；Worker 继续保留待交付证据，需恢复元数据容量 |
| 真实 write/fsync 失败、结果不确定 | poison 并返回不可用，要求检查存储后重启 |

没有在线日志压缩或任务身份 GC。增加 quota 前应检查真实磁盘余量；删除 artifact 不会减少元数据日志。不能丢弃日志后仅凭活动 Worker 恢复尚未分配任务、图和终态历史。

## Worker 状态与 outbox {#outbox}

Worker 的 `STATE/tasks/TASK-GENERATION` 保存 assignment、原生执行记录、控制观察、Bundle、trace 和封存 spool。outbox 绑定 Worker ID 与 Controller URL；最多 4096 个待交付记录，每条最大 16 MiB。它是终态交付账本，不保存 Controller 的全量 desired state。

```text
原生结束 / mount 释放 / 本地 spool 封存并持久化
  → 持久化 terminal outbox
  → 可选 native-done（缩减预留）
  → 上传对象与 manifest
  → complete（Controller 验证并持久化）
  → 校验 ACK 身份，保存 durable receipt
  → 删除 pending 项
```

网络超时、丢 ACK 和 Worker 重启通过相同 key 重投。持久化回执先于移除 pending；拒绝过期身份的终结与已接受身份的回执分别保存。ack 后的本地执行目录、spool 和 checkpoint receipt 不会因此自动清理，Worker 本地 GC 仍是后续工作。

## Controller 产物 CAS {#artifacts}

产物目录由 `journal.with_extension("artifacts")` 推导，内容以 BLAKE3 digest 和字节长度引用。小 manifest 指向分块对象，可保留 native Bundle、trace、VM 私有 writable layer 。只上传 Bundle JSON 不会自动上传本地路径指向的文件。

上传绑定完整 key 并建立 durable lease pins；大对象 I/O 在调度锁外执行，关键发布前后重查资格。完成时验证 manifest、对象 hash/length、要求的文件、导出能力和原生 Run/Attempt 身份；不信任 Worker 任意声明的远端路径。

默认旧式 payload 上限为 8 GiB；新的 `ArtifactStorageLimits` 对唯一对象字节数/个数提供持久化、可在线调整的策略。计入共享内容去重、并发发布预留和 orphan uploads。配额不等于每租户空间隔离；Controller CAS 仍是本地单 authority 存储。

日志保存唯一 artifact authority，产物目录与之绑定。备份恢复必须保留匹配的日志和仓库；任意空日志或其他 shard 的日志不能拿来回收已有目录。

## 检查点存储与原子性边界 {#checkpoints}

完整执行检查点保存在源 Worker 的本地 SnapshotStore；Controller artifact CAS 只保存租约绑定的 Bundle、trace 和私有文件变更证据，不分发执行检查点。检查点封存、Controller 控制回执与本地恢复是不同提交点。

重试依赖内容不可变、完整性校验和 outbox；取消或失联时保留已经发生的原生观察。检查点可读并不证明本地运行时兼容，见[检查点生命周期](lifecycle.md#checkpoint)。

## GC 根与计划执行 {#gc}

| 根 | 保护对象与结束条件 |
| --- | --- |
| 活动 assignment 的 durable upload pins | 该 lease 的上传对象；身份正式终结后解除 |
| 保留 terminal manifest | manifest 及传递引用；显式证据退休后解除 |
| 活动下载保护 | 下载引用集；释放或到期后解除 |
| 发布中预留/打开读取保护 | 并发发布、读取的对象；对应操作收敛后解除 |
| 待对账租约或无 pin 协议的历史租约 | 保守阻止破坏性回收，避免漏根 |

GC 使用 preview/plan → apply 两步。plan ID 不可变、5 分钟有效，候选保存 inode/device/length 等现场身份；apply 再校验最新根和文件身份，避免把后来替换或新引用的对象删除。重启后内存 plan 需要重新生成。

可选 terminal evidence retirement 先持久化证据退休，之后才移除对应根并回收无引用内容。被退休任务仍保留元数据与原生结果，下载返回已退休语义；共享对象只有全部根解除才可删。

下载使用显式保护 lease，默认 5 分钟，可续期，最长生命周期 1 小时。客户端应在整个多对象下载期间保持续期并释放保护，不能把一次 manifest GET 视为永久保留。

待对账期间拒绝整个 shard 的 destructive plan/apply，是当前保守策略。它避免资源收敛前的漏根，也意味着一个长期失联 Worker 可以拖延全局回收；后续细化必须先证明根完整性，不能直接删掉这个门槛。

实现位置为 `journal.rs`、`server/dispatcher.rs`、`artifacts.rs`、`artifacts/{quota,gc}.rs` 及 Worker `bin/worker/{outbox,artifacts}.rs`。
