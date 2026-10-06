# Daemon 存储与回收

在有界本机 registry 中保留 sandbox 持久意图。Podman 拥有容器存储，预制服务拥有工作负载文件与流式输出。这些存储不是集中式 pVisor 证据仓库。

## 私有 Registry {#metadata}

```text
状态目录（0700）
  daemon.lock       进程独占归属
  sandboxes.json    版本、owner UUID、sandbox 记录（0600）
  .registry-*       临时 checkpoint 发布
```

Store 拒绝符号链接状态目录，Unix 上打开 lock/registry 文件不跟随符号链接。Registry 必须为最多 16 MiB 的普通文件，使用支持的版本 1，含有效 owner/sandbox 身份、正资源量与端点凭据。

记录保留镜像/argv、环境、metadata、CPU/内存预留、创建／过期时间、最近观察状态和 sandbox 端点 token。环境秘密进入持久状态，须保护目录及备份。它们不作为值出现在 Podman 参数中，但可信宿主／运行时所有者仍可访问。

## 提交屏障 {#commit}

每次修改复制当前有界 registry，应用变化并序列化完整 checkpoint。创建私有唯一临时文件，写入并同步，再 rename 到 `sandboxes.json`，最后同步父目录。只有 save 成功后才替换内存 registry。

Rename 可能在后续 sync 报错前已经提交。因此 daemon 标记存储失败并拒绝后续 registry 访问，不用旧内存覆盖不确定持久状态。修复与重启决定实际存活 checkpoint。

不同 sandbox 的 registry 更新串行，但慢 Podman 操作与就绪检查不持有全局运行时 mutex。Checkpoint 成本随 registry 保留大小增长，尚未测量。容量和 16 MiB 限制约束记录，不保留百万任务历史。Daemon 路径没有追加式任务 journal、completion outbox 或 artifact CAS。

## 移除与保留 {#gc}

原生移除前先持久化删除意图。只有确认原生对象不存在，才能持久移除记录并释放 CPU/内存/数量记账。不能为腾容量而随意驱逐待删除、不确定创建或原生缺失的 Failed 记录。

删除 sandbox 不撤销远端副作用，也不建立保留证据包。工作负载数据和镜像回收遵循 Podman／预制服务规则；daemon API 没有 stage review/apply、产物保留／下载或对象 GC 协议。不要通过独立删除原生资源或 registry 内容进行常规恢复。

原生不可变缓存／快照 store 保留各自的发布、完整性、pin 与 GC 合同，见[共享镜像存储](../shared-image-cache-storage.md)与[职责收敛](responsibility-convergence.md#authority)。Daemon 到 node 存储的桥接尚未实现。

实现：`daemon/store.rs` 拥有 checkpoint 发布，`daemon/mod.rs` 拥有预留与删除顺序。
