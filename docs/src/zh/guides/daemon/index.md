# 运行单机 daemon


缓存是同一 daemon 的可选组件：在 sandbox 启动命令上添加 `--cache`，或单独运行 `pvisor-daemon serve --cache`。`--cache-listen` 和 `--cache-image-store` 配置独立端点与存储。缓存启动失败或异常退出会使 daemon 报错；daemon 关闭或崩溃时缓存 worker 退出。HTTP 关闭最多等待十秒；进行中的缓存请求可能失败。原生 VM supervisor 和共享池保留现有重启生命周期。
安装 `pvisor-daemon`，通过部分兼容 OpenSandbox 1.1.0 的 API，在一台 Linux 主机上管理镜像沙箱。daemon 负责本机准入、生命周期、持久状态与过期清理。

跨节点调度、全局 DAG 和分布式 lease 由 Kubernetes、Ray 或你的应用管理。daemon 不包含 Controller 或 Worker。

需要本机原生 VM 沙箱生命周期时用 `pvisor-daemon`；公开 Job 与文件审查流程仍用 `pvisor run`。

!!! warning
    项目**尚未提供 guest bootstrap 或镜像配方，也未做端到端验证**；目前没有 SDK 兼容性验证或密度测量结果。下列每一步都需要自行准备镜像与 vsock 桥接。

## 前提条件 {#prerequisites}

需要 Linux x86_64、可用的 `/dev/kvm`、可信的绝对路径，以及可写、已委派并启用 CPU/memory/PID controller 与 `cgroup.kill` 的 cgroup v2 层级。

前置检查会实际写入 controller 并调用 KVM API。没有 host、OCI 命令或 registry-pull 降级。

镜像是可信本机 `images_dir/<key>.json` manifest，不是 registry reference。字段包括绝对路径的独立 Linux `rootfs`（不能是宿主 `/` 或与 daemon 状态重叠）、绝对路径的 guest bootstrap `entrypoint` argv、可选 `cmd`、可选 `env` 与可选绝对路径 firmware `library_dir`。请求的工作负载 argv（为空则使用 `cmd`）追加到 `entrypoint`，请求 env 覆盖 manifest env，不继承宿主环境，也不经 shell 插值。

长期运行的 guest bootstrap 必须监督工作负载、真实 OpenSandbox 1.1.0 execd 与 egress，自行初始化／鉴权服务、转发信号并回收子进程。它必须在 guest **CID 3** 的 **44772/18080** 端口提供字节透明的 AF_VSOCK listener，桥接真实服务。Supervisor 的 loopback TCP 发布经私有 Unix socket 与原生 vsock 转发连接 guest。普通 rootfs 或只运行 sleep 的进程不够。Create、对 Running VM 的 Inspect 与 resume 就绪检查经 bridge 要求真实 HTTP 200 的 execd `/ping`、JSON `initialized: true` 的 `/ready` 和 egress `/healthz`，响应体有界。daemon 不注入／初始化 execd，也不合成 command/SSE/file 响应。

## 安装与启动 {#install}

Linux x86_64 和 macOS ARM64 wheel 都会随 `pvisor` 安装 `pvisor-daemon`。缓存服务支持两个平台；以下 sandbox 服务需要 Linux x86_64：

```bash
pip install pvisor
pvisor-daemon --help
pvisor-daemon protocol
```

需要 `main` 最新构建时，使用 [nightly wheel 安装器](../../start/installation.md)。`protocol` 打印固定的 OpenSandbox 版本与 commit。源码 revision、SDK 1.1.0 与镜像内容应一起固定。本地源码安装路径（`cargo install --locked --path crates/pvisor-daemon`）尚未验证。

`serve` 使用已实现且必需的 `--images-dir` 与 `--cgroup-root` 构造 `NativeRuntime`：

```bash
export OPEN_SANDBOX_API_KEY="$(openssl rand -hex 32)"
pvisor-daemon serve --sandbox \
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

## OpenSandbox 接口范围 {#profile}

兼容基线固定为 OpenSandbox **1.1.0**，tag `release-1.1.0`，commit `b1a29cf93a823a95913f7943010febb3f29de05c`。支持范围限于下表中的部分 API；完整 OpenSandbox 与未经修改 SDK 的端到端兼容性尚未验证。

| 能力 | 当前行为 |
| --- | --- |
| 镜像创建、列表、查询、删除 | 本机沙箱生命周期；创建接受 argv/env/metadata、CPU/memory 限制及可选 TTL |
| Pause / resume | 同一 Attempt 上已确认的 live vCPU pause/resume，不是 checkpoint |
| 续期 | 未来 RFC3339 时间，必须延长已有 TTL |
| 端点 | 仅 daemon 路由的 execd 44772 与 egress 18080 |
| 命令、文件、health、metrics | 流式转发到 prepared image 中的真实 upstream 服务 |
| 快照、templates/pools、metadata 修改、hooks | 不支持 |
| 网络策略、credential proxy、secure access、volumes、image auth | 不支持的创建选项被拒绝 |
| 任意应用端口、签名端点、WebSocket、CONNECT | 不支持 |
| Stage/apply、checkpoint/fork、offload | 未接入此 daemon 后端 |

## 查询、删除与过期 {#operations}

| 请求 | 含义 |
| --- | --- |
| `GET /v1/sandboxes` | 最近持久化的观察；支持重复 state 筛选、SDK 编码的 metadata 与 page/pageSize |
| `GET /v1/sandboxes/{id}` | 与本机原生状态核对该沙箱 |
| `DELETE /v1/sandboxes/{id}` | 确认原生删除后才释放预留；成功返回 204 |
| `POST /v1/sandboxes/{id}/pause` | 确认 live vCPU 已暂停；202，空响应体 |
| `POST /v1/sandboxes/{id}/resume` | 确认 live vCPU 已恢复；202，空响应体 |
| `POST /v1/sandboxes/{id}/renew-expiration` | 用未来 RFC3339 时间 `expiresAt` 延长已有 TTL |

将 `{id}` 替换为创建/列表返回的 sandbox ID。HTTP 断开不会取消已接受的操作。不同沙箱有独立生命周期锁；同一沙箱的控制与连接建立串行协调。

丢失创建响应后不要盲目重复创建：此 API 不是旧任务 ID 幂等协议。先检查保留状态。创建报告原生结果不确定时，保留错误消息中的 sandbox ID，再查询/删除它。创建失败且清理确认完成才释放准入；清理不确定则保留记录与预留。

创建 `timeout` 可选，省略/null 表示手动清理。配置 TTL 至少 60 秒，不超过 `--max-timeout-seconds`（默认一天，最高可配置一年）。续期必须延长已有期限。过期清理是 **best effort**，不是硬执行截止时间；过期后拒绝新的代理请求。正常停止 daemon 会保留原生沙箱与持久记录供重启使用；daemon 停机期间不保证清理。需要硬截止或停机清理时，另行配置原生宿主监督。

## 端点认证 {#endpoints}

通过 `GET /v1/sandboxes/{id}/endpoints/44772` 解析 execd，端口 `18080` 解析 egress。返回的 authority 不含 URL scheme，流量经过 daemon，不直接指向 supervisor loopback 发布。

- 设置 `use_server_proxy=true` 时，路由请求使用生命周期 `OPEN-SANDBOX-API-KEY` header 认证。
- 默认模式在端点 `headers` 中提供随机、沙箱范围的 `X-PVISOR-SANDBOX-TOKEN`。客户端必须保留这些 header；该 token 不授权生命周期操作或访问其他沙箱。
- 转发到 guest 前会移除控制凭据。调用方提供的 guest execd access token 不会被生命周期 key 替换。

不支持签名端点过期、任意应用端口、WebSocket 或 CONNECT。请求/响应流式转发，包括 multipart 与 SSE。上传加等待 upstream header 的上限为 120 秒；已建立的响应流没有总超时。输出/流量配额仍由 upstream 服务负责。

默认只监听 loopback。对外访问前，在可信反向代理上配置 TLS，并用 `--public-endpoint HOST:PORT` 指定外部路由可达的 authority，不带 scheme 或路径。

## 启用 daemon 内存池 {#memory-pool}

在 `serve` 命令中添加 `--memory-pool`，即可让新沙箱接入 daemon 持有的物理共享页池；默认关闭。运行时扫描已驻留的 4 KiB 页面，优先收纳跨实例重复内容，daemon 保存一份物理页。VM 读取时保持共享，写入时由内核 COW 生成私有页。这个 Linux 路径不需要 userfaultfd，也不压缩独有页。实际占用与重复程度、扫描和写入有关，见[内存 benchmark](../../benchmarks/vm-memory/index.md#linux-lifecycle)。

池组件独立于 API 进程，可跨 API 重启保留页面。连接断开后，用 pidfd 确认 VM 进程退出才释放映射引用。池进程丢失会使依赖 VM 失败，不会自动替换活动池。默认上限为 512 MiB 页面数据、32,768 个对象、32 个连接，每个连接 32,768 个引用：4 KiB 对象数限制使默认池最多保存 128 MiB 不同页面，每 VM 最多引用 128 MiB。索引、映射、线程和分配器另有开销；池在单个沙箱 cgroup 与准入额度之外，需预留宿主余量。升级旧压缩池时先排空旧 VM，再使用新状态目录。

若要选择实验性压缩模式，改为添加 `--memory-pool --memory-pool-mode compressed`。相同的解码后 64 KiB 块共享 sealed fill/zstd 对象，独有可压缩内容也能入池。VM 启动需要内核缺页 userfaultfd 权限。池丢失后已持有句柄仍可用，新发布停止。模式绑定持久配置，切换需排空旧实例并使用新状态目录。进程级共享与服务退出测试通过，双 VM 探针也已验证池终止后的 guest UFFD 回收/refault 与文件 I/O。见 [sealed 压缩池合同](../../design/memory-optimization/compression-pool.md)。这个开关的 VM/池测试不等于完整 OpenSandbox SDK 端到端验证。

## 重启与状态所有权 {#restart}

一个 daemon 独占私有状态；原生创建／控制前持久化 owner 身份。私有 IPC 校验同 UID peer、owner、sandbox ID、generation 与秘密 token；持久身份绑定 boot ID 和 cgroup device/inode。ID 不复用、不重新启动。IPC 丢失表示不确定，不是 Missing 或清理证据。持久删除意图与 supervisor 独占锁阻止迟到启动；清理使用身份绑定的 `cgroup.kill`，不保存 PID 或按 PID kill，确认 cgroup 为空且 owner 锁释放后才释放容量。同一 boot 下没有持久 tombstone 证据的 cgroup 被替换或丢失不证明对象不存在。

待删除意图跨重启保留，不会被原生观察覆盖。删除状态未知时保留预留。原生沙箱丢失后仍以 Failed 显示，直到显式删除。不要删状态来修复错误，也不要让两个 daemon 共用它。注册表受配置容量和总计 16 MiB 逻辑版本 1 等价预算约束；它不是无限保留的分布式任务档案，也不是 Run Bundle/artifact store。

版本 1 中已有记录的 `sandboxes.json` 若缺少原生 `owner.json` marker，会在既有 store 独占锁内被拒绝：它可能仍持有活动 Podman 容器。使用全新原生状态并保留／清理旧部署，或通过旧 Podman daemon 删除全部 sandbox、确认清理后，再用已清空的 registry 切换后端。不要删除 registry 条目、预留或所有权状态，也不要伪造原生 marker 绕过检查。原生 daemon 不会把这些容器接管为 Missing 或静默释放其预留。

当前 v2 状态使用小型 `sandboxes.json` owner/header 与空 map，加上私有逐 sandbox `records/`。缺少原生 `owner.json` 仍会拒绝 v2 启动；空 header 不证明 sandbox 不存在。归属／记录状态须一同保留。原生 v1 存储只有在运行时 factory 接受 owner 后才迁移；见[存储迁移](../../design/daemon/storage.md#migration)。

删除／对账可在原 rootfs 或 firmware 缺失时继续基于 tombstone 的清理，回收空的所属 cgroup 和私有 run/RAM/temp/socket／秘密记录，保留最小 ID 屏障。回收错误保留预留供重试；不要自行删除 tombstone 或 registry 状态。见[存储](../../design/daemon/storage.md#gc)。

## 隔离与信任边界 {#isolation}

原生 supervisor 在独立、脱离 daemon 生命周期的子进程中嵌入 `pvisor::PVisor`，只配置 `VmExecutor` 并持有 RunHandle。Sandbox cgroup 限制整个 supervisor/VMM/helper 树：总 CPU 速率 **10–8000 millicores**、硬内存、零 swap、`pids.max=512` 与 group OOM。vCPU 数按 quota 向上取整（最多 8），guest RAM 向下取整到 MiB；硬内存上限还包含宿主侧开销。Paused、Failed 和不确定记录继续保守预留资源。

启动 callback 在 owner 锁内检查 started／删除／tombstone marker，通过预先打开的 `cgroup.procs` FD 在 **exec 之前**加入身份绑定的 cgroup。直接在该 cgroup 外调用隐藏命令会失败关闭。

原生 OverlayNet 提供 VM 出口网络。OpenSandbox 网络策略请求不受支持，会被拒绝；拒绝请求不会将出口改为 deny-all。

宿主账户、daemon/firmware 和预制镜像必须可信。私有状态与同 UID IPC 无法阻止敌对同 UID 或 root 代码。其他本机用户可能访问 loopback 发布，应配置服务鉴权与宿主访问控制。秘密不进入 supervisor argv 或宿主环境，但保存在私有记录中。目前没有安全审计结果；敌对多用户隔离不在保护范围内。

## 与原生 Job、缓存和旧 Cluster 的关系 {#native}

daemon sandbox ID 不是 Job ID，不要传给 `pvisor checkpoint`、`fork`、`apply` 或 `drop`。daemon pause 是同一 live Attempt 上已确认的 vCPU pause，不是封存 RAM/CPU/设备/文件系统快照。原生 Job 的文件审查、检查点与模型捕获继续走 `pvisor run` 路径：执行步骤见 [VM 指南](../executors/vm.md)，本机 Job 文件检查点见[检查点与分叉](../fork-checkpoint.md)，模型请求路由和捕获见 [Gateway 指南](../capture.md)。兼容的独立 rootfs、无网络 VM Job 可以使用 execution checkpoint；通过 `pvisor status --json` 检查 capability 与 blockers。

`pvisor cache prepare/publish/list/stat/read` 保持为独立命令，不是 daemon 子命令；发布、后端、认证和访问见[共享镜像缓存参考](../../reference/shared-image-cache.md)。Daemon 状态与原生 Job 和缓存存储分开保存；Daemon API key 不授权独立缓存或 node 运行时 IPC。

旧 Cluster 任务/客户端 SDK、Controller/Worker 注册、放置、DAG、lease 续期、完成 outbox 和 artifact 退役命令都不是 daemon 接口。不要把旧 task JSON、`worker.toml`、Controller 凭据或 journal 作为 daemon 输入；分布式任务历史没有自动转成沙箱状态的路径。退役部署前保留旧结果，为 OpenSandbox profile 使用新状态。

## 排障 {#troubleshooting}

| 现象 | 检查 |
| --- | --- |
| 绑定前启动失败 | Linux x86_64/KVM、短绝对状态路径、可信 manifest 和真实委派 cgroup v2 |
| 已有记录的 v1 registry 缺原生 `owner.json` | 保留旧状态／容器；使用全新原生状态，或通过旧 Podman daemon 清理全部 sandbox 后切换 |
| v2 header 缺原生 `owner.json`，即使 map 为空 | 恢复原始归属状态；不要删除 `records/`、预留或伪造 marker |
| 状态目录忙 | 其他 daemon 持有独占锁；不要删锁或状态绕过 |
| 镜像不可用 | 先在本机准备镜像；不支持自动 pull 或 image auth |
| SDK 创建无法就绪 | 真实 execd 初始化与 `/ping`/`/ready`、egress `/healthz` 及 guest CID 3 的 44772/18080 vsock bridge；普通 rootfs 不够 |
| 外部端点不可达 | TLS 代理路由、domain/protocol、`--public-endpoint` 与端点提供的 headers |
| 失败/pause 后准入仍满 | Failed/uncertain/paused 保留硬预留；查询并确认删除 |
| 过期沙箱仍存在 | 清理是 best effort；检查 daemon 在线时间、生命周期锁与原生清理错误 |
| 策略/template/volume 被拒绝 | 不支持的选项被拒绝，不会静默安装或模拟 |

API 错误包含 `{code, message}`，响应包含 `X-Request-ID`。保留 HTTP status、request ID 和脱敏诊断；API 成功不是命令退出状态。见[退出码与错误](../../reference/exit-codes.md#daemon)。
