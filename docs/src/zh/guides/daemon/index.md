# 运行单机 daemon

安装 `pvisor-daemon`，通过部分兼容 OpenSandbox 1.1.0 的 API 管理一台 Linux 主机上的镜像沙箱。daemon 负责本机准入、生命周期、持久状态与过期清理。跨节点任务调度、全局 DAG 和分布式 lease 由 Kubernetes、Ray 或你的应用管理；daemon 不包含 Controller 或 Worker。


## 前提条件 {#prerequisites}

要求 Linux x86_64、可用 `/dev/kvm`、可信绝对路径，以及可写、已委派且启用 CPU/memory/PID controller 与 `cgroup.kill` 的 cgroup v2 层级。前置检查验证真实 controller 写入和 KVM API；没有 host、OCI 命令或 registry-pull 降级。

镜像是可信本机 `images_dir/<key>.json` manifest，不是 registry reference。字段包括绝对路径的独立 Linux `rootfs`（不能是宿主 `/` 或与 daemon 状态重叠）、绝对路径的 guest bootstrap `entrypoint` argv、可选 `cmd`、可选 `env` 与可选绝对路径 firmware `library_dir`。请求的工作负载 argv（为空则使用 `cmd`）追加到 `entrypoint`，请求 env 覆盖 manifest env，不继承宿主环境，也不经 shell 插值。

!!! warning
    需要自行准备 guest bootstrap 与 vsock 服务桥接，具体要求见[镜像契约](boundaries.md#images)。项目**尚未提供 bootstrap 或镜像配方，也未做端到端验证**；目前没有 SDK 兼容性验证或密度测量结果。

## 安装可执行文件 {#install}

按[原生构建前提](../../community/development.md)准备环境，在选定 revision 上运行以下源码安装命令；这条安装路径尚未验证：

```bash
cargo install --locked --path crates/pvisor-daemon --bin pvisor-daemon
pvisor-daemon --help
pvisor-daemon protocol
```

`protocol` 打印固定的 OpenSandbox 版本与 commit。源码安装独立于 Python `pvisor` 包安装；使用 wheel 时，检查该版本是否包含 daemon，并单独准备镜像。源码 revision、SDK 1.1.0 与镜像内容应一起固定；SDK 兼容性仍需端到端验证。

## 启动 API {#start}

`serve` 使用已实现且必需的 `--images-dir` 与 `--cgroup-root` 构造 `NativeRuntime`。Cargo 链接 `pvisor` 与 `pvisor-core`；同步 `main` 在参数解析或 Tokio 之前调用 `pvisor::run_krun_internal_if_requested()`，随后派发隐藏的 `native-supervisor --sandbox-dir ABSOLUTE_PATH` 命令。准备好镜像与 cgroup 层级后，使用以下 CLI 配置启动 API。

```bash
export OPEN_SANDBOX_API_KEY="$(openssl rand -hex 32)"
pvisor-daemon serve \
  --images-dir /srv/pvi \
  --cgroup-root /sys/fs/cgroup/pvd \
  --listen 127.0.0.1:8080 \
  --state /run/user/1000/pvd \
  --max-sandboxes 32 \
  --cpu-millis 4000 \
  --memory-bytes 8589934592 \
  --max-timeout-seconds 86400
```

运行时路径使用绝对路径，daemon 重启时保持不变。状态路径保持短，例如 `/run/user/1000/pvd`：逐 sandbox 的 `control.sock` 必须短于 104 字节，vsock Unix socket 也有路径长度限制。保留状态；此 `/run` 示例不保证跨注销／重启持久化，VM 不能跨宿主重启存活。`/sys/fs/cgroup/pvd` 必须是真实委派层级，不能是普通目录。

选择 checkout 外的私有状态目录。API key 只生成一次，保存在受保护的服务秘密存储中，重启时复用同一值。示例的准入预算是 32 条记录、四个 CPU 单位和总计 8 GiB 硬内存限制。全节点物理占用尚未测量；为 daemon/cache/宿主留余量，另行配置宿主监督。

在第二个 shell 中使用同一份受保护 API key 查询生命周期 API：

```bash
curl --fail-with-body --config - <<EOF
url = "http://127.0.0.1:8080/v1/sandboxes"
header = "OPEN-SANDBOX-API-KEY: ${OPEN_SANDBOX_API_KEY}"
EOF
```

预期返回 JSON 沙箱列表；全新状态下列表为空。此查询验证 API 访问；沙箱创建、原生资源强制与 SDK 数据面就绪需要另行验证。不要公开密钥，也不要把带凭据的日志贴进 issue。

## 启用 daemon 内存池 {#memory-pool}

在上述 `serve` 命令中添加 `--memory-pool`，即可让新沙箱接入 daemon 持有的共享冷页池；默认关闭。池压缩冷页，相同内容只保存一份，VM 再次访问时恢复到自己的私有 RAM。扫描和实际回收由 VM 运行时执行，daemon 统一持有去重对象；运行时复核内容未变化且对象已发布后才回收。Linux 需要 userfaultfd 内核缺页权限。实际占用与扫描、内容和访问方式有关，见[内存 benchmark](../../benchmarks/vm-memory/index.md#linux-lifecycle)。

池组件独立于 API 进程，复用同一状态目录时可跨 API 重启继续持有对象。池进程本身必须保持运行，丢失后依赖它的 VM 会失败，不会自动替换有活动引用的池。默认上限为 512 MiB 编码数据、32,768 个对象，每个 VM 连接最多 32,768 个引用（对应 128 MiB 冷页）；预算不足时保留驻留页。索引、线程和分配器另有开销；池在单个沙箱的 cgroup 与准入额度之外，需要预留宿主余量。启动保留尚未驻留的 RAM，首次缺页只按 4 KiB 分配零页；扫描、去重、回收和缺页恢复均以独立的 4 KiB 页为单位；扫描跳过未驻留页，读取热页只恢复该页，邻近冷页继续保留在池中。总预算仍应包含实际工作集与恢复峰值。这个开关的 VM/池测试不等于完整 OpenSandbox SDK 端到端验证。

## 连接客户端 {#clients}

使用 OpenSandbox SDK 1.1.0，配置选定的 domain、protocol 与生命周期 API key。创建请求需要 `image`、`entrypoint` argv，以及恰好包含 `cpu`、`memory` 的 `resourceLimits`。可选 `timeout` 以秒计，至少 60 秒；省略或 null 表示手动清理。这里不提供 prepared-image 名称，因为目前没有经过验证的开箱即用镜像配方。

daemon 检查真实 execd 的 `/ping`、`/ready` 和 egress 的 `/healthz` 后才把创建视为就绪。`202` 表示生命周期请求已接受；工作负载退出结果需通过命令接口获取。命令、文件与 metrics 流量转发到镜像中的 execd 和 egress 服务。

默认只监听 loopback。对外访问前，在可信反向代理上配置 TLS，并用 `--public-endpoint HOST:PORT` 指定外部路由可达的 authority，不带 scheme 或路径。通配监听与开发用的零端口绑定也要求显式正确的 public endpoint。认证见[端点认证](operations.md#endpoints)。

版本 1 中已有记录的 `sandboxes.json` 若缺少原生 `owner.json` marker，会在既有 store 独占锁内被拒绝：它可能仍持有活动 Podman 容器。使用全新原生状态并保留／清理旧部署，或通过旧 Podman daemon 删除全部 sandbox、确认清理后，再用已清空的 registry 切换后端。不要删除 registry 条目、预留或所有权状态，也不要伪造原生 marker 绕过检查。原生 daemon 不会把这些容器接管为 Missing 或静默释放其预留。

当前 v2 状态使用小型 `sandboxes.json` owner/header 与空 map，加上私有逐 sandbox `records/`。缺少原生 `owner.json` 仍会拒绝 v2 启动；空 header 不证明 sandbox 不存在。归属／记录状态须一同保留。原生 v1 存储只有在运行时 factory 接受 owner 后才迁移；见[存储迁移](../../design/daemon/storage.md#migration)。

## 从 Cluster 迁移 {#migration}

旧 Cluster 任务/客户端 SDK、Controller/Worker 注册、放置、DAG、lease 续期、完成 outbox 和 artifact 退役命令都不是 daemon 接口。不要把旧 task JSON、`worker.toml`、Controller 凭据或 journal 作为 daemon 输入。分布式任务历史没有自动转成沙箱状态的路径。

退役部署前保留旧结果，为 OpenSandbox profile 使用新状态。`NativeRuntime` 已接入原生 VM 执行；stage/apply 与 checkpoint/fork API 未实现，也不自动获取 node/cache/pool 共享资源。见[运维](operations.md)。
