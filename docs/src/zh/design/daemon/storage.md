# Daemon 存储与回收

在有界本机 registry 中保留 sandbox 持久意图。原生运行时持有逐 sandbox 私有 run 状态，预制服务拥有工作负载文件与流式输出。这些存储不是集中式 pVisor 证据仓库。

## 私有 Registry {#metadata}

```text
状态目录（0700）
  daemon.lock       进程独占归属
  sandboxes.json    版本、owner UUID、sandbox 记录（0600）
  .registry-*       临时 checkpoint 发布
```

Store 拒绝符号链接状态目录，Unix 上打开 lock/registry 文件不跟随符号链接。Registry 必须为最多 16 MiB 的普通文件，使用支持的版本 1，含有效 owner/sandbox 身份、正资源量与端点凭据。

记录保留镜像/argv、环境、metadata、CPU/内存预留、创建／过期时间、最近观察状态和 sandbox 端点 token。环境秘密进入持久状态，须保护目录及备份。它们不进入 supervisor argv 或宿主环境，但可信宿主／运行时所有者仍可访问。

运行时状态另有私有 `owner.json`，逐 sandbox 的 `preparing.json`/`identity.json`、`run.json`（generation/Run/Attempt ID）、`observation.json`、生命周期 marker、owner 锁、`control.sock`、`ports/` bridge 与 `run/` 原生存储（含 `live-ram` backing 和作为 supervisor `TMPDIR` 的 `tmp/` scratch）；清理完成后只保留最小 tombstone／锁／marker 屏障，用于对账／清理，不是公开 stage/checkpoint/artifact API。

## 提交屏障 {#commit}

每次修改复制当前有界 registry，应用变化并序列化完整 checkpoint。创建私有唯一临时文件，写入并同步，再 rename 到 `sandboxes.json`，最后同步父目录。只有 save 成功后才替换内存 registry。

Rename 可能在后续 sync 报错前已经提交。因此 daemon 标记存储失败并拒绝后续 registry 访问，不用旧内存覆盖不确定持久状态。修复与重启决定实际存活 checkpoint。

不同 sandbox 的 registry 更新串行，但慢原生 VM 操作与就绪检查不持有全局运行时 mutex。Checkpoint 成本随 registry 保留大小增长，尚未测量。容量和 16 MiB 限制约束记录，不保留百万任务历史。Daemon 路径没有追加式任务 journal、completion outbox 或 artifact CAS。

## 移除与保留 {#gc}

原生移除前先持久化删除意图。只有确认原生对象不存在，才能持久移除记录并释放 CPU/内存/数量记账。不能为腾容量而随意驱逐待删除、不确定创建或原生缺失的 Failed 记录。

清理独立于启动资源，校验持久 owner/ID/generation 与 boot/cgroup 绑定：不要求原 rootfs 或 firmware 目录仍存在，也不要求 guest argv/env/资源设置通过启动校验。证明原生对象不存在并取得 owner 锁后，持久发布 `tombstone.json`，删除空的所属 cgroup，回收私有 run 存储、live RAM backing、临时 overlay/spec、socket 与含秘密的 identity/observation 记录。最小目录保留 tombstone、owner 锁和生命周期 marker，阻止 ID 复用。中断的回收从 tombstone 继续，包含 cgroup 已移除的情况；报错保留预留。同一 boot 下没有此持久证据的 cgroup 丢失／被替换仍不授权不存在结论。

删除 sandbox 不撤销远端副作用，也不提供保留证据包。Sandbox 清理不删除外部预制的镜像 manifest/rootfs/firmware。Daemon API 没有 stage review/apply、产物保留／下载或外部对象 GC 协议。不要通过独立删除原生资源或 registry 状态进行恢复。

原生不可变缓存／快照 store 保留各自的发布、完整性、pin 与 GC 合同，见[共享镜像存储](../shared-image-cache-storage.md)与[职责收敛](responsibility-convergence.md#authority)。Daemon 到 node 存储的桥接尚未实现。

实现：`daemon/store.rs` 拥有 checkpoint 发布，`daemon/mod.rs` 拥有预留与删除顺序。
