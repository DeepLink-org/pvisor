# `pvisor` 命令参考

Job 是 pVisor 面向用户的核心对象：一次受管理的命令、执行证据以及暂存文件改动。
`pvisor run` 创建 Job；其余扁平命令直接操作 Job，不新增 `job` 子命令。
`replay` 从轨迹创建 Job。现有 Job ID 和磁盘记录仍保留
`run-*`、`Run Bundle` 等名称。
Host、OCI VM 和透明 host-rootfs VM 的完整命令示例见
[使用 pVisor 运行工作负载](../guides/execution.md)。

## 按任务查找命令

- **运行命令：** 从[`pvisor run`](../start/first-run.md)开始，再用 `status --review`、
  `inspect` 和 `apply` 决定哪些修改进入项目。
- **理解执行边界：** 使用 `status` 和 `inspect`，然后阅读[执行指南](../guides/execution.md)。
- **继续轨迹：** 只有在已有受支持轨迹时才使用 `replay`，先阅读[回放指南](../guides/sandbox-replay.md)。

第一次使用时，先复制最小闭环：

```bash
pvisor run --stage ./runs/task-001 -- codex
pvisor status --review last
pvisor apply last --path src
```

加上 `--tui` 可显示类似 Zellij 的终端边框、底部状态栏和浮动审查面板：

```bash
pvisor run --tui -- bash
```

默认键盘输入交给 shell 或 Agent，终端始终保持完整宽度。顶部显示工作目录及文件、网络边界；
底栏显示运行状态、时间和非零的访问异常，授权等待会突出提示。详细计数保留在面板中。
按 `Ctrl-]` 打开菜单，同一行切换为快捷键。
pVisor 自身的启动信息显示在
Log 面板，不混入 Agent 终端。按 `Ctrl-]` 进入命令模式，再按 `r`、`f`、`n`、
`u`、`l`、`p` 打开概览、文件、网络、Job、Log 或 Permissions 面板，按 `?` 查看按键帮助。在面板中用
Tab 或 `1`–`6` 切换视图，用 `j`/`k` 滚动，按 Esc 或 `Ctrl-]` 返回 Agent。
连续按两次 `Ctrl-]` 可将该按键原样发送给 Agent。

### 暂存与存储 {#暂存与存储}

下表描述默认 CLI 行为。显式配置的提交方式、可写共享和应用兼容策略可能改变写入去向。

| 运行方式或目录 | 写入去向 | 退出后的处理 |
| --- | --- | --- |
| 普通 host，未启用暂存 | 原工作区 | 已写入宿主；不能用 `drop` 撤销 |
| `--safe` 或 `--ask` 的工作区 | Job 存储中的写时复制 stage | 默认保留，供 `status --review`、`apply`、`drop` 使用 |
| 显式 `--stage PATH` 的工作区 | 指定 stage | 默认保留；该选项也启用暂存 |
| VM 工作区 | 指定 stage 或默认 Job 存储 | 默认保留 |
| VM 根目录的其他写入 | 私有临时 upper | VM 退出后丢弃 |
| `--safe` 的 HOME／`CODEX_HOME` 状态 | 独立私有 stage | 退出后丢弃，不进入工作区 Run Bundle |
| 显式 `--mount SOURCE:write` | 宿主 SOURCE | 直接持久写入，不经工作区 apply/drop |

Job 记录和 Run Bundle 保存在运行存储中。`--stage PATH` 选择位置，
不是 `--safe/--ask` 保留工作区改动的前提。文件审查不覆盖已经发生的远程副作用。

### 文件系统参数 {#文件系统参数}

macOS 的 macFUSE 临时工作区默认以启动 pVisor 时的当前目录作为 lower；
`/Volumes/pvisor-*` 是合并视图的挂载点，包含当前目录已有的文件。
显式配置工作目录或 OverlayFS base 时，以显式配置为准。


普通 host Job 默认将工作区写入直接透传到 lower。`--safe` 和 `--ask` 默认将工作区
改动保留在 Job 存储中，退出后用 `status --review`、`apply` 或 `drop` 手动处理。
`--stage PATH` 仅用于指定存储位置，不再是保留改动的前提。

```bash
pvisor run --stage ./run-stage -- codex
pvisor run --safe --mount /opt/zcode:read --mount /var/lib/zcode:write -- zcode
pvisor run --access '/workspace/**/.ssh:deny' -- zcode
pvisor run --access '.env:ask' -- codex
```

`--mount SOURCE:read` 向 host executor 授予原绝对路径的只读访问，要求 `--safe` 或 `--ask`；
`SOURCE:write` 直接修改宿主机。两者不支持改写 TARGET，不能与工作区、Job 存储或可写运行时路径重叠；
Linux 的私有 `/tmp` 内不能使用只读共享。显式共享不经过工作区 OverlayFS 的 ask 规则。
`--mount SOURCE[:TARGET]:stage` 则把 SOURCE 加入工作区写时复制视图的底层，并非独立目录挂载。
`--access PATH-GLOB:deny|ask|warn` 默认累加配置、预设和 CLI 规则：deny 拒绝、ask 询问、warn 放行并警告。
清空配置与默认文件保护必须显式使用 `--clear-access`；之后再加入 CLI 规则。deny > ask > warn。
原先表示警告的 `:read` 已拒绝，改用 `:warn`；真正只读请用 `--mount PATH:read`。

使用 `pvisor --ask -- bash` 可在命中 `ask` 文件规则或访问未列入规则的代理网络目标时询问权限；
`--ask` 同时启用 `--tui` 和 `--safe`。
指定 `--access ...:ask` 会自动启用审计 TUI 和 safe 暂存视图，无需另加 `--ask`
或 `--tui`。弹窗用 Tab 或上下方向键切换范围、保存期限和按钮，左右方向键选择，Enter 在按钮上确认；默认选中拒绝按钮。可按 `1` 选择仅此文件、`2` 允许同级目录、`3` 允许相同后缀；
`d` 拒绝此次目标。对于未列入规则的代理网络目标，`--ask` 的弹窗可按 `1` 仅允许
此目标，或按 `2` 允许当前域名及其子域名；两种选择都限定在当前端口和传输协议，
IP 地址不能使用域名范围。明确的 `deny` 规则仍然直接拒绝，不进入询问弹窗。
选择会写入当前 Job 目录的 `audit-policy.json`，之后命中相同范围时自动应用；
每次决策记录在 `audit.jsonl`。已保存决定只用于命中 ask 的访问，不能覆盖静态 deny 或外层沙箱。
在 Permissions 面板用 `j`/`k` 选择决定，按两次 `x` 移除；之后回到更宽范围规则或重新询问。
移除不会关闭已经打开的文件句柄；其他已运行的 TUI 在下次启动时加载更新。这些记录默认在 Job 结束后保留，`--stage PATH` 可指定位置。
代理网络审计属于协作式边界：未经过代理的直接连接不会触发此弹窗。

```text
pvisor
├── run                 创建 Job
├── apply               提交已停止 Job 的暂存改动
├── drop                丢弃已停止 Job 的暂存改动
├── status              查看 Job 状态和审查证据
├── kill                请求终止正在运行的 Job
├── fork                从已停止的 Job 创建子 Job
├── inspect             只读查看 Job 的文件系统
└── replay              从 Agent 轨迹创建 Job
```

## 安全的第一次运行 {#安全的第一次运行}

```bash
pvisor run --safe --stage ../stage-001 -- codex
pvisor status --review last
```

默认 host 执行保留宿主机文件系统视图；`--filesystem sandbox` 才启用 pVisor 的
synthetic-root/Landlock 或 Seatbelt 文件系统访问策略。
`--safe` 默认暂存工作区，并给 HOME（包括在 shell 内启动的 Codex）提供独立的写时复制视图。
没有 `--stage` 时，changeset 和 Run Bundle 默认保留在 Job 存储中，退出后可 review/apply/drop。
显式 `--stage <PATH>` 会保留 Job 和可写 stage，
改动可供人工审查，并以 `0600` 写入 `run-bundle.json`。

`--strict` 要求每个被请求的 capability 维度都有不可绕过的 enforcement 证据，
否则在 Agent 启动前失败关闭。当前 host / container / VM 都会请求 Network 与
Subprocess，且无一 claim Subprocess，因此 `--strict` 在这些路径上会以
`UnsupportedPolicy` 退出。该旗标用于验证 fail-closed，不表示「更强沙箱已就绪」。
在 Linux 上，`--filesystem sandbox` 会使用 pVisor 的 rootless launcher，启用
User/mount/PID namespace、最小 bind-projected root、`chroot` 和按内核协商的
Landlock 策略。`--overlaynet-deny-all` 独立增加私有 network namespace；public/allowlist
代理模式仍是协作式。在 macOS 上，host executor 只在请求文件系统 sandbox 或网络隔离时安装生成的
Seatbelt 策略；文件系统策略与网络策略相互独立。对 deny-all Run，它拦截 IP 和
ambient host Unix socket，同时保留精确的 AgentCtl 与 Run 本地 IPC。读取和
选择性网络策略仍是 ambient/协作式，并在 Bundle 中单独标注。原生 OCI 和 libkrun
executor保留同样的外层 Run、OverlayFS 和 AgentCtl 状态观察。

完成后：

```bash
pvisor status --review last
pvisor fork last -- codex
pvisor apply last --all # or: pvisor drop last
```

`fork` 会先为已停止 Job 的暂存文件系统创建快照，再启动子 Job。
传入 `--checkpoint ID` 可复用已有逻辑检查点。嵌入式 host 可以调用
`RunHandle::checkpoint`：pVisor 发布 AgentCtl quiesce 指令，要求每个被冻进
checkpoint 的 Session 报告匹配的 quiesced 状态，快照 raw upper，再发布
`continue`。逻辑 checkpoint 保留文件系统和协作客户端 safe-point 边界，不
保留进程内存。

要结束正在运行的 Job，使用 `pvisor kill JOB_ID`。它向 Job 的监督进程请求正常
终止；用 `pvisor status JOB_ID` 查看最终状态。已停止的 Job 仍可审查并选择应用或丢弃。

## `--safe` 参数预设

```bash
pvisor run --safe -- claude
pvisor run --safe --vm --rootfs image=my-agent-image:latest -- codex
pvisor run --safe -- zcode
pvisor run --safe --overlaynet-allow inference.example.com:443 -- zcode
```

`--safe` 生成一组命令行参数补丁，经过同一个 CLI 解析器后应用，再应用用户显式参数。
预设按可执行文件名匹配 Agent，自动放行对应 API 的 HTTPS 目标。`--safe` 要求所选执行器落实隔离，不选择 executor。
优先级是 **显式 CLI > safe 预设 > 配置文件 > 普通默认值**。
支持普通命令和 TOML `--config`；已准备好的 JSON `--spec` 不接受该预设。

`--safe` 直接要求落实文件读取、写入和网络隔离，不允许静默回退到普通 host 进程。
不引入额外的 sandbox 命令行参数或配置项。`--strict` 仍是对全部请求能力的校验，
含资源限制等，和该隔离要求不同。

- macOS host：强制 Seatbelt 读取/写入范围，只允许连接 pVisor 分配的 loopback TCP 代理端口；
  阻止其他直接 IP 出口和环境中的宿主 Unix socket，仅保留必要的 Run 内 IPC。
  Agent 使用临时 HOME，不能直接读取原来的主目录；凭据需显式传入或由 Gateway 持有。
  系统运行库和启动所需的路径元数据仍可读取。
- Linux host：`--safe` 要求 rootless namespace、synthetic root、chroot、Landlock
  和 HOME 写时复制视图。选择性出口和 Gateway
  通过 supervisor loopback proxy cooperative 转发，直接 socket 仍可能绕过；需要不可绕过
  网络边界时使用 VM 或 deny-all。启动器通过私有 stage 投影宿主 HOME；overlay deny 规则
  不会隐藏工作区视图之外原路径上的秘密文件。
- VM：要求现有 `auto` 网络边界；safe 不自动选择 VM。
- container：当前缺少完整强制边界，`--safe` 拒绝启动。

隔离安装失败会停止运行。`--safe` 不能与 `--overlaynet off` 同时使用。


预设仅自动放行下表中的 HTTPS 目标（443 端口），其他目标默认拒绝。

| 命令 | 默认放行目标 |
|---|---|
| `codex`、`bash`、`sh`、`zsh`、`fish` | `api.openai.com`、`chatgpt.com`、`ab.chatgpt.com` |
| `claude` | `api.anthropic.com` |
| `gemini` | `generativelanguage.googleapis.com` |
| `zcode` | `api.z.ai`、`open.bigmodel.cn` |

未知命令默认拒绝出站。通过 `--overlaynet-allow HOST:PORT` 显式设置授权（覆盖预设列表）；
已有拒绝规则和限速继续生效，Gateway capture 使用显式配置的路由。

未使用 `--safe` 时，Codex 状态和项目写入会到达宿主 lower。使用 `--safe` 时，
工作区进入可审查的 Run stage；HOME（及显式设置的 `CODEX_HOME`）使用单独的私有 stage。
HOME 状态改动在 Run 结束后丢弃，不包含在工作区 Run Bundle 中。

独立于 `--safe`，Linux rootless host 直接运行 `zcode` 时会应用兼容策略：继承宿主环境变量，
允许直接持久写入已存在的 `~/.zcode`、`$XDG_CONFIG_HOME`（或 `~/.config`）和
`~/.local/share/applications`。找到随包 Chromium setuid helper 时，pVisor 会隐藏它并加上
`--no-sandbox`，同时加上 `--disable-gpu`。这条路径会关闭 Chromium 自身沙箱，
但保留 pVisor 外层 rootless 边界。这些应用状态写入绕过工作区 stage，带 `--safe` 时也一样。
策略按直接可执行文件名匹配，shell 包装器不会触发。`zcode-bigmodel` Gateway profile
是独立的路由适配。

预设通过 `--clear-pass-env` 清空配置文件中的 `run.pass_env`；
`--clear-pass-env` 也可单独使用，之后的显式 `--pass-env NAME` 仍然生效。
直接运行 Codex 时保留宿主环境继承，以维持账号与路由配置；其他命令默认关闭继承，
凭据通过显式 `--pass-env NAME` 授予。
对应的显式 CLI 参数可以重新授予或覆盖。`--safe` 默认暂存工作区；
已有容器挂载和文件系统底层仍保留；项目 base、rootfs、executor 不变。
需要向 Agent 交付凭据时显式使用 `--pass-env`；使用已配置的 Gateway 可由可信侧持有上游 Key。

启动时会打印实际策略及覆盖提醒。必须注意：

- 不使用 `--safe` 时，host/container 选择性代理仍可绕过。
- 域名规则无法区分同域名下的推理、遥测与上传 API，也无法阻止内容被夹带在模型请求中。
- 通配符规则覆盖 OverlayFS 视图；Linux 上投影的 HOME 可能仍暴露原路径上的敏感文件，显式授权的额外路径也需单独保护。
  文件名规则不能识别改名副本、源码中的密钥或 Git 历史中的内容，也没有批量读取或 tool-call 关联监控。
- 扩大共享范围或选择 `--rootfs host` 会增加可访问的数据；`--safe` 不能与关闭 OverlayNet 同时使用。

## 文件访问规则

```bash
pvisor run --safe --mount /opt/tool:read --access '**/.ssh:deny' --access '**/.env*:warn' -- my-agent
```

`--mount SOURCE:read|write` 是 host executor 的显式共享，分别授予只读或持久写入权限；
只读共享要求 `--safe` 或 `--ask`。`--mount SOURCE[:TARGET]:stage` 是工作区视图的底层组合。
`--access PATH-GLOB:deny|ask|warn` 追加文件规则，不会替换默认保护；`--clear-access` 才会显式清空。
`warn` 仅告警，不是只读；文件询问授权包含视图内的检查、读取、修改和删除。

规则相对于挂载根目录匹配：`*` 不跨目录，`**` 可跨目录，匹配目录时覆盖全部后代。
为防止大小写不敏感文件系统上的别名绕过，匹配不区分大小写；绝对路径、空规则及 `.`/`..`
路径分量无效。deny 优先于 warn。命中 deny 的路径在目录枚举中隐藏，访问、创建和修改被拒绝；
warn 放行并在监督进程 stderr 打印路径，不打印文件内容。告警表示文件系统访问尝试（含元数据访问），
不是准确的内容读取计数；内核缓存可能合并访问。

`--safe` 的默认 deny 为任意层级的 `.ssh`、`.gnupg` 目录，以及 `id_rsa`、`id_dsa`、
`id_ecdsa`、`id_ecdsa_sk`、`id_ed25519`、`id_ed25519_sk` 文件。
默认 warn 为 `.env`、`.env.*`、`*.pem`、`*.key`、`*.pub`、`*.p12`、`*.pfx`、
`.aws/credentials`、`.netrc`、`.npmrc`。`.ssh` 内公钥也随目录被隐藏；目录外公钥只告警。
这不保证识别所有私钥；自定义文件名需要增加规则。

TOML 中对应：

```toml
[filesystem]
stage = "../stage-001"
mount = [{ source = "/opt/tool", access = "stage" }]
access = [
  { path = "**/.ssh", level = "deny" },
  { path = "**/.env", level = "warn" },
]
```

显式 CLI 访问规则追加在配置文件与 safe 预设之后；deny 优先于告警。
规则随 Run 和 overlay 记录保存。

FUSE 与 VM virtio-fs 共用规则检查。开启 deny 时，本版保守拒绝所有多硬链接普通文件和新建硬链接，
避免通过别名读取；普通目录改名/交换/删除会检查受影响子树，含受保护文件时拒绝操作。
符号链接由挂载命名空间解析，文件打开不跟随最终符号链接到后端原始文件。
VM 对根视图也应用规则，并保护工作区原始路径和 overlay 后端目录。
这不替代执行器隔离：host 的环境读取权限、容器额外分享和未经该视图的凭据仍须单独控制。
底层目录应由可信监督进程管理，规则不承诺抵抗宿主其他进程同时改写底层文件的竞态。

## 回放一条 Agent 轨迹 {#replay-an-agent-trajectory}

`pvisor replay` 假定调用方已经正常创建了新 sandbox。它通过 `after_step`
回放完整 tool batch，用新鲜 observation 重建所选 Agent 原生上下文，然后
启动 live Agent：

```bash
pvisor replay \
  --agent claude-code \
  --trajectory /input/session.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/claude \
  --boundary-user-prompt 'Review the fresh observation before continuing.'
```

OpenHands、mini-swe-agent、Pi agent、OpenCode、Codex 和 SWE-agent 使用环境中已有的模型端点
和凭据。Pi 要求精确的 `0.83.0` runtime，并接受包含核心 `read`、`bash`、
`edit` 和 `write` 工具的原生 RPC event JSONL。Claude Code 使用
SandboxReplay 拥有的临时 bridge，因为它的原生 resume 传输会插入 wake-up
消息。该 bridge 在转发模型请求前校验并去掉那份精确的 Resume Transport
envelope。它不启用 pVisor Gateway、不捕获模型流量、也不持久化 bridge
审计。

OpenCode 要求精确的 `1.17.7` runtime，轨迹格式为
`opencode run --format=json` 的事件 JSONL；Codex 要求精确的 `0.149.0` runtime，
轨迹格式为 Codex rollout `response_item` JSONL。两者均在新沙箱中重建原生前缀，
并调用各自的原生 resume 命令续跑。
Codex 的 native session ID 从轨迹 `session_meta` 提取；`session_id` 只作为模型
路由/Run 标识，不能覆盖该 native session。缺少 native session 时会 fail-closed。

等价的严格 replay TOML 是：

```toml
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 30
agent_entrypoint = "/usr/bin/claude"
max_steps = 200
session_id = "task-291-attempt-1"
replay_only = false
disable_thinking = true
boundary_user_prompt = "Review the fresh observation before continuing."
```

Pi 使用同一套 CLI/TOML 面。runtime 安装在 `/opt/pi-agent` 时，例如：

```bash
pvisor replay --agent pi-agent \
  --trajectory /input/pi-agent.events.jsonl \
  --after-step 30 \
  --agent-entrypoint /opt/pi-agent/bin/pi
```

Replay 有三种模式。默认回放前缀并继续；`--replay-only` 执行前缀并在模型
请求前停止；`--prepare-only` 构造前缀，不执行工具、也不要求 runtime。
`--max-steps` 是包含已回放动作的总动作预算。
`--allow-stale-observations` 是显式的仅 Claude 逃生口，会把 v3 结果标为
`degraded`。

`--boundary-user-prompt TEXT` 在最后一条新鲜 observation 之后、第一次 live
模型推理之前追加一条用户消息。TOML 写法是 `replay.boundary_user_prompt`。
prepare-only 和 replay-only 模式下它不参与推理；省略该选项则保持未修改的
replay 边界。结构化结果和 replay journal 只存储注入状态、长度和 digest；
Agent 原生的 prepared 或 continued 轨迹可以包含这条用户消息。

结果 schema 是 `sandbox-playback.result/v3`，带类型化的 `phase`、`quality`
和 `agent_status` 字段，以及 state/output 位置、artifacts 和可选结构化失败。
原先只用 `replay_only = true` 来构造前缀的非 Claude 调用方必须迁移到
`prepare_only = true`。

`disable_thinking` 属于 `[replay]`，也暴露为 `--disable-thinking`。Claude Code
由协议 bridge 将其应用到上游请求；OpenCode 设置后会省略 `--thinking`。该选项
不会打开 Gateway capture。可选的 `[run]`、`[overlayfs]` 和 `[overlaynet]` 段会
创建外层受管 `pvisor run`；它们不改变内部 replay 模型路径。

默认情况下，replay 的内部状态、WAL、manifest、新鲜 observation 比较和原生
工作文件留在 `/tmp/pvisor-sandbox-replay`，并随 sandbox 消失。Replay 不启用
pVisor Gateway、模型流量 capture store 或 Claude Resume Transport 审计。显式选择 `--state-dir` 或 `--output-dir` 的调用方拥有这些
文件。用 `--replay-only` 执行前缀并在 live 推理前停止，或用 `--prepare-only`
在不执行的情况下构造它。

## 一套配置模型

`pvisor run` 只有一份规范 `RunConfig`。CLI 覆盖常用字段，但 `run.inherit_env`
当前没有对应的直接 CLI 开关。而且 `apply_safe_defaults` 当前会在未指定 `--safe` 的
非 Codex CLI 命令上清除环境继承；直接 `zcode` 的 host 适配又会重新启用它。
这些 CLI 路径上的 TOML `inherit_env` 目前不能按配置值生效。
`--config` 读取显式声明的 TOML `RunConfig`；`--spec` 要求准备好的 JSON
`RunSpec` 用于委托执行，不能与其他 Run 覆盖项组合。pVisor 不会发现隐藏的项目配置文件。

```bash
pvisor run \
  --name my-agent \
  --stage ../stage-001 \
  --mount /opt/tool:stage \
  --access '**/.ssh:deny' \
  --overlaynet-allow api.openai.com:443 \
  --overlaynet-deny 169.254.0.0/16 \
  --overlaynet-limit 10mbps \
  --gateway-mode capture \
  --gateway-level dialogue \
  --gateway-route \
    'name="openai", provider="openai", upstream="https://api.openai.com/v1", api_key_env="OPENAI_API_KEY"' \
  --record-destination ./capture \
  -- my-agent
```

`--record-destination` 将 `pvisor_core::event::Event` 写入本地 Journal
`events.trace.jsonl`，不支持旧 JSONL。Journal position 定义追加顺序，
`caused_by` 定义因果关系；`observed_at_unix_ms` 是观测元数据，不是顺序事实源。

等价 TOML 是：

```toml
# host（默认）或 sandbox；与 OverlayNet 和 OverlayFS 暂存相互独立
filesystem = "host"

[run]
agent = "my-agent"
executor = "host"
command = ["my-agent"]

[filesystem]
stage = "../stage-001"
mount = [{ source = "/opt/tool", access = "stage" }]
access = [{ path = "**/.ssh", level = "deny" }]

[overlaynet]
mode = "proxy"
policy = "allowlist"

[[overlaynet.rules]]
host = "api.openai.com"
ports = [443]

[[overlaynet.deny]]
host = "169.254.0.0/16"

[[overlaynet.limits]]
bytes_per_second = 1250000

[gateway]
mode = "capture"
level = "dialogue"

[[gateway.routes]]
name = "openai"
provider = "openai"
upstream = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

[record]
destination = "./capture"
```

用 `pvisor run --config run.toml` 运行。显式 CLI 标量替换 TOML 标量。网络和 Gateway
列表选项替换配置中的完整列表；文件系统的 `--mount` 和 `--access` 追加到配置条目。
`[filesystem]` 的序列化字段 `stage`、`mount`、`access`、`max_size`
分别对应 `--stage`、`--mount`、`--access`、
`--overlayfs-max-size`。`--` 之后的命令替换 `run.command`。
大小限制在运行结束后检查，因此不限制 Agent 运行期间的峰值占用。

`--container-image IMAGE` 自动选择原生 OCI container executor；
`--executor container` 让选择显式。传输层生成标准 OCI bundle，解析匹配的静态
`linux-amd64`/`linux-arm64` pVisor，挂进 rootfs，设置 process args，并走普通
`pvisor run --executor host --spec ...` 路径。Agent 命令放在 RunSpec
内，而不是暴露在 OCI runner argv。注入的 pVisor 创建自己的 AgentCtl 并
返回类型化 RunResult。最终 OverlayFS cwd 和会话 Gateway 配置挂在稳定路径。
`--container-rootfs PATH` 可直接指定已有 rootfs；否则 pVisor 使用自带 OCI
image store 准备 `--container-image`。运行时必须是 `runc` 或 `crun`，不再调用
Docker/Podman。用户 mount 是可重复的 TOML inline table，例如：

```bash
pvisor run \
  --container-image example/codex-agent:latest \
  --container-pvisor-binary ./dist/pvisor-linux-amd64 \
  --container-platform linux/amd64 \
  --container-network none \
  --container-mount \
    'source="/host/cache", target="/cache", read_only=false' \
  -- codex
```

进程内 Gateway 和显式 OverlayNet 代理当前要求 `container.network = "host"`，
因为它们注入的地址是 host loopback 端点。关闭这些 driver 时，`none` 模式有效；
`bridge` 需要外部 CNI 配置，当前会被拒绝。executor 记录 container 隔离，但不声称完整 capability
enforcement。

`--executor vm` 使用静态链接的 libkrun 及其嵌入 init 启动最小 Linux guest。
`--rootfs image=<IMAGE>` 选择该 executor，并直接拉取 OCI/Docker 镜像，不调用 Docker、
Podman 或 Buildah。未提供显式 rootfs 或镜像时，Linux 上默认通过 virtiofs 和
OverlayFS 使用宿主 `/`，保留宿主运行环境、PATH 和 HOME，不拉取镜像。
macOS 上需要显式提供 Linux rootfs 或镜像。
manifest 和 layer digest 会被校验，host 架构选择 `linux/arm64` 或
`linux/amd64`，解包后的 rootfs 成为 pVisor OverlayFS 的不可变 lower。
`--image-store` 覆盖平台缓存目录。OCI 缓存目标被标为不可变，且该保护在逻辑
checkpoint/fork 后仍然有效，因此 `pvisor apply` 不能改写被其他 Run 共享的
rootfs。

在 Linux 上，`--rootfs host` 选择 host `/` 作为 VM rootfs lower，并在省略
`--executor` 时选择 VM executor。`--rootfs <PATH>` 使用准备好的目录，
`--rootfs image=<PATH>` 使用 OCI 镜像或镜像路径；三者互斥，host rootfs 在
macOS 上被拒绝。这是统一 rootfs 语法；
`--mount SOURCE[:TARGET]:stage` 为 guest 工作区组合额外底层，当前工作区是隐式底层；
read/write 显式宿主共享目前仅支持 host executor。工作区改动进入指定 stage 或默认 Job 存储，退出后保留；
VM 根目录其他写入使用临时 upper，并在 VM 退出时丢弃。

合并后的 rootfs 是 guest `/`，`/workspace` 成为 guest cwd。在 Linux 和
macOS 上，vendored libkrun 通过 virtio-fs 直接服务 pVisor 的 rootfs 与
工作区 copy-on-write union。VMM 从不重新导出 host FUSE mount，也不物化或
对账这两棵树。Linux 使用 KVM，Apple Silicon macOS 通过同一 executor 使用
HVF。Linux 静态 musl 构建内嵌 guest 内核，运行时不需要固件共享库，
并拒绝 `--vm-library-dir`。macOS wheel 把 `libkrunfw.5.dylib` 安装在 pVisor
旁边；macOS 源码运行否则把 pinned 官方 release 下载到经 SHA-256 校验的
平台缓存，由 `/usr/bin/cc` 把预构建 kernel bundle 转成所需 dylib。
macOS 上可用 `--vm-library-dir` 选择已有固件目录。OverlayNet `auto` 使用不可绕过的 VM smoltcp IPv4 TCP/DNS driver，而
Gateway capture 使用经 guest virtual router 的内部路由。Linux 另外用
namespace 和 Landlock 约束 VMM。macOS VMM 仍拥有调用用户的 host 权限，因此
尽管有 guest-kernel 隔离，第一版 OCI-image 也不应被当成敌对多租户边界。

在 host/container 执行上，四个可见 OverlayNet 策略标志和 Gateway capture
会自动启用代理 driver。`--safe` 默认暂存工作区，`--mount` 添加显式底层；
工作区写入去向见 [暂存与存储](#暂存与存储)。当 stage 嵌在 base 或 compose 层内时，pVisor 从合并视图
隐藏该子树，并拒绝 guest 重建它。libkrun Run 不创建 live host mountpoint，
防止 host indexer 递归进入 `<stage>/merged`。反向拓扑——stage 包含 lower
层——会被拒绝。在 pVisor 能安全物化完整 merged-vs-base diff 之前，组合 Run
拒绝随后的 `pvisor apply` 命令。
host/container 的选择性网络规则作用于经过显式代理的流量。host deny-all 使用
namespace 或 Seatbelt 阻止直接出口；容器离线使用 `--container-network none`。
在 libkrun VM 上，`auto` 使用 smoltcp IPv4 TCP/DNS，`off` 让 guest 离线；
deny-all 仍允许已配置的内部 Gateway 路由。各路径的范围见 [网络边界](../guides/network.md)。

## Run 项目发现

当前目录是默认项目关联。`--mount` 指定额外宿主底层和可选的 Agent 可见路径。
每个 Run 在 pVisor 默认记录根目录下获得独立目录。若该根会落在
所选 OverlayFS base 或 compose 层内，pVisor 改用系统临时 Run 根，以保持
可写 stage 分离：

```text
project/                         # reusable workspace / default base

~/.pvisor/runs/
└── run-<uuid>/                  # one generated Run and default stage
    ├── run.json
    ├── run-bundle.json          # mode 0600; outcome + safety + changes + effects
    ├── overlay.json             # when OverlayFS is enabled
    ├── upper/
    ├── merged/
    ├── checkpoints/
    ├── lease.lock
    ├── control.sock             # while a live OverlayFS Run is available
    ├── .capture/                # when OverlayNet/Gateway is enabled
    └── events.jsonl             # when --record-destination is set
```

生命周期命令接受 Run id、Run 目录、项目工作区、`run.json`、upper 或 merged
路径。项目工作区选择其最新 Run：

```bash
pvisor status /path/to/project
pvisor inspect /path/to/project -- rg TODO .
pvisor apply /path/to/project --all
pvisor apply /path/to/project --path src --path tests/unit
pvisor apply /path/to/project --include 'docs/**' --exclude 'docs/generated/**'
pvisor apply /path/to/project --target /path/to/another-target --all
pvisor drop /path/to/project
```

`inspect` 创建单独的内核只读视图。`apply` 和 `drop` 拒绝改写 live Run。
过滤后的 apply 是依赖闭合且可重复的：未选路径保持 staged，不透明目录和
hard-link 组保持原子。每个成功 batch 持久化到 `apply-ledger.json`。
overlay 为每个被改写的目标路径记录 durable first-touch fingerprint。若所选
目标路径在 staging 之后发生变化，`apply` fail closed；已准备的 batch 向前
恢复，单个非目录替换用同目录原子 rename 提交。host 文件系统仍不为任意
多文件 batch 提供单一原子提交点。
提交全部剩余改动或丢弃 stage 是终态；`drop` 不能撤销已 apply 的 batch，
`apply` 也不能恢复已丢弃的改动。终态清理删除 `upper`、`work` 和其他一次性
staging 数据，但保留紧凑的 Run/Overlay 元数据、apply ledger 和 capture 产物。

### 共享镜像文件缓存

`pvisor cache serve` 在前台提供 OCI 镜像文件服务；`cache prepare IMAGE`、
`cache list DIGEST [PATH]`、`cache stat DIGEST PATH` 和 `cache read DIGEST PATH`
通过 `PVISOR_CACHE_SERVER` 访问它。默认使用用户缓存目录下的
`pvisor/cache.sock` Unix socket。服务端可用 `--image-store DIR`
指定已有 OCI store。文件读取支持分段和 SHA-256 校验。

VM 镜像启动会自动探测默认 socket；服务可用时，将远程镜像挂为只读 FUSE lower，
以 1 MiB 数据块按需读取并持久缓存。默认 socket 不存在或已失效时走本地 OCI 准备。
显式指定服务端后连接失败会报错；`PVISOR_CACHE_SERVER=off` 强制本地准备。
显式 rootfs 目录和原生 container executor 保持原有行为。
完整协议、限制和 SSH 远程访问方式见 [共享镜像缓存协议](https://github.com/DeepLink-org/pvisor/blob/main/docs/shared-image-cache.md)。
