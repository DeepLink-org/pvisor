# 单节点 Daemon 架构

通过 `pvisor-daemon` 管理本机 sandbox：一个进程接受有资源边界的创建请求，持久化归属与意图，控制原生运行时，清理过期 sandbox，并转发对预制服务的访问。外部编排选择主机并协调业务工作，不是 pVisor 产品控制面。

## 职责划分 {#architecture}

```text
调用方／外部编排
  → OpenSandbox 生命周期 HTTP API
  → daemon：本机准入 + 持久 registry + 每 sandbox 生命周期锁
  → Runtime 适配器 → 外部 rootless Podman
  → 预制镜像：工作负载 + 真实 execd + 无 capability 的 egress 服务
调用方 → daemon 端点代理 → 预制服务
```

| 所有者 | 职责 | 不负责 |
| --- | --- | --- |
| 调用方 | 工作负载、预制镜像、输入版本、重试与业务副作用 | 从超时推断执行没有发生 |
| Daemon | 本机准入、归属、意图、运行时对账、TTL、鉴权端点 | 跨主机调度或训练事务 |
| 外部 Podman | 容器执行、cgroup 控制、namespace、原生观察 | pVisor Job 暂存、VM 检查点或模型证据 |
| 预制镜像 | 监督 argv，初始化并鉴权 execd/egress | 替代生命周期 API 授权 |
| 原生 pVisor 执行器与 node 资源 | 独立的 Job/VM 语义与不可变 backing 所有权 | 自动接入该 daemon |

Daemon crate 使用 `Runtime` trait，不依赖已依赖本包的 `pvisor`，避免循环依赖。当前唯一后端是外部 rootless Podman，没有 host 降级或原生 VM 后端。

## 创建与访问 {#task-flow}

1. 校验镜像、argv、环境、metadata、CPU/内存硬限制与可选 TTL。在准入前拒绝不支持的控制。
2. 串行更新 registry 时检查本机容量，持久写入随机 `sb-*` 身份、预留与 `Pending` 记录。
3. 持有该 sandbox 的生命周期锁，创建并启动带标签的容器，核对资源设置和真实服务就绪状态。
4. 原生创建成功后才持久化 `Running`。失败创建的清理已确认时释放记录；清理不确定时保留身份与预留。
5. 通过 daemon 解析支持的服务端点。查询对账原生状态；删除先持久化意图，确认原生对象不存在后才释放容量。

这是 sandbox 管理，不是任务／结果协议。Sandbox ID 不代表 Job/Run/Attempt 身份，也不意味着存在 Run Bundle。

## 设计入口 {#documents}

| 问题 | 设计 |
| --- | --- |
| 本机能准入多少资源？ | [本机准入](admission.md) |
| 控制何时完成？ | [生命周期](lifecycle.md) |
| 重启后保留什么？ | [状态与恢复](state-and-recovery.md) |
| 存储与回收哪些内容？ | [存储](storage.md) |
| 如何部署与排障？ | [运维](operations.md) |
| 不可变共享与按需读属于哪里？ | [共享工作集](shared-working-set.md) |
| 哪些服务应保持独立？ | [职责收敛](responsibility-convergence.md) |

## 兼容性与证据 {#invariants}

部分 API profile 固定为 **OpenSandbox 1.1.0**、`release-1.1.0`、commit `b1a29cf93a823a95913f7943010febb3f29de05c`。这不代表完整 API 或未修改 SDK 的端到端兼容。预制 execd/egress 镜像合同尚无已验证的端到端配方，见[运维](operations.md#image-contract)。

原生 VM、stage/apply、checkpoint/fork、offload、Gateway 推理等待协调和 node 资源获取均未接入该后端。旧 Cluster 测量属于已退役分布式实现，不是 daemon 性能证据。Daemon 密度优势和全节点物理内存收益均未验证。

实现归属：`crates/pvisor-daemon/src/daemon/{models,store,mod,api}.rs`、`runtime.rs` 和 `main.rs`。由 legacy feature 隔离的模块是过渡代码，不属于该架构。
