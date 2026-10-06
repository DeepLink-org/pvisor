# Daemon 与原生 Service 入口

独立运行 `pvisor-daemon` 提供 OpenSandbox 生命周期 API。原生 node、镜像缓存和内存池服务仍服务于使用它们的原生 pVisor Job。Sandbox supervisor 嵌入原生运行时，但 API 状态与 node 资源所有权仍独立。

## 直接启动 daemon {#daemon}

按[安装与启动步骤](index.md#install)安装后，调用独立可执行文件：

```bash
pvisor-daemon protocol
pvisor-daemon serve --help
```

`OPEN_SANDBOX_API_KEY` 配置生命周期认证。监听地址、public endpoint、持久状态与准入预算是 daemon 选项；`RunConfig`、`RunSpec`、原生 service TOML 和旧 Worker profile 都不是 daemon 配置。daemon 状态与原生 Job/node/cache/pool 状态分开保存。

VM-only supervisor 嵌入 pVisor，跨 daemon 重启保留 RunHandle。Pause/resume 使用同一 Attempt 上已确认的 live vCPU 控制，不是 cgroup freeze 或快照。不自动获取 node owner／冷页池。Cargo 与可执行入口已接入原生运行时构造和隐藏 supervisor 派发；同步内部 VM 派发先于 Tokio。见[启动](index.md#start)。

## 原生 node、cache 与 memory pool {#native}

原生服务工具保留：

```bash
pvisor service --help
pvisor service daemon --help
pvisor service cache --help
pvisor service memory-pool --help
```

原生 supervisor 的 `run/status/restart/stop --config FILE` 管理配置中的 `node` 与可选 `pool` 角色。其 TOML 接受 `state`、`node`、`pool`、`cgroup_root` 和按角色的 `limits`，没有 daemon 角色，旧 Controller/Worker 字段被拒绝。node resource service 为原生调用方持有共享不可变环境/RAM backing，不是替代 Controller，也不是全局调度器。配置时保留授权 snapshot roots、pin 所有权与资源预算。原生使用方须显式采用相应 node socket/profile，不会自动成为 daemon 集成。

OCI cache 发布与访问见[共享镜像缓存参考](../../reference/shared-image-cache.md)。实验性 Apple Silicon pool 见[内存共享接入](../../design/memory-optimization/proof-of-concept.md#v1-integration)。依赖 VM 持有 pin 时保持 owner/pool 存活；owner 丢失可能导致后续 fault 失败，停止 pool 可能使依赖 VM 失败。先停使用方，再停数据 owner，不要为强制退出 supervisor 而杀 owner。

带有新 CLI 接线且同目录安装了 `pvisor-daemon` 的构建通过 `pvisor service daemon` 提供伴随入口。以该构建的 `pvisor service --help` 为准；只安装独立 daemon 不会给旧 CLI 增加子命令。部署仍可使用上方独立入口。不要将已退役的 `service cluster` / `service worker`、`[controller]` / `[[workers]]` 配置或 Cluster token 用于新 daemon。

## 宿主监督与外部访问 {#supervision}

每个服务以其所属宿主账户运行，使用私有持久状态和受保护凭据。使用伴随派发时，把可信 companion 与匹配的原生 CLI 安装在一起，不假设 PATH 查找或 wheel 自带。服务重启策略和宿主资源上限与沙箱准入累计分别配置。

要求 Linux x86_64、可用 `/dev/kvm`、可信绝对路径，以及可写、已委派且启用 CPU/memory/PID controller 与 `cgroup.kill` 的 cgroup v2 层级。前置检查验证真实 controller 写入和 KVM API；没有 host、OCI 命令或 registry-pull 降级。 对外访问使用 TLS 和正确的 `--public-endpoint HOST:PORT`。Daemon key 不授权 node/cache/pool 服务。

旧 Controller/Worker 重启、drain、注册与 VM 共享验收记录描述的是已退役部署，不验证此 daemon 或新 companion 接线。当前支持范围见[运行时边界](boundaries.md)与[运维](operations.md)。
