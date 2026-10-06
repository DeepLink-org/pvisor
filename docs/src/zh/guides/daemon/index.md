# 运行单机 daemon

安装 `pvisor-daemon`，通过部分兼容 OpenSandbox 1.1.0 的 API 管理一台 Linux 主机上的镜像沙箱。daemon 负责本机准入、生命周期、持久状态与过期清理，不跨节点调度业务任务，不提供全局 DAG、分布式 lease、Controller 或 Worker。跨节点编排交给 Kubernetes、Ray 或你的应用。

| 需求 | 指南 |
| --- | --- |
| 安装并启动 API | 下方命令 |
| 准备镜像与选择执行边界 | [运行时与集成边界](boundaries.md) |
| 查询、删除与恢复沙箱 | [运维](operations.md) |
| 单独管理原生 node/cache/pool 服务 | [Service 入口](service.md) |

## 前提条件 {#prerequisites}

使用 Linux、rootless Podman、cgroup v2，以及已委派的 CPU、memory、PID controllers。daemon 要求可信的 Podman 可执行文件绝对路径，在绑定 API 前检查运行时；不满足条件会失败，不退化为宿主执行或无资源限制的容器。

可工作的沙箱还需要**预先准备在本机的镜像**，包含真实 OpenSandbox 1.1.0 execd 和 egress 服务。daemon 不拉取镜像。普通发行版镜像、只运行 sleep 的容器或一个 upstream 镜像名称，都不满足这个契约。

!!! warning
    固定版本的 upstream 默认 egress 组件会安装 iptables 重定向，不能原样运行在此后端的 `cap-drop=ALL` 下。当前没有经过端到端验证的 prepared-image 配方。你可以安装并启动生命周期 API，但在准备真实的无 capability execd/egress 部署前，不应期待普通 SDK `Sandbox.create()` 通过就绪检查。要求见[镜像契约](boundaries.md#images)。

## 安装可执行文件 {#install}

在准备部署的 revision checkout 中，先安装 Rust/Cargo，再执行：

```bash
cargo install --locked --path crates/pvisor-daemon --bin pvisor-daemon
pvisor-daemon --help
pvisor-daemon protocol
```

`protocol` 打印固定的 OpenSandbox 版本与 commit，不代表完整 SDK 兼容认证。源码安装与 Python `pvisor` 包安装是两件事；不要假设已有 wheel 包含新的 daemon 伴随程序或 prepared image。源码 revision、SDK 1.1.0 与镜像内容应一起固定。

## 启动 API {#start}

选择 checkout 外的私有持久状态目录，以拥有已准备 Podman 镜像的非 root 账户运行。密钥只生成一次，保存在服务受保护的秘密存储中，重启时复用同一值：

```bash
export OPEN_SANDBOX_API_KEY="$(openssl rand -hex 32)"
pvisor-daemon serve \
  --podman /usr/bin/podman \
  --listen 127.0.0.1:8080 \
  --state "$HOME/.local/state/pvisor/daemon" \
  --max-sandboxes 32 \
  --cpu-millis 4000 \
  --memory-bytes 8589934592 \
  --max-timeout-seconds 86400
```

如果可信 Podman 安装在其他位置，替换 `/usr/bin/podman` 的绝对路径。前台进程通过运行时检查后报告监听地址。这里最多准入 32 个沙箱、四个 CPU 单位及合计 8 GiB 的硬内存限制，不等价于整机 8 GiB 物理内存上限。为 daemon、辅助进程与缓存留出余量，并另行配置宿主监督。

在第二个 shell 中使用同一份受保护 API key 查询生命周期 API：

```bash
curl --fail-with-body --config - <<EOF
url = "http://127.0.0.1:8080/v1/sandboxes"
header = "OPEN-SANDBOX-API-KEY: ${OPEN_SANDBOX_API_KEY}"
EOF
```

预期返回 JSON 沙箱列表；全新状态下列表为空。这只验证 API 访问，不验证沙箱创建、原生资源强制或 SDK 数据面就绪。不要公开密钥，也不要把带凭据的日志贴进 issue。

## 连接客户端 {#clients}

使用 OpenSandbox SDK 1.1.0，配置选定的 domain、protocol 与生命周期 API key。创建请求需要 `image`、`entrypoint` argv，以及恰好包含 `cpu`、`memory` 的 `resourceLimits`。可选 `timeout` 以秒计，至少 60 秒；省略或 null 表示手动清理。这里不提供 prepared-image 名称，因为目前没有经过验证的开箱即用镜像配方。

daemon 检查真实 execd 的 `/ping`、`/ready` 和 egress 的 `/healthz` 后才把创建视为就绪。`202` 是生命周期响应，不是工作负载退出结果。命令、文件与 metrics 流量发往镜像中的真实服务，不由 pVisor 仿造 execd。

默认只监听 loopback。对外访问前，在可信反向代理上配置 TLS，并用 `--public-endpoint HOST:PORT` 指定外部路由可达的 authority，不带 scheme 或路径。通配监听与开发用的零端口绑定也要求显式正确的 public endpoint。认证见[端点认证](operations.md#endpoints)。

## 从 Cluster 迁移 {#migration}

旧 Cluster 任务/客户端 SDK、Controller/Worker 注册、放置、DAG、lease 续期、完成 outbox 和 artifact 退役命令都不是 daemon 接口。不要把旧 task JSON、`worker.toml`、Controller 凭据或 journal 作为 daemon 输入。分布式任务历史没有自动转成沙箱状态的路径。

退役旧部署前保留所需结果。用全新状态目录启动 daemon，把调用方迁移到受支持的 OpenSandbox 生命周期接口。原生 `pvisor run`、VM 执行、本机 checkpoint/fork 及 node/cache/memory-pool 服务独立保留，并未接入此 daemon 后端。清理与重启语义见[运维](operations.md)。
