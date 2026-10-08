# Daemon 存储与回收

在有界本机 registry 中保留 sandbox 持久意图。原生运行时持有逐 sandbox 私有 run 状态，预制服务拥有工作负载文件与流式输出。这些存储不是集中式 pVisor 证据仓库。

## 私有 Registry {#metadata}

```text
状态目录（0700）
  daemon.lock       进程独占归属
  sandboxes.json    v2 header：版本、owner UUID、空 sandboxes（0600）
  records/          私有逐 sandbox registry（0700）
    meta.json       版本 2 与匹配的 owner UUID（0600）
    sb-<uuid>.json  带 owner 信封的 sandbox 记录（0600）
    .record-*       未提交的替换临时文件
  .records-*        迁移暂存目录
  .records-retired-* 可丢弃的未激活记录目录
```

Store 拒绝符号链接状态／记录目录，Unix 上打开 lock/registry 文件不跟随符号链接。版本 2 只从已提交的 `records/` 树加载记录；header、metadata 与每条记录必须有相同 owner。Sandbox ID、正资源量与端点凭据均要校验。已有记录树但 header 缺失、记录损坏或 metadata 缺失／不匹配时 fail closed，不回退到旧快照。

总计 16 MiB 预算衡量逻辑上的版本 1 等价快照：一份 owner/header 加上 sandbox key 与序列化记录内容，不计逐文件重复的 owner 信封。它不是物理目录大小上限；原本恰好达到上限的有效 v1 快照仍可迁移。每个 JSON 文件也必须是大小有界的普通文件。

记录保留镜像/argv、环境、metadata、CPU/内存预留、创建／过期时间、最近观察状态和 sandbox 端点 token。环境秘密进入持久状态，须保护目录及备份。它们不进入 supervisor argv 或宿主环境，但可信宿主／运行时所有者仍可访问。

运行时状态另有私有 `owner.json`，逐 sandbox 的 `preparing.json`/`identity.json`、`run.json`（generation/Run/Attempt ID）、生命周期 marker、owner 锁、`control.sock`、`ports/` bridge 与 `run/` 原生存储（含 `live-ram` backing 和作为 supervisor `TMPDIR` 的 `tmp/` scratch）；清理完成后只保留最小 tombstone／锁／marker 屏障，用于对账／清理，不是公开 stage/checkpoint/artifact API。

原生运行时不写入 `observation.json` 缓存，也不以其作为存活证据。端点查询认证 live supervisor，并检查当前 Running 状态与删除屏障；完整就绪检查仍保留在 create、Inspect 和 resume。见[服务访问](lifecycle.md#endpoints)。

## 提交屏障 {#commit}

每次修改只克隆／序列化目标 sandbox 记录，不处理整个 registry。替换先写入并 fsync 私有唯一 `.record-*` 文件，再原子 rename 为 `records/sb-<uuid>.json`，最后 fsync `records/`。移除会 unlink 对应记录并 fsync 目录。普通修改不重写 `sandboxes.json` 或 `meta.json`；持久化成功后才更新内存中的对应条目。

独立 commit 锁串行化准入决策与持久修改。Registry 读锁在阻塞线程池执行磁盘 I/O 前释放，因此写入期间读者仍可观察最近已提交的清单。慢原生 VM 操作与就绪检查仍不持有全局运行时 mutex。

Rename/unlink 可能在后续 fsync 报错前已经提交。Storage-failed 标记的行为不变：拒绝后续 registry 访问，不用旧内存覆盖不确定磁盘状态；保留状态、修复存储、重启对账。容量和逻辑 16 MiB 上限约束保留记录，不保留百万任务历史。增量持久化已实现，但性能尚未测量。此路径没有追加式任务 journal、completion outbox 或 artifact CAS。

## 版本 1 迁移 {#migration}

激活前，v1 `sandboxes.json` 始终是权威完整快照。只有运行时 factory 接受其持久 owner 后，Store 才初始化 v2 布局：在私有 `.records-*` 暂存中写入并 fsync owner metadata 与记录，同步暂存目录，rename 为 `records/` 并同步状态目录，最后原子替换／fsync 根 header，使其为版本 2 与空 `sandboxes` map。此 header 替换激活已提交的记录树；原生前置检查在初始化之后执行。

中断的暂存或未激活的 `records/` 副本不能覆盖完好的 v1 快照。保留名称的迁移残留会校验私有归属、名称和普通文件类型，不要求不完整载荷可反序列化，也不跟随符号链接。未激活记录可 rename 为 `.records-retired-*`，与暂存残留一起回收。v2 header 激活后，记录／metadata 必不可少，不回退到 v1。激活结果不确定时，本次 open 失败，下次 open 读取实际保留的 header。

NativeRuntime 在迁移前拒绝缺少 `owner.json` 的非空 v1 状态；任何缺少原生 marker 的 v2 header 也会被拒绝，即使 header map 为空。保留原始归属状态，不要删除记录／预留或伪造 marker 绕过检查。

## 移除与保留 {#gc}

原生移除前先持久化删除意图。只有确认原生对象不存在，才能持久移除记录并释放 CPU/内存/数量记账。不能为腾容量而随意驱逐待删除、不确定创建或原生缺失的 Failed 记录。

清理独立于启动资源，校验持久 owner/ID/generation 与 boot/cgroup 绑定：不要求原 rootfs 或 firmware 目录仍存在，也不要求 guest argv/env/资源设置通过启动校验。证明原生对象不存在并取得 owner 锁后，持久发布 `tombstone.json`，删除空的所属 cgroup，回收私有 run 存储、live RAM backing、临时 overlay/spec、socket 与含秘密的 identity 记录及可能遗留的 observation 缓存。最小目录保留 tombstone、owner 锁和生命周期 marker，阻止 ID 复用。中断的回收从 tombstone 继续，包含 cgroup 已移除的情况；报错保留预留。同一 boot 下没有此持久证据的 cgroup 丢失／被替换仍不授权不存在结论。

删除 sandbox 不撤销远端副作用，也不提供保留证据包。Sandbox 清理不删除外部预制的镜像 manifest/rootfs/firmware。Daemon API 没有 stage review/apply、产物保留／下载或外部对象 GC 协议。不要通过独立删除原生资源或 registry 状态进行恢复。

原生不可变缓存／快照 store 保留各自的发布、完整性、pin 与 GC 合同，见[共享镜像存储](../shared-image-cache-storage.md)与[职责收敛](responsibility-convergence.md#authority)。Daemon 到 node 存储的桥接尚未实现。

实现：`daemon/store.rs` 拥有逐记录发布、预算记账与 v1 激活，`daemon/mod.rs` 拥有预留与删除顺序。
