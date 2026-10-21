# 选择 pVisor 接入方式

需要原生 Job 与文件审查时选 CLI，需要在 Rust 应用中拥有运行生命周期时选 Runtime，需要 HTTP 沙箱管理时评估 daemon API，已有 containerd 调用链则使用 shim。先决定调用协议和状态所有者，再核对需要的能力。

## 入口与所有权 {#entry-points}

| 入口 | 适合的调用方 | 管理的身份与生命周期 | 接入前提 |
| --- | --- | --- | --- |
| `pvisor` CLI | 开发者、脚本、CI、通过子进程集成的 evaluator | 原生 Job/Attempt；命令管理运行、审查和状态操作 | 按所选 host、OCI 或 VM 执行器准备环境；用机器可读输出关联任务 |
| Rust `pvisor` Runtime | 需要直接控制执行与清理的 Rust 应用 | `PVisor`、`RunHandle` 与 Job service；应用持有运行时并完成等待与清理 | 固定 crate 版本，配置 executor/drivers，并遵守生命周期与持久化合同 |
| `pvisor-daemon serve --sandbox` | 使用 HTTP 生命周期 API 的平台服务 | daemon sandbox ID；本机准入、TTL、supervisor 与持久状态 | Linux x86_64、KVM、委派 cgroup v2、可信 prepared image 与 guest 服务桥接 |
| `containerd-shim-pvisor-v2` | containerd / CRI 调用链 | containerd task / exec 身份及相应任务生命周期 | Linux 与 containerd 配置；VM 路径另需 `vm` 构建特性及 KVM |

CLI 是调用方式，executor 是运行环境；选择 CLI 不等于只能运行 host 进程。API 身份不可混用：daemon sandbox ID 不能传给原生 Job 命令，containerd task ID 也不自动成为 Job selector。

## 当前能力矩阵 {#capabilities}

“受条件限制”表示必须核对 executor、profile 和配置。运行时提供机制不代表每个前端都暴露该机制。

| 能力 | 原生 CLI | Rust Runtime | daemon Sandbox API | containerd shim |
| --- | --- | --- | --- | --- |
| 运行与取消 | Job 命令 | 应用持有执行句柄与生命周期 | 创建、查询、删除及 TTL | task/exec 生命周期 |
| 文件暂存、审查、apply/drop | 显式启用暂存后使用 | 通过配置与 Job service 集成 | 未暴露 | 未暴露原生 Job 审查接口 |
| 工作区 checkpoint/fork | 按支持条件使用 | 按 Job service 合同集成 | 未暴露 | 未实现 checkpoint |
| 完整执行 checkpoint/resume/fork | 受支持 VM profile | 原生执行状态服务；同样受 profile 限制 | 未暴露；pause/resume 只控制 live vCPU | 未实现 checkpoint 与 pause/resume |
| 模型通信捕获 | 显式配置 Gateway | 嵌入应用配置 Gateway driver | 未暴露该 API | 未暴露该 API |
| 运行结果与证据 | Job 记录、Bundle 与 JSON 输出 | 按所用 Session/Job service 合同收集 | 生命周期状态与 upstream 数据面；无 Run Bundle 导出 | task 事件、退出状态与 I/O；无原生 Bundle 接口 |

原生 execution checkpoint 当前限定于支持的无网络、私有 RAM VM profile，恢复要求相同宿主启动、二进制与固件；共享池等配置有独立限制。完整条件及能力查询见[CLI 参考](../../reference/cli.md#full-vm-execution-checkpoints)。

daemon 实现部分 OpenSandbox 1.1.0 profile；项目尚未提供 bootstrap/镜像配方，未完成 SDK 端到端兼容性验证，也没有完整 daemon 密度证据。已有底层 VM/内存池测试不替代这些验证。接口与交付要求以[daemon 指南](../daemon/index.md#profile)为准。

## 原生 Job 与嵌入式应用 {#native}

先通过[离线任务](../../start/first-run.md)验证本机环境，再决定是否需要嵌入 Runtime。脚本或现有训练器可以调用 CLI，并用[机器可读输出](../../reference/json-output.md)保留任务与执行结果的关联。

Rust 应用直接管理 executor、drivers、等待和清理。取消请求、进程退出、资源回收与终态发布有不同时间点；接入时需要按照[执行模型](../../design/execution-model.md)保留相应所有者。API 入口见 [Runtime README](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor/README.md)，接口兼容性见[稳定性说明](../../reference/stability.md)。

## containerd 集成 {#containerd}

shim 将 containerd task/exec 操作接到 host 或显式选择的 VM 路径。它与原生 CLI 的 OCI executor 是不同入口：后者通过 `pvisor run --executor container` 调用 `runc`/`crun` 并保持 Job 流程。

VM shim 的 CPU/RAM 注解配置 VM 形状；部分宿主 OCI 约束被接受为 no-op，不能据此推断对应限制已在 guest 中强制生效。注册、guest exec、挂载和支持范围见 [shim README](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-shim/README.md)。从注册成功到实际部署，还需验证工作负载要求的控制和数据路径。

## 镜像缓存是共享服务 {#cache}

客户端通过 `pvisor cache prepare/publish/list/stat/read` 管理内容；服务入口为 `pvisor-daemon serve --cache`，可以与 `--sandbox` 同时启用。缓存服务支持 Linux x86_64 与 macOS ARM64；单独启用缓存不要求 KVM 或 sandbox cgroup 条件。

缓存端点、存储与认证独立于 sandbox API。准备了缓存镜像并不等于生成了 daemon 所需的 prepared-image manifest 和 guest bootstrap。配置与后端合同见[共享镜像缓存参考](../../reference/shared-image-cache.md)。

## 将执行交给上层系统 {#handoff}

调用方需要固定输入，关联业务身份与执行身份，并定义超时、结果接受和重试规则。断开等待或收到不确定错误时，先按所用接口核对状态，避免重复启动、评分或清理。

在同一个版本与环境中验证成功、失败、取消和资源回收后，再扩大并发。[RL rollout](../rl-rollouts.md)给出原生 Job 的评分交接；[基础设施总览](index.md)说明推理、状态复用与容量评估的关系。
