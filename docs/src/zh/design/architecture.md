# 核心架构

pVisor 是 Operation 的处理核心：接收操作请求，根据策略决定如何处理和放置，调用实际执行机制，再通过 Event 描述发生了什么。

这条主线分成两个职责：**core 提供定义，pvisor 提供实现。** 外部调用方提交执行请求，通过运行句柄控制一次执行，通过 Event 观察它的过程和结果。

## 定义与实现

| 所有者 | 职责 |
| --- | --- |
| `pvisor-core` | Operation、策略决定、Placement、Outcome、Event，以及跨组件交互契约；共享纯校验与策略求值 |
| `pvisor` | 请求解析、能力准入、实际策略改写、放置选择、调度、执行和 Attempt 生命周期 |
| `pvisor-journal` | Event 提交、回执、去重、因果引用校验和恢复 |
| `pvisor-overlay-core` | 文件授权接入、写时复制、preimage、review/apply/recovery/drop |
| `pvisor-overlayfs` | 宿主 FUSE 挂载与文件操作接入 |
| `pvisor-overlaynet` | 网络解析、代理转发和 VM 网络接入；落实 core 定义的策略 |
| `pvisor-guest` | VM 内 PID 1 与命令启动契约 |
| `pvisor-gateway` | 可选的模型协议路由、转换和调用观察 |
| `pvisor-daemon` | 独立的单节点 sandbox 准入、持久归属、原生 VM supervisor 生命周期与端点代理 |
| `pvisor-tui`、`pvisor-replay` | 依赖 pvisor 的终端前端与 Agent 轨迹回放工具 |

[Daemon](daemon/index.md) 使用 VM-only NativeRuntime：独立 supervisor 嵌入 `PVisor::run`，跨 daemon 重启保留 RunHandle。可执行入口通过原生 CLI 设置构造 NativeRuntime；stage/apply、checkpoint/fork 和 Gateway API 未实现，也不自动获取 node 共享。见[职责收敛](daemon/responsibility-convergence.md)。主机选择和工作流属于外部编排。

core 不拥有执行循环，也不启动进程或打开控制 socket。pvisor 实现 AgentCtl 客户端／服务端和审批 socket；驱动实现各自的文件、网络与隔离边界。默认核心不依赖 Gateway、TUI 或 replay，捕获通过 `gateway` feature 启用。

## Host 与 Guest AgentCtl {#host-agentctl}

Host AgentCtl 是内置 Job CLI 操作的权限路径。Guest AgentCtl 是每个 Attempt
面向工作负载 `Hello`/`Sync`、客户端状态、directive 与 checkpoint 静默点的
协作路径。两者使用独立 schema、凭据与端点：guest 协作 token 不能授权
宿主 Job、VM、暂存文件或 daemon supervisor 控制。Bundle 的 `agentctl`
快照仍描述 Guest 协作，不是 Host 授权回执。

### Listener 与请求所有权 {#host-job-service}

CLI 解析类型化 `JobCommand` 并连接按需启动的持久 listener。
`JobCommand` 嵌入 CLI DTO，是要求精确 schema／构建匹配的内部契约，
不是稳定公共 API。Core 的共享 Host envelope 与 supervisor 契约仍是
纯定义和校验，不包含 CLI DTO 或传输。
`cli/host_service.rs` 用私有锁串行化启动，在规范化
`/tmp/pvisor-host-<有效 UID>` 下发布 generation/capability manifest 和
以 generation 命名的 Unix socket。根目录属于同 UID、不是符号链接，
权限恰为 `0700`；socket 与私有 manifest 文件为 `0600`。
已有条目无效时 fail closed，不用 chmod 修复。listener 独立于
daemon sandbox/pool 所有权及独立缓存。Node 资源协议仍是运行时设施；旧 CLI supervisor 已移除，没有 daemon 适配器。

通过内核同 UID peer 验证和构建兼容协商后，前端用 `SCM_RIGHTS` 传递
stdin/stdout/stderr，提交类型化命令、cwd、环境、终端上下文及固定目标。
listener 返回关联 ticket，携带 stdio 和私有 worker 通道。前端将检查过的
可执行文件启动为子进程，保留原终端 session，建立独立进程组。
listener 在授予准入前校验注册和 worker readiness；worker 不重新解释
shell argv。cwd、环境、输出与退出状态属于请求，不属于持久 listener。
嵌入式 `PVisor` 调用仍直接进入 runtime，不要求经过该 CLI 前端。

持久 Job selector 无需端点参数。选中的记录固定到 Job、Attempt 及存在时
的执行 generation，并在产生副作用前复核；过期选择不会悄悄指向新目标。
Host live 控制和 stage 发现链接也使用共享权限根目录下的私有端点。
executor 将权限根目录排除在 guest 暴露范围之外；CLI 拒绝暴露它的 guest
文件系统源。同 UID 是宿主信任边界，不是对该用户所有进程的隔离。

### 线上契约与升级 {#host-wire}

Core 定义 `AgentCtlHostRequest<C>`（`version`、`request_id`、可选 `target`、
`command`）与 `AgentCtlHostResponse<R>`（`version`、`request_id`、`result`）。
当前版本为 **1**；target 包含 `job_id`、可选 `attempt_id` 与可选
`generation`。端点所有者验证权限与目标范围。Live Attempt 端点要求精确
Job/Attempt，拒绝独立 generation。Envelope 拒绝未知字段；身份非空，
至多 256 字节，不含控制字符。Job 服务及其内部 worker 使用
`runtime/host_transport.rs` 的换行分隔 JSON；async/sync 使用相同 framing
规则，JSON 上限为 1 MiB，不含换行分隔符。reader 只消费到该分隔符，
保留下一个 frame 或 FD marker。`SCM_RIGHTS` marker 字节是独立传输记录，
不是 JSON；描述符处理由 `cli/host_fds.rs` 负责。类型化错误码为 `invalid_request`、`unauthorized`、`version_mismatch`、
`conflict`、`unsupported`、`internal` 和 `unavailable`。

Core 的 `host_protocol` 还定义 live VM 的 `HostVmCommand`（`Pause`、
`Resume`、`Offload`、`Status`）及 `HostVmResult`（`status`、`value`）。
`pvisor` 导出 `host_vm_exchange`，用于类型化 Host 请求／响应交换。
`--vm-load` 选择 `Resume`，映射为
同一 live Attempt 的 `RunResume`，不是 `Load` 线上操作。

内部 Job 握手在接纳描述符或命令之前检查 Host 版本 **1**、Job ticket
schema、Cargo 包版本及可执行文件内容的 BLAKE3 摘要。
仅包版本相同不代表兼容；原地重新构建也可能不兼容。worker 启动前检查
程序路径、所有权、权限、device/inode 与文件内容。

Linux 读取 `/proc/self/exe`。macOS 的 `cli/host_image.rs` 在对同一个
已打开文件求摘要之前，将 dyld 已加载主映像的 UUID 与磁盘 Mach-O 中
匹配 CPU slice 的 `LC_UUID` 比较。UUID 元数据缺失、格式错误、有歧义
或不匹配时 fail closed；准入要求源 Mach-O UUID 匹配。该检查验证首次路径替换前后的
可执行文件身份。UUID 匹配不等于已加载内存的
逐字节认证，也不等于内核固定的 exec 权限。macOS 平台路径尚未编译或
测试；parser 检查不能验证 dyld 访问、平台链接或真实程序替换行为。

Daemon 原生 supervisor 使用同一 version-1 换行 Host envelope，
配合私有 owner/token 凭据和 Job/Attempt/generation 目标。它不使用 Guest
`Hello`/`Sync`，也不使用 CLI worker ticket 机制。该线上格式与旧 supervisor
不兼容。升级前使用旧二进制排空 sandbox；同样先排空活动 CLI 请求并停止
旧 Job listener，再替换程序。新客户端在提交描述符或命令之前拒绝不兼容
的 live listener。不提供 legacy fallback，也不透明接管旧 supervisor。

### 取消与验证限制 {#host-limits}

前端在启动和准入之前锁存 SIGINT/SIGTERM/SIGHUP，发送关联取消，并负责
恢复终端及回收 worker。listener 持有请求清理所有权直到 worker 完成，
可升级清理强度。Linux 实现 subreaper 收养、`/proc` 后代跟踪及基于 pidfd
的信号发送，并将 listener 排除在请求清理之外。macOS 跟踪出生身份已确认
的后代及已知工作负载进程组，可跨普通进程组变化。清理先冻结 root 与发现
的 forker，反复扫描直到跟踪集合稳定，再逐个发送经出生身份复核的信号。
清理范围包含 worker 进程组之外跟踪到的后代。不保证拥有发现前已 reparent、因而漏掉的孤儿；
libproc 身份检查后按数字 PID 发信号，不是原子 pidfd 操作，也不提供
Linux 等价的 containment。macOS 清理路径尚未编译或测试。

持久 listener 提供请求接入，不持久化请求队列。`request_id` 关联响应、错误、ticket 与
取消；通用去重和 exactly-once 不在其合同范围内。部分持久 Job 操作保留自己
范围内的回执，但不覆盖所有 Host 命令。断连、超时和取消可能发生在副作用
之后；前端报告不确定性，不自动重试。决定再次提交前，先核对 Job 状态和
产物。

Host AgentCtl 路径尚无真实 VM TUI 端到端验证。现有传输、进程或 mock 检查的
验证范围不含 guest 正确性、生产级持久性或完整平台行为；macOS 身份与清理
路径的限制见上方说明。

## 一条生产执行路径

```text
CLI／嵌入调用方
  → RunSpec
  → pvisor：准入、策略改写、Placement → 有效 Operation
  → Session：准备驱动与运行资源
  → 提交启动事实
  → RunExecutor::execute
  → 清理、检查控制观察、保存结果、公布终态
```

`RunSpec` 是调用方的执行配置输入；`Operation` 是结构化的操作描述。目前唯一生产操作是 `run.execute`，包含程序、参数和工作目录。执行器实际消费有效 RunSpec 及准备好的驱动附件。`PVisor::resolve_operation` 使用同一准入路径，供启动前审查，不能代替实际执行及安装证据。

准入保留请求与有效操作的快照。实际策略变化记录为 Rewritten，选定的 VM／Overlay 放置记录为 Placed。拦截发生在实际驱动边界：文件操作进入 OverlayFS／OverlayCore，网络流量进入 OverlayNet。它们共享策略定义，但当前并没有将每个文件或网络操作都提升为独立的公共 Operation。

例如，网络驱动把请求的 Ambient 能力收窄为 Deny：Requested 保留原始权限，Rewritten 保存收窄前后快照，Placed 描述最终放置，执行器按有效配置运行。这些快照记录实际策略处理；通用规则解释器不在当前实现范围内。

## 一个生命周期所有者

当前每次 `PVisor::run` 创建一个 Attempt，由 pvisor 中的 `Session` 统一持有和管理；Job、Run 与 Attempt 的身份区分见[执行模型](../design/execution-model.md)。

Session 负责驱动准备、Guest AgentCtl server、取消与超时、执行后清理、观察检查、Bundle 保存及终态公布。执行器返回 `ExecutorOutput`，不自行分配 Job／Attempt 身份或公布终态。`RunHandle` 提供状态、取消、checkpoint 和事件订阅；取消请求不等于执行已经停止。

Process／VM 清理其受管理的进程组；容器使用 runtime 的终止接口。进程组之外的后代和各平台隔离缺口见[隔离设计](isolation.md)。Guest AgentCtl 负责工作负载协作和 checkpoint 静默点，本身不是强制控制；Host AgentCtl 则是上方描述的独立宿主权限路径。

## Event 是观察接口

外部观察的是 Event，不需要依赖 Session 的内部字段。事件链、身份、因果引用与记录边界见 [Operation 与 Event](operations-events.md)。

当前实现先准备驱动，再提交启动事实，最后调用执行器。必要启动事实提交失败会阻止执行器派发并清理准备资源；准备阶段仍可能产生文件或 socket 副作用。

## 策略、控制与证据

策略、准入计划与执行后观察是三个独立层次，不能互相替代；`ExecutorPlan`／`ExecutorObservations` 的等级与证据口径见[能力与证据](../concepts/capabilities-and-evidence.md)。

core 共享文件／网络策略求值。user、workspace、session 和执行器基础策略共同约束权限，后面的 allow 不能覆盖前面的显式拒绝。实际授权、拦截和控制安装由 pvisor 与驱动落实。

## 记录与文件应用

| 记录 | 回答的问题 |
| --- | --- |
| `run.json` | 这项 Job 的身份、状态、执行器及本地资源是什么？ |
| Run Bundle | 结果、控制观察、产物及文件／网络摘要是什么？ |
| Event Journal／Trace | 已发布了哪些事实，它们如何关联？ |
| Overlay diff／preimage | 哪些文件仍待审查，应用时应检查什么原始状态？ |

这些记录各有范围。Event 可以重建已观察到的操作过程，无法仅凭日志恢复全部外部状态。Agent 原生轨迹 replay 也不是任意副作用的确定性重放。

暂存文件先审查再应用。OverlayCore 负责目标校验、持久化应用意图、更新及恢复；一个批次不是原子文件系统事务。`apply`、`drop` 与文件检查点的恢复范围及不能撤销的外部副作用见[能力与证据](../concepts/capabilities-and-evidence.md)，操作流程见[审查与应用](../guides/review-apply.md)。
