# pVisor 命令模型

Job 是 CLI 的核心对象，命令保持扁平：`pvisor run` 创建 Job，`status`、`kill`、`inspect`、`fork`、`apply`、`drop` 直接操作它，`replay` 从已有轨迹创建 Job；Job、Run 与 Attempt 的关系见[执行模型](../design/execution-model.md)。
内部仍用 Run 记录保存 Job，配置类型仍是 `RunConfig`；配置文件必须显式传入，
不会被当作隐式项目策略。

## 使用 `run` 启动

短写法与完整写法等价：

```bash
pvisor -- codex
pvisor run -- codex
```

`--stage PATH` 将 changeset 保留在指定目录。普通 host Job 省略时直接写入工作区；
`--safe` 和 `--ask` 默认保留工作区暂存改动，生命周期见 [暂存与存储](../reference/cli.md#暂存与存储)。选中的 host、container 或 VM Provider 会在
Run Bundle 中分别记录实际 capability 与限制。

常用控制按目的分组：

| 目的 | 选项 | 结果 |
| --- | --- | --- |
| Filesystem | `--safe`、`--stage`、`--mount SOURCE[:TARGET]:read\|stage\|write`、`--access PATH-GLOB:deny\|ask\|warn` | 按需暂存改动并声明路径权限 |
| Runtime | `--executor host\|container\|vm`、`--rootfs`、`--container-image` | 选择执行 Provider 与 rootfs |
| Network | `--overlaynet-deny-all`、`--overlaynet-allow`、`--overlaynet-limit` | 请求 deny、allowlist 或限速策略 |
| Gateway | `--gateway-mode`、`--gateway-route`、`--gateway-level` | 配置路由，并按需捕获模型流量 |
| Limits | `--timeout`、`--memory`、`--max-processes`、`--max-open-files` | 在 Provider 支持时限制 Attempt |
| Configuration | `--config`、`--spec`、`--name`、`--pass-env` | 提供 RunConfig 或准备好的 RunSpec、身份和显式环境变量 |

Provider 选择不会改变 Run 契约，只会改变各 capability 维度的 enforcement 机制；最终
Evidence 会分维度记录。

## 查看并决定

完成后的 Job 仍然是一个记录，staged effect 必须显式接受或丢弃：

```bash
pvisor review last
# Compatibility entry: pvisor status --review last
pvisor inspect last -- git status --short
pvisor apply last --path src
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
pvisor apply last --all
# 或：pvisor drop last
```

`review` 和 `status --review` 解释历史 Run Bundle 的执行证据，并重新读取选定工作区的 staged change；`inspect` 在 Job 视图中执行只读命令；`apply`
提交选定路径并保留其余内容；`drop` 丢弃剩余文件变更，保留 Job 和 checkpoint。两者要求显式 Job 和已确认的停止终态。重置会
创建新的 stage generation，避免旧 metadata 覆盖新的决定。

## Checkpoint 与 Fork

`fork` 默认先为已停止 Job 的文件系统创建逻辑检查点，再启动子 Job（检查点范围见[执行模型](../design/execution-model.md)）：

```bash
pvisor checkpoint create last --request-id before-refactor --json
pvisor checkpoint list last --json
pvisor fork last --state workspace --stage ./stage/branch -- codex
```

嵌入式调用方可使用协作式 AgentCtl 协议，让参与的 session
先进入 quiesce，再保存检查点。

## 配置优先级

`--config` 接受 TOML `RunConfig`，`--spec` 接受准备好的 JSON `RunSpec`。显式 scalar 选项覆盖文件值；
重复的 list 选项替换整个列表；`--` 后的命令替换 `run.command`。`--container-image` 与
`--rootfs` 可以推断匹配的 executor；自动化场景仍建议显式指定 `--executor`。

公共工作流保持简单：启动 Job，检查 Evidence，然后明确决定 staged effect 的去向。Provider
行为见[执行环境](../guides/executors/index.md)，完整选项见 [CLI 参考](../reference/cli.md)。

## 核心命令与服务边界

顶层按所操作的对象组织：Job 生命周期与工作区命令保持扁平。Sandbox 生命周期及可选池所有权使用独立 `pvisor-daemon` 程序；OCI 缓存使用独立 `pvisor-cache`。`replay` 从轨迹创建 Job，`tui` 是可选交互前端。根帮助保持简洁，按 Execution（`run`、`status`、`kill`）、Changes（`review`、`apply`、`drop`、`inspect`）、Checkpoints（`checkpoint`、`suspend`、`resume`、`fork`）和 Tools（已安装的 `replay`/`tui` 伴随程序、`feature`、`help`）分组，提供 run-review-apply 示例，并提示用 `pvisor help COMMAND` 查看详情。

| 职责 | 入口 |
|---|---|
| Job 生命周期与分支 | `run`、`status`、`kill`、`suspend`、`resume`、`fork` |
| 查看、审查与接受文件变化 | `inspect`、`review`、`apply`、`drop` |
| Job 不可变检查点 | `checkpoint` |
| 轨迹回放与交互前端 | `replay`、`tui` |
| 本机 sandbox 生命周期 | 直接 `pvisor-daemon serve` 与 OpenSandbox profile HTTP API |
| 不可变镜像缓存 | `pvisor-cache prepare/publish/serve/list/stat/read` |
| 可选 daemon 自有共享页 | `pvisor-daemon serve --memory-pool`；独立 `memory-pool --directory DIR` 组件 |

`status --review` 是快捷形式；`review` 是详细审查入口。`run --tui` 是主要交互路径，顶层 `tui` 用于显式前端调用。Job 操作直接使用顶层命令，没有 `job` 命令层。

资源工具使用各自的可执行文件。原生执行状态捕获、恢复和存储管理使用 Job 命令，能力由实际 VM profile 决定。其他未知名称按普通默认执行规则处理。直接调用 `pvisor ctrl`、`pvisor ctrl --help` 和 `pvisor help ctrl` 会明确拒绝并给出迁移提示，不会按默认规则运行工作负载；`pvisor run -- ctrl` 仍表达显式工作负载意图，不是控制 API 别名。

```bash
pvisor-daemon serve --help
pvisor-daemon protocol
pvisor-cache --help
pvisor-daemon memory-pool --help
```

Sandbox 生命周期及可选池所有权直接调用 `pvisor-daemon`。缓存、replay 和 TUI 仍独立于 Job 命令；replay/TUI 伴随程序查找会明确报告工具缺失。Daemon CLI 构造原生 VM 运行时并派发 supervisor，同步内部 VM 派发先于 Tokio。Node 资源协议属于运行时，没有 daemon acquire/release 适配器。打包不暴露 staging/checkpoint API，也不自动获取 node 资源。

工具来自静态表，只查可信安装目录，不搜索 PATH 或执行 discovery。安装目录与可执行文件归当前用户或 root 所有，不得 group/world 可写，拒绝符号链接。Unix `exec` 保留参数、stdio、信号和退出码；子命令自己的 `--help`/`--version` 原样转交，工具不能覆盖 Job 命令。Sandbox 部署见 [daemon 运维](daemon/operations.md)，独立原生资源预算见[职责收敛](daemon/responsibility-convergence.md)。

核心默认构建不包含 Gateway。启用捕获使用 `--features gateway`；wheel 构建启用该 feature。
无捕获时，普通显式代理仍由 OverlayNet 授权与转发。请求未编译的捕获或 Gateway debug 能力会报错。

内置 Job CLI 操作以类型化请求经过按需启动的持久 Host AgentCtl listener；
前端启动授权请求 worker，listener 不重建 shell 命令。普通持久 Job 不需
端点参数。Live VM flags 是子命令局部选项：`status` 接受 `--vm-socket`、
`--vm-job-id`、`--vm-attempt-id`；`suspend` 接受这三个身份选项及
`--vm-pause`、`--vm-offload`、`--vm-ram-file`；`resume` 接受三个身份选项及
`--vm-load`。根命令前置 live VM flags，以及在其他命令上使用 live VM flags，
均被拒绝。三个身份选项必须一起提供，suspend/resume 的 Job 位置参数必须
与 `--vm-job-id` 相同。pause 与 offload 互斥，`--vm-ram-file` 要求 offload。
Live resume 继续同一个 Attempt；持久 Job 的 suspend/resume 保留执行检查点
捕获／恢复行为。见 [CLI 参考](../reference/cli.md#vm-instance-control)。

嵌入调用方通过 `RunHandle` 查询状态、请求取消、创建 checkpoint 和订阅 Event。
Attempt 生命周期由 Session 管理；Guest AgentCtl 保留工作负载协作职责，
与 Host 权限隔离。协议兼容、取消与升级限制见
[Host 与 Guest AgentCtl](architecture.md#host-agentctl)。
具体执行与终态处理见[核心架构](architecture.md)，记录与失败语义见 [Operation 与 Event](operations-events.md)。


完整 VM 快照的 Job 接口见[Job 检查点与分叉 CLI 设计稿](job-checkpoint-cli.md)。工作区命令和原生 VM execution 保存/恢复通过 Job 命令提供；支持范围和验收见设计的第 10 节。[完整环境快照与迁移](environment-snapshot.md)说明存储 SDK 和历史证据的边界。

帮助、版本和 feature 查询返回时不输出 startup 日志。执行命令仅在解析完成后、
派发 Job 请求前打出 `process.entry` 和 `cli.parsed`。标记名称不代表已测量
完整的进程入口或参数解析开销。

## 实现职责 {#implementation-ownership}

`cli/host.rs` 负责类型化 Job 派发及 live VM 选项校验；`cli/host_service.rs`
负责 listener 准入、兼容与 worker 授权。`cli/host_fds.rs`、`cli/host_cancel.rs`
和 `cli/host_process.rs` 实现描述符传递、取消及进程所有权；
`cli/host_image.rs` 在求摘要之前验证 macOS 已加载／磁盘 Mach-O UUID。
`runtime/host_transport.rs` 负责共享宿主权限路径、相同的有界 async/sync
换行 JSON framing 与 peer 检查，包含 Job 服务／内部 worker 的 frame；
FD marker 字节仍是独立的非 JSON 传输记录。内部 version-1 握手检查
Job ticket schema 及精确包版本／内容构建匹配。`JobCommand` 嵌入内部
CLI DTO，不是稳定公共 API；Core 拥有纯共享 Host envelope／supervisor
契约和校验，不包含 CLI DTO 或传输。

`cli/run.rs` 负责新 Job 的配置与启动；`cli/run/lifecycle.rs` 负责工作区分支与原生执行状态恢复；`runtime/job_execution.rs` 负责持久化 Job 状态、请求回执和原生终态确认，不会仅凭已保存的文件系统推断暂停成功。

Lazy 镜像所有权分开管理宿主 FUSE 挂载与 VM 直接后端附件。VM 准备路径返回直接附件，宿主卸载逻辑不会作用于该附件。Gateway 保存规范事件，在 actor 分发前排除 draft，不再提供 live Markdown 兼容选项或旧草稿投影命令。
