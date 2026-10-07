# 单节点 Daemon 架构

通过 `pvisor-daemon` 管理本机 sandbox：一个进程接受有资源边界的创建请求，持久化归属与意图，控制原生运行时，清理过期 sandbox，并转发对预制服务的访问。外部编排选择主机并协调业务工作，不是 pVisor 产品控制面。

## 职责划分 {#architecture}

```text
调用方／外部编排
  → OpenSandbox 生命周期 HTTP API
  → daemon：本机准入 + 持久 registry + 每 sandbox 生命周期锁
  → NativeRuntime → 嵌入 pvisor 的独立 supervisor → 仅 VM
  → 预制镜像：工作负载 + 真实 execd + 带 guest CID 3 vsock bridge 的 egress 服务
调用方 → daemon 端点代理 → 预制服务
```

| 所有者 | 职责 | 不负责 |
| --- | --- | --- |
| 调用方 | 工作负载、预制镜像、输入版本、重试与业务副作用 | 从超时推断执行没有发生 |
| Daemon | 本机准入、归属、意图、运行时对账、TTL、鉴权端点 | 跨主机调度或训练事务 |
| 原生 supervisor | 嵌入 pVisor/VM、RunHandle、已确认 vCPU 控制、cgroup 身份、vsock bridge | Stage/apply/checkpoint API 或自动 node 共享 |
| 预制镜像 | 监督 argv，初始化并鉴权 execd/egress | 替代生命周期 API 授权 |
| 原生 pVisor | Supervisor 嵌入 VM 执行；公开 Job 工作流仍独立 | 自动暴露 stage/checkpoint API |
| 原生 node 运行时 | 独立不可变 backing 协议 | 自动 daemon 获取／共享 |
| Daemon 池组件 | `serve --memory-pool` 启用的可选独立池 | 池进程／宿主重启恢复；node acquire/release |
| 独立 `pvisor-cache` | OCI prepare/publish/serve/list/stat/read | Daemon 生命周期所有权 |

Runtime trait 只有一个实现：VM-only NativeRuntime。独立 supervisor 嵌入 `pvisor::PVisor`，只配置 VmExecutor，跨 daemon 重启保留 RunHandle。没有 host/OCI/pull 降级。可执行程序的 `serve` 命令使用必需的 `--images-dir`/`--cgroup-root` 构造 NativeRuntime；同步内部 VM 派发先于 Tokio，隐藏 supervisor 命令负责 supervisor 派发。见[运维](operations.md#deployment)。

## 创建与访问 {#task-flow}

1. 校验镜像、argv、环境、metadata、CPU/内存硬限制与可选 TTL。在准入前拒绝不支持的控制。
2. 串行更新 registry 时检查本机容量，持久写入随机 `sb-*` 身份、预留与 `Pending` 记录。
3. 持久化原生 preparation/identity，安装身份绑定的 cgroup 限制，在 exec 前将子进程放入该 cgroup，启动独立 supervisor，验证已确认的 live VM 控制与真实服务就绪。
4. 原生创建成功后才持久化 `Running`。失败创建的清理已确认时释放记录；清理不确定时保留身份与预留。
5. 通过 daemon 解析支持的服务端点。查询对账原生状态；删除先持久化意图，确认原生对象不存在后才释放容量。

这是 sandbox 管理，不是任务／结果协议。私有运行时记录将 generation 绑定到原生 Run/Attempt ID，但 `sb-*` 不是公开 Job ID，API 不暴露 Job review、checkpoint 或 Run Bundle 导出。

## 并发与故障边界 {#documents}

[Registry commit 锁](storage.md#commit)串行化容量检查与持久修改；每个 sandbox 的生命周期锁排序控制操作和代理建连。慢 VM 操作不持有全局运行时 mutex，让无关 sandbox 继续推进，同时防止两个创建请求重复消费预留，或建连与 daemon 管理的删除发生竞争。

持久意图和 live 观察具有不同权威。原生删除前先持久化 `Stopping`，确认对象不存在后才释放容量。IPC 或确认回复丢失时保留待对账工作，不能据此启动替代 VM 或复用容量。[只重启 daemon](state-and-recovery.md#reconcile)会使用相同归属状态重连仍存活的 supervisor；宿主重启则丢失 live VM。

保守预留用利用率换取明确的清理记账：Paused、Failed 和不确定记录在确认移除前持续计费。低 RSS 与原生缓存共享不会减少预留；[准入](admission.md#reservations)和物理工作集记账仍然独立。

## 兼容性与证据 {#invariants}

部分 API profile 固定为 **OpenSandbox 1.1.0**、`release-1.1.0`、commit `b1a29cf93a823a95913f7943010febb3f29de05c`。这不代表完整 API 或未修改 SDK 的端到端兼容。预制 execd/egress 镜像合同尚无已验证的端到端配方，见[运维](operations.md#image-contract)。

运行时已接入原生 VM 执行。Stage/apply、checkpoint/fork 和 offload API、Gateway 推理等待协调与自动 node 资源获取未实现。旧 Cluster 结果不验证 daemon SDK 兼容、性能或密度，也没有全节点物理内存收益证据。

实现归属：`crates/pvisor-daemon/src/daemon/{models,store,mod,api}.rs`、`runtime.rs` 和 `main.rs`。已退役 Cluster 实现不属于该架构。
