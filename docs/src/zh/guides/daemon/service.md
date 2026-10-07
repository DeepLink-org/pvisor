# Daemon 与独立缓存入口

直接运行 `pvisor-daemon` 提供 OpenSandbox 生命周期 API 及可选内存池。独立使用 `pvisor-cache` 发布和访问 OCI 缓存。Sandbox supervisor 嵌入原生运行时；node 资源协议仍是独立运行时设施。

## 直接启动 daemon {#daemon}

按[安装与启动步骤](index.md#install)安装后，调用独立可执行文件：

```bash
pvisor-daemon protocol
pvisor-daemon serve --help
```

`OPEN_SANDBOX_API_KEY` 配置生命周期认证。监听地址、public endpoint、持久状态和准入预算是 daemon 选项。Daemon 不读取原生 `RunConfig`、`RunSpec` 或已移除的 service 角色 TOML。Daemon 状态与原生 Job 和缓存存储分开保存。

VM-only supervisor 跨 API 重启保留 RunHandle。Pause/resume 使用同一 Attempt 上已确认的 live vCPU 控制，不是 cgroup freeze 或快照。为 `serve` 添加 `--memory-pool`，启用默认关闭的 daemon 自有池。Daemon 在私有状态目录下启动或复用独立的 `pvisor-daemon memory-pool --directory DIR` 组件；组件需要已持久化的池配置。见[池启用与预算](index.md#memory-pool)。

## 独立缓存与 node 运行时 {#native}

```bash
pvisor-cache --help
pvisor-daemon memory-pool --help
```

`pvisor-cache prepare/publish/serve/list/stat/read` 保持为独立命令，不是 daemon 子命令。发布、后端、认证和访问见[共享镜像缓存参考](../../reference/shared-image-cache.md)。

旧 CLI service supervisor、node/pool 角色和独立池二进制已移除。Node 运行时仍拥有不可变环境挂载与 snapshot RAM，保留授权 store、兼容性检查和连接 pin。这些协议未迁入 daemon：没有自动 node acquire/release 适配器、RAM restore 或 template API。嵌入式原生调用方仍须显式管理所有权；先停使用方，再停 backing owner。

## 宿主监督与外部访问 {#supervision}

Daemon 和缓存以各自所属宿主账户运行，使用私有状态和受保护凭据。宿主重启策略和资源上限与 sandbox 准入累计分别配置。独立池支持跨 API 重启存活，不支持池进程故障或宿主重启恢复。池丢失会使依赖 VM 失败；替换池状态前先排空它们。池位于单个 sandbox cgroup 之外，因此须为页、索引和进程开销预留宿主余量。

Daemon sandbox 执行要求 Linux x86_64、可用 `/dev/kvm`、可信绝对路径，以及已委派、启用 CPU/memory/PID controller 和 `cgroup.kill` 的 cgroup v2。没有 host、OCI 命令或 registry-pull 降级。对外访问使用 TLS 和正确的 `--public-endpoint HOST:PORT`。Daemon API key 不授权独立缓存或 node 运行时 IPC。

历史 Controller/Worker 和 service supervisor 验收记录保留原部署范围，不验证当前 daemon。见[运行时边界](boundaries.md)与[运维](operations.md)。
