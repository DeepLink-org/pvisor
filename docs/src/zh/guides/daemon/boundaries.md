# Daemon 运行时与集成边界

管理本机原生 VM 沙箱生命周期时选择 `pvisor-daemon`；公开 Job／文件审查流程使用 `pvisor run`。daemon 嵌入原生执行，但 API 身份、状态与能力仍独立。

## Prepared-image 契约 {#images}

镜像是可信本机 `images_dir/<key>.json` manifest，不是 registry reference。字段包括绝对路径的独立 Linux `rootfs`（不能是宿主 `/` 或与 daemon 状态重叠）、绝对路径的 guest bootstrap `entrypoint` argv、可选 `cmd`、可选 `env` 与可选绝对路径 firmware `library_dir`。请求的工作负载 argv（为空则使用 `cmd`）追加到 `entrypoint`，请求 env 覆盖 manifest env，不继承宿主环境，也不经 shell 插值。

长期运行的 guest bootstrap 必须监督工作负载、真实 OpenSandbox 1.1.0 execd 与 egress，自行初始化／鉴权服务、转发信号并回收子进程。它必须在 guest **CID 3** 的 **44772/18080** 端口提供字节透明的 AF_VSOCK listener，桥接真实服务。Supervisor 的 loopback TCP 发布经私有 Unix socket 与原生 vsock 转发连接 guest。普通 rootfs 或只运行 sleep 的进程不够。

Create、对 Running VM 的 Inspect 与 resume 就绪检查经 bridge 要求真实 HTTP 200 的 execd `/ping`、JSON `initialized: true` 的 `/ready` 和 egress `/healthz`，响应体有界。daemon 不注入／初始化 execd，也不合成 command/SSE/file 响应。Python SDK 初始化即使没有网络策略也会解析两个端点。

Bootstrap 与镜像配方**未提供，也未经端到端验证**。旧容器的 `cap-drop=ALL` 限制不适用于此原生 VM 后端；upstream 镜像名称不是原生 bootstrap/vsock 适配器。不提供假就绪，也没有 SDK 兼容或密度证据。

## 隔离与资源限制 {#isolation}

要求 Linux x86_64、可用 `/dev/kvm`、可信绝对路径，以及可写、已委派且启用 CPU/memory/PID controller 与 `cgroup.kill` 的 cgroup v2 层级。前置检查验证真实 controller 写入和 KVM API；没有 host、OCI 命令或 registry-pull 降级。

原生 supervisor 在独立、脱离 daemon 生命周期的子进程中嵌入 `pvisor::PVisor`，只配置 `VmExecutor` 并持有 RunHandle。Sandbox cgroup 限制整个 supervisor/VMM/helper 树：总 CPU 速率 **10–8000 millicores**、硬内存、零 swap、`pids.max=512` 与 group OOM。vCPU 数按 quota 向上取整（最多 8），guest RAM 向下取整到 MiB；硬内存上限还包含宿主侧开销。完整限制／就绪检查保留在 create、Inspect 和 resume；端点解析认证 live Running 状态并检查删除屏障，不为每次数据请求重复完整 health/cgroup 检查。见[服务访问](../../design/daemon/lifecycle.md#endpoints)。Paused、Failed 和不确定记录继续保守预留资源。

启动 callback 在 owner 锁内检查 started／删除／tombstone marker，通过预先打开的 `cgroup.procs` FD 在 **exec 之前**加入身份绑定的 cgroup。Supervisor 启动时核验成员关系，不迁移已运行的 Tokio 进程；supervisor/Tokio 分配与后续 VM/helper 子进程均计入 sandbox 预算。直接在该 cgroup 外调用隐藏命令会失败关闭。

原生 OverlayNet 提供 VM 出口网络；OpenSandbox 网络策略请求仍不支持并被拒绝，不等于 deny-all egress。宿主账户、daemon/firmware 和预制镜像属于可信输入；私有状态与同 UID IPC 不防御敌对宿主 UID/root 代码。其他本机用户可能访问 loopback 发布，仍需真实服务鉴权与宿主控制。秘密不进入 supervisor argv 或宿主环境，但保存在私有记录中。不承诺安全审计或敌对多用户隔离。

私有 IPC 校验同 UID peer、owner、sandbox ID、generation 与秘密 token；持久身份绑定 boot ID 和 cgroup device/inode。ID 不复用、不重新启动。IPC 丢失表示不确定，不是 Missing 或清理证据。持久删除意图与 supervisor 独占锁阻止迟到启动；清理使用身份绑定的 `cgroup.kill`，不保存 PID 或按 PID kill，确认 cgroup 为空且 owner 锁释放后才释放容量。同一 boot 下没有持久 tombstone 证据的 cgroup 被替换或丢失不证明对象不存在。

## OpenSandbox 接口范围 {#profile}

兼容基线固定为 OpenSandbox **1.1.0**，tag `release-1.1.0`，commit `b1a29cf93a823a95913f7943010febb3f29de05c`。这是部分 API profile，不是完整 OpenSandbox 或未经修改 SDK 的端到端兼容承诺。

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

## 原生 VM、检查点与 Gateway {#native}

原生 `pvisor run --executor vm` 保留 KVM/HVF 与 rootfs 前提，daemon 使用原生 VM executor，但不暴露这些 Job API。执行步骤见 [VM 指南](../executors/vm.md)，本机 Job 文件检查点见[检查点与分叉](../fork-checkpoint.md)。兼容的独立 rootfs、无网络 VM Job 可以使用 execution checkpoint；通过 `pvisor status --json` 检查 capability 与 blockers，并遵循 [CLI execution checkpoint 契约](../../reference/cli.md#full-vm-execution-checkpoints)。

daemon pause 是同一 live Attempt 上已确认的 vCPU pause，不是封存 RAM/CPU/设备/文件系统快照。daemon sandbox ID 不是 Job ID，不要传给 `pvisor checkpoint`、`fork`、`apply` 或 `drop`。原生快照存储、node owner、共享镜像缓存与实验性 macOS 冷页池仍是原生设施，不是 daemon template 或 pool。

原生 Job 的模型请求路由和捕获使用 [Gateway 指南](../capture.md)与 [Agent 接入](../agents/index.md)。daemon 没有旧 Worker 的 `gateway.routes` profile，也没有集成的 pVisor credential proxy/capture 契约。模型访问由 prepared service 和应用负责；文件审查不能撤销外部 API 副作用。
