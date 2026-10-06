# Daemon 运行时与集成边界

管理本机镜像沙箱生命周期时选择 `pvisor-daemon`；需要原生 Job 执行和文件审查时选择 `pvisor run`。两者的运行时、状态与能力相互独立。

## Prepared-image 契约 {#images}

当前 daemon 后端在 Linux 上调用可信的外部 rootless Podman 可执行文件。镜像必须已存在于本机：创建使用 `--pull=never`，没有 image-auth 或 registry-pull API。

镜像 ENTRYPOINT 必须监督请求的工作负载 argv，**同时**运行真实 OpenSandbox 1.1.0 execd 与 egress 服务，分别监听容器端口 44772、18080。请求会替换镜像 CMD；wrapper 必须不经 shell 插值忠实执行参数、转发信号并回收子进程，还要自行完成运行时初始化和服务认证配置。

daemon 不注入 `/execd`，不执行其初始化握手，也不合成命令/SSE/文件响应。就绪检查要求 execd `/ping`、`/ready`，以及 egress `/healthz`。Python SDK 初始化即使没有请求网络策略，也会解析两个端点。

固定版本的 upstream 默认 egress sidecar 需要 iptables 重定向，不能原样运行在 `cap-drop=ALL` 下。必须准备真实的无 capability 部署；目前没有经过端到端验证的镜像配方。添加 `NET_ADMIN`、改用 privileged 容器或用假服务替代就绪响应，都不是受支持的绕过方法。镜像契约满足前，普通 SDK 创建会在就绪检查失败。

## 隔离与资源限制 {#isolation}

创建会安装私有 namespace、no-new-privileges、`cap-drop=ALL` 和 CPU/memory/swap/PID 限制，并检查容器资源配置。准入保守地累计硬限制，包含 paused、failed 和状态不确定的沙箱；观察到空闲不授权隐式超售。

Rootless slirp4netns 禁止访问宿主 loopback，但**不拒绝所有出口**。请求的网络策略会被拒绝。此后端共享宿主内核，不是 VM 级边界，也未经过安全审计。宿主账户、Podman 配置/hooks 与 prepared image 都属于可信输入。

其他本机用户可能访问原生 loopback 服务端口。daemon 端点认证不保护这些原生映射；考虑不可信多用户部署前，需要真实服务认证和宿主网络控制。工作负载秘密不放进 Podman argv，但可信宿主/runtime 所有者仍能看到。

## OpenSandbox 接口范围 {#profile}

兼容基线固定为 OpenSandbox **1.1.0**，tag `release-1.1.0`，commit `b1a29cf93a823a95913f7943010febb3f29de05c`。这是部分 API profile，不是完整 OpenSandbox 或未经修改 SDK 的端到端兼容承诺。

| 能力 | 当前行为 |
| --- | --- |
| 镜像创建、列表、查询、删除 | 本机沙箱生命周期；创建接受 argv/env/metadata、CPU/memory 限制及可选 TTL |
| Pause / resume | 原生 cgroup freeze/unfreeze，不是 VM checkpoint |
| 续期 | 未来 RFC3339 时间，必须延长已有 TTL |
| 端点 | 仅 daemon 路由的 execd 44772 与 egress 18080 |
| 命令、文件、health、metrics | 流式转发到 prepared image 中的真实 upstream 服务 |
| 快照、templates/pools、metadata 修改、hooks | 不支持 |
| 网络策略、credential proxy、secure access、volumes、image auth | 不支持的创建选项被拒绝 |
| 任意应用端口、签名端点、WebSocket、CONNECT | 不支持 |
| Stage/apply、checkpoint/fork、VM/offload | 未接入此 daemon 后端 |

## 原生 VM、检查点与 Gateway {#native}

原生 `pvisor run --executor vm` 保留 KVM/HVF 与 rootfs 前提，不使用此 Podman adapter。执行步骤见 [VM 指南](../executors/vm.md)，本机 Job 文件检查点见[检查点与分叉](../fork-checkpoint.md)。兼容的独立 rootfs、无网络 VM Job 可以使用 execution checkpoint；通过 `pvisor status --json` 检查 capability 与 blockers，并遵循 [CLI execution checkpoint 契约](../../reference/cli.md#full-vm-execution-checkpoints)。

daemon pause 是 cgroup freeze，不是封存 RAM/CPU/设备/文件系统快照。daemon sandbox ID 不是 Job ID，不要传给 `pvisor checkpoint`、`fork`、`apply` 或 `drop`。原生快照存储、node owner、共享镜像缓存与实验性 macOS 冷页池仍是原生设施，不是 daemon template 或 pool。

原生 Job 的模型请求路由和捕获使用 [Gateway 指南](../capture.md)与 [Agent 接入](../agents/index.md)。daemon 没有旧 Worker 的 `gateway.routes` profile，也没有集成的 pVisor credential proxy/capture 契约。模型访问由 prepared service 和应用负责；文件审查不能撤销外部 API 副作用。
