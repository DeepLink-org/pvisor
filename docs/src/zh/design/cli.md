# pVisor 命令模型

Job 是 CLI 的核心对象：一次受管理的命令、执行证据和暂存改动。`pvisor run` 创建
Job；`status`、`kill`、`inspect`、`fork`、`apply`、`drop` 直接操作 Job，
命令保持扁平。`replay` 从已有轨迹创建 Job。
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

## 配置优先级

`--config` 接受 TOML `RunConfig`，`--spec` 接受准备好的 JSON `RunSpec`。显式 scalar 选项覆盖文件值；
重复的 list 选项替换整个列表；`--` 后的命令替换 `run.command`。`--container-image` 与
`--rootfs` 可以推断匹配的 executor；自动化场景仍建议显式指定 `--executor`。

公共工作流保持简单：启动 Job，检查 Evidence，然后明确决定 staged effect 的去向。Provider
行为见[执行环境](../guides/execution.md)，完整选项见 [CLI 参考](../reference/cli.md)。

## 核心命令与伴随工具

`pvisor` 内置 `run`、`status`、`kill`、`inspect`、`fork`、`apply`、`drop`，
另提供帮助与 `extensions` 列表。单独安装核心即可管理完整 Job 生命周期。

`pvisor-tui` 属于 `pvisor-tui`，`pvisor-replay` 属于 `pvisor-replay`；
它们依赖核心，核心不依赖它们。`pvisor-cache` 前端仍在核心包中，因为 OCI 与懒加载缓存
仍由执行器使用。wheel 安装四个二进制到同一目录。

伴随命令的名称与描述来自静态表。只查核心同目录的三个第一方命令，不搜索 PATH，
不扫描二进制 manifest，不计算文件摘要或传递启动器 evidence。
安装目录与可执行文件仍须归当前用户或 root 所有，且不得 group/world 可写；拒绝符号链接。
派发使用 Unix `exec`，保留参数、stdio、信号与退出码，伴随命令不能覆盖核心命令。
`pvisor help NAME` 支持伴随工具；`run --tui` 和交互式审批转交 TUI。

核心默认构建不包含 Gateway。启用捕获使用 `--features gateway`；wheel 构建启用该 feature。
无捕获时，普通显式代理仍由 OverlayNet 授权与转发。请求未编译的捕获或 Gateway debug 能力会报错。

生命周期只使用 `Session`；旧的 `ExecutorSession` 和 `AttemptContext` 别名已删除。
使用现有 `RunHandle` 的状态、取消、checkpoint 和事件 API，不提供测试专用的 Hook/Control 协议。
AgentCtl 保留工作负载协作职责。终态仍在驱动清理、结果持久化和终态事件提交后公布；
追加结果不确定时，不发送冲突的替代终态事件。
