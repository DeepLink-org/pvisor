# pVisor 命令模型

Job 是 CLI 的核心对象：一次受管理的命令、执行证据和暂存改动。`pvisor run` 创建
Job；`status`、`kill`、`inspect`、`fork`、`apply`、`drop` 直接操作 Job，
命令保持扁平。`env` 管理 Job 使用的可复用环境，`replay` 从已有轨迹创建 Job。
内部仍用 Run 记录保存 Job，配置类型仍是 `RunConfig`；配置文件必须显式传入，
不会被当作隐式项目策略。

## 使用 `run` 启动

短写法与完整写法等价：

```bash
pvisor -- codex
pvisor run -- codex
```

`--stage PATH` 将 changeset 保留在指定目录。普通 host Job 省略时直接写入工作区；
`--safe` 使用临时 stage，并在 Job 结束后自动丢弃。选中的 host、container 或 VM Provider 会在
Run Bundle 中分别记录实际 capability 与限制。

常用控制按目的分组：

| 目的 | 选项 | 结果 |
| --- | --- | --- |
| Filesystem | `--safe`、`--stage`、`--mount SOURCE[:TARGET]:read\|stage\|write`、`--access PATH-GLOB:deny\|ask\|read` | 按需暂存改动并声明路径权限 |
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
pvisor status --review last
pvisor inspect last -- git status --short
pvisor apply last --path src
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
pvisor apply last --all
# 或：pvisor drop last
```

`status --review` 解释 Run Bundle 与 staged change；`inspect` 在 Job 视图中执行只读命令；`apply`
提交选定路径并保留其余内容；`drop` 丢弃 stage。两者都不会修改正在运行的 Job。重置会
创建新的 stage generation，避免旧 metadata 覆盖新的决定。

## Checkpoint 与 Fork

`fork` 默认先为已停止 Job 的文件系统创建逻辑检查点，再启动子 Job；检查点不保存进程内存：

```bash
pvisor fork last -- codex
```

嵌入式调用方可使用协作式 AgentCtl 协议，让参与的 session
先进入 quiesce，再保存检查点。

## 可复用环境

`env` 为具名 stage 提供跨命令的稳定生命周期：

```bash
pvisor env create dev --target ./project
pvisor env exec dev -- make test
pvisor env shell dev
pvisor env inspect dev -- git status --short
pvisor env apply dev --path src
pvisor env drop dev
pvisor env delete dev --force
```

Environment 是持久 stage，不是常驻 VM。`start` 与 `stop` 控制是否接受新的 session；
`apply` 与 `drop` 完成决定后会推进 stage generation。

## 配置优先级

`--config` 接受 TOML `RunConfig`，`--spec` 接受准备好的 JSON `RunSpec`。显式 scalar 选项覆盖文件值；
重复的 list 选项替换整个列表；`--` 后的命令替换 `run.command`。`--container-image` 与
`--rootfs` 可以推断匹配的 executor；自动化场景仍建议显式指定 `--executor`。

公共工作流保持简单：启动 Job，检查 Evidence，然后明确决定 staged effect 的去向。Provider
行为见[执行环境](../guides/execution.md)，完整选项见 [CLI 参考](../reference/cli.md)。

## 可执行扩展

`pvisor tui`、`pvisor replay` 分别派发到 `pvisor-tui`、`pvisor-replay`。
构建、安装和 wheel 均一起交付三个二进制；`run --tui` 与交互式 `--ask`
也转交 TUI 扩展。`pvisor extensions` 以 JSON 列出安装路径与 manifest，
根命令帮助列出可用扩展。新增 CLI 功能优先使用 `pvisor-NAME` 可执行文件。

发现顺序为核心二进制所在目录、PATH 中的非空目录；内置命令名保留。
扩展在二进制中嵌入唯一 JSON 数据块：NUL + `PVISOR_COMMAND_MANIFEST_V1`
+ 换行，随后 JSON，再以换行 + `PVISOR_COMMAND_MANIFEST_END` + NUL 结束。
字段为 `schema_version`、`name`、`version`、`description`、`session_protocol`；
两个协议版本目前均为 1，name 必须匹配文件名后缀。JSON 上限 4096 字节，
可执行文件上限 256 MiB。发现时只读文件，不执行扩展；`--pvisor-manifest`
是扩展显式提供的 JSON 查询接口。派发使用 Unix `exec`，保留 argv、stdio、
信号与退出码。Rust 扩展可在入口使用 `persisting_pvisor::command_manifest!`
与 `manifest_requested` 嵌入并提供 manifest。
