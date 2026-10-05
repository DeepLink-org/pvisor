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

顶层按所操作的对象组织：Job 生命周期与工作区命令保持扁平，部署/集群/节点资源集中在 `service`；`replay` 从轨迹创建 Job，`tui` 是可选交互前端。主帮助按 Jobs、Filesystems、Extensions 分组。Jobs 包含 `checkpoint`；Extensions 包含 `service`、`replay`、`tui`，可选伴随命令安装后显示。`extensions` 命令已删除。

| 职责 | 入口 |
|---|---|
| Job 生命周期与分支 | `run`、`status`、`kill`、`suspend`、`resume`、`fork` |
| 查看、审查与接受文件变化 | `inspect`、`review`、`apply`、`drop` |
| Job 不可变检查点 | `checkpoint` |
| 轨迹回放与交互前端 | `replay`、`tui` |
| 部署角色生命周期 | `service run/status/restart/stop --config FILE` |
| 集群任务/控制与执行节点 | `service cluster`、`service worker` |
| 不可变环境与实验冷页池 | `service cache`、`service memory-pool` |

`status --review` 保留为已存在的快捷形式；`review` 仍是详细审查入口。`run --tui` 是主要交互路径，顶层 `tui` 保留显式前端调用。暂不引入另一个 `job` 命令层，避免让相同 Job 操作形成两套语法。

资源工具使用 `service` 子命令。原生执行状态捕获、恢复和存储管理使用 Job 命令，能力由实际 VM profile 决定。当前命令以外的名称按普通默认执行规则处理，不保留旧命令别名或迁移处理逻辑。

```bash
pvisor service --help
pvisor service cluster --help
pvisor service worker --help
pvisor service cache --help
pvisor service memory-pool --help
pvisor help service cluster submit
```

同目录的七个制品为 `pvisor`、`pvisor-cluster`、`pvisor-worker`、`pvisor-cache`、`pvisor-memory-pool`、`pvisor-replay`、`pvisor-tui`。资源 companion 是 service 子命令的实现；单独核心仍可管理 Job，没有安装对应 companion 时服务工具明确报错。Supervisor 与数据角色保留独立进程，CLI 合并不改变故障边界。

工具来自静态表，只查可信安装目录，不搜索 PATH 或执行 discovery。安装目录与可执行文件归当前用户或 root 所有，不得 group/world 可写，拒绝符号链接。Unix `exec` 保留参数、stdio、信号和退出码；子命令自己的 `--help`/`--version` 原样转交，工具不能覆盖 Job 命令。部署与预算见[统一服务指南](../guides/cluster/service.md)。

核心默认构建不包含 Gateway。启用捕获使用 `--features gateway`；wheel 构建启用该 feature。
无捕获时，普通显式代理仍由 OverlayNet 授权与转发。请求未编译的捕获或 Gateway debug 能力会报错。

嵌入调用方通过 `RunHandle` 查询状态、请求取消、创建 checkpoint 和订阅 Event。
Attempt 生命周期由 Session 管理；AgentCtl 保留工作负载协作职责。
具体执行与终态处理见[核心架构](architecture.md)，记录与失败语义见 [Operation 与 Event](operations-events.md)。


完整 VM 快照接入 Job 的目标接口见[Job 检查点与分叉 CLI 设计稿](job-checkpoint-cli.md)。工作区命令和原生 VM execution 保存/恢复已接入；支持范围和验收见设计的第 10 节。独立 snapshot 已删除；[完整环境快照与迁移](environment-snapshot.md)说明存储 SDK 和历史证据的边界。
