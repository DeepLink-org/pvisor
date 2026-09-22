# `pvisor` 命令参考

`pvisor` 是单个 Run 和持久环境的产品命令。
Host、OCI VM 和透明 host-rootfs VM 的完整命令示例见
[使用 pVisor 运行工作负载](../guides/execution.md)。

## 按任务查找命令

- **运行命令：** 从[`pvisor run`](../start/first-run.md)开始，再用 `review`、
  `inspect` 和 `apply` 决定哪些修改进入项目。
- **理解执行边界：** 使用 `status` 和 `inspect`，然后阅读[执行指南](../guides/execution.md)。
- **保留工作区：** 用 `env create` 和 `env exec` 管理可复用的 staged environment，
  用 `env apply` 或 `env drop` 收尾。
- **继续轨迹：** 只有在已有受支持轨迹时才使用 `replay`，先阅读[回放指南](../guides/sandbox-replay.md)。

第一次使用时，先复制最小闭环：

```bash
pvisor run --stage ./runs/task-001 -- codex
pvisor review last
pvisor apply last --path src
```

下面的参考按 Run 生命周期组织；每组参数都配有验证下一步。

```text
pvisor
├── run                 execute one Agent Run
├── replay              replay and continue an Agent-native trajectory
├── env                 manage durable reusable environments
├── status              aggregate Run, filesystem, and network state
├── inspect             open a read-only Run view
├── review              review the durable Run Bundle
├── checkpoint          snapshot a stopped transactional upper
├── fork                start a child Run from a logical checkpoint
├── apply               commit a stopped Run's filesystem stage
└── drop                discard a stopped Run's filesystem stage
```

## 安全的第一次运行

```bash
pvisor run --stage ../stage-001 -- codex
pvisor review last
```

默认 host 执行使用 safe-best-effort 隔离；`--stage <PATH>` 才启用当前目录的
OverlayFS stage，在显式 `--stage` 路径创建独立
Run 和可写 stage，保留改动供人工审查，并以 `0600` 写入 `run-bundle.json`。

`--strict` 要求每个被请求的 capability 维度都有不可绕过的 enforcement 证据，
否则在 Agent 启动前失败关闭。当前 host / container / VM 都会请求 Network 与
Subprocess，且无一 claim Subprocess，因此 `--strict` 在这些路径上会以
`UnsupportedPolicy` 退出。该旗标用于验证 fail-closed，不表示「更强沙箱已就绪」。
在 Linux 上，默认 host executor 会在异步 runtime 到达 Agent 之前，通过
pVisor 的 rootless launcher 自执行。User/mount/PID namespace、namespace 内
PID 1 后代回收器、最小 bind-projected root 加 `chroot`、按内核协商的 Landlock ABI v1-v3
策略、关闭继承描述符、`no_new_privs` 以及空 capability 集，使工作区约束对
Agent 进程树不可绕过。
`--overlaynet-deny-all` 再加一个私有 network namespace；public/allowlist
代理模式仍是协作式。在 macOS 上，默认 safe host executor 安装生成的
Seatbelt 策略，使 staged 写入不可绕过。对 deny-all Run，它拦截 IP 和
ambient host Unix socket，同时保留精确的 AgentCtl 与 Run 本地 IPC。读取和
选择性网络策略仍是 ambient/协作式，并在 Bundle 中单独标注。原生 OCI 和 libkrun
executor保留同样的外层 Run、OverlayFS 和 AgentCtl 状态观察。

完成后：

```bash
pvisor review last
pvisor checkpoint last --name before-experiment
pvisor fork last --checkpoint before-experiment -- codex
pvisor apply last --all # or: pvisor drop last
```

CLI checkpoint 是 stopped-consistent。嵌入式 host 可以调用
`RunHandle::checkpoint`：pVisor 发布 AgentCtl quiesce 指令，要求每个被冻进
checkpoint 的 Session 报告匹配的 quiesced 状态，快照 raw upper，再发布
`continue`。逻辑 checkpoint 保留文件系统和协作客户端 safe-point 边界，不
保留进程内存。

持久环境拥有稳定名称和可复用 OverlayFS upper：

```bash
pvisor env create dev --target ./project
pvisor env exec dev -- make test
pvisor env shell dev
pvisor env inspect dev -- git status --short
pvisor env stop dev
pvisor env start dev
pvisor env apply dev --path src   # 提交选中部分，其余继续 staged
pvisor env apply dev --all        # 提交剩余修改并重置为空 stage
pvisor env drop dev        # 丢弃修改并重置为空 stage
pvisor env delete dev --force
```

默认元数据位于 `~/.persisting/envs`，可用 `--root` 或 `PERSISTING_ENV_HOME`
覆盖。`start` / `stop` 控制是否接受新会话，并不表示常驻虚拟机；每次 `exec` / `shell`
都会挂载同一个 writable upper，所以修改会跨命令保留。`inspect` 使用内核强制的只读视图。
`apply --all` 或 `drop` 不会把 terminal Overlay 原地改回 `staged`；它们会创建单调递增的
Overlay generation。命令取得环境 lease 后会重新读取 generation，避免用 reset 前的
metadata 覆盖新 stage。

## `--safe` 参数预设

```bash
pvisor run --safe -- claude
pvisor run --safe --vm --rootfs image=my-agent-image:latest -- codex
pvisor run --safe -- zcode
pvisor run --safe --overlaynet-allow inference.example.com:443 -- zcode
```

`--safe` 生成一组命令行参数补丁，经过同一个 CLI 解析器后应用，再应用用户显式参数。
各 Agent 的补丁分别放在 `cli/run/safe/codex.rs`、`claude.rs`、`gemini.rs`、`zcode.rs`，
公共部分只负责选择与组合。`--safe` 同时要求所选执行器落实隔离，也不选择 executor。
优先级是 **显式 CLI > safe 预设 > 配置文件 > 普通默认值**。不带 `--safe` 的行为不变。
支持普通命令和 TOML `--spec`；已准备好的 JSON RunSpec 不接受该预设。

`--safe` 直接要求落实文件读取、写入和网络隔离，不允许静默回退到普通 host 进程。
不引入额外的 sandbox 命令行参数或配置项。`--strict` 仍是对全部请求能力的校验，
含资源限制等，和该隔离要求不同。

- macOS host：强制 Seatbelt 读取/写入范围，只允许连接 pVisor 分配的 loopback TCP 代理端口；
  阻止其他直接 IP 出口和环境中的宿主 Unix socket，仅保留必要的 Run 内 IPC。
  Agent 使用临时 HOME，不能直接读取原来的主目录；凭据需显式传入或由 Gateway 持有。
  系统运行库和启动所需的路径元数据仍可读取。
- Linux host：必须启用 namespaces 和 Landlock。目前仅支持普通出口 deny-all，
  选择性代理或 Gateway 缺少 namespace 代理桥时直接拒绝启动；需要此组合时显式使用 VM。
- VM：要求现有 `auto` 网络边界；safe 不自动选择 VM。
- container：当前缺少完整强制边界，`--safe` 拒绝启动。

隔离安装失败会停止运行。`--safe` 不能与 `--overlaynet off` 同时使用。


| 实际执行的命令 | 默认允许的普通网络目标 |
| --- | --- |
| `codex` | `api.openai.com:443` |
| `claude` | `api.anthropic.com:443` |
| `gemini` | `generativelanguage.googleapis.com:443` |
| `zcode` | `api.z.ai:443`、`open.bigmodel.cn:443` |
| 其他命令或 shell 包装器 | 默认拒绝；需显式声明目标或配置 Gateway |

识别依据是命令的文件名，支持绝对路径，`--name` 只影响显示名称。
这些是标准 API 服务预设，不会读取 Agent 私有配置或自动发现 OAuth、自定义供应商地址。
未匹配目标（包括独立域名上的遥测、上传、更新和依赖下载）被策略拒绝。
`--overlaynet-allow` 替换预设的允许目标；`--overlaynet-deny` 在允许列表上增加拒绝规则。
已有配置中的拒绝规则和限速保留。

ZCode 预设面向 **API Key + OpenAI 兼容协议直连**，依据
[官方模型配置文档](https://zcode.z.ai/cn/docs/configuration)：Coding Plan 使用
`https://api.z.ai/api/coding/paas/v4` 或 `https://open.bigmodel.cn/api/coding/paas/v4`；
普通 API 使用对应域名的 `/api/paas/v4`。这里只放行域名和端口，不限制这些路径。
不默认放行 `zcode.z.ai`、登录域名、对象存储、插件市场或更新地址。

核对依据为 ZCode 官方源码提交 `872ad960de7ec172591f7e1952f7849229f94521`：
[模型转发代码](https://github.com/zai-org/ZCode/blob/872ad960de7ec172591f7e1952f7849229f94521/apps/zcode-cli/packages/adapters/src/model/official-coding-plan-gateway.ts)
会将两家官方 Anthropic messages 端点改发 `zcode.z.ai`，因此该路径和依赖业务域名的账号登录流程
不在本预设的可用范围内；不要把“API Key 登录”直接等同于 OpenAI 协议直连。
[代理解析代码](https://github.com/zai-org/ZCode/blob/872ad960de7ec172591f7e1952f7849229f94521/apps/zcode-cli/packages/adapters/src/network/http-config.ts)
要求显式代理配置或 `ZCODE_HTTP_PROXY`，模型请求不会默认采用普通 `HTTP_PROXY`。
仅注入通用代理变量不能让 ZCode 的模型请求使用代理；macOS required 会拒绝其直连，
需要配置其专用代理入口才能联网，或显式使用 VM 的透明出口。
同域名的其他 API（例如 `api.z.ai` 的业务接口）仍可访问，不能称为只允许推理。

历史上传风险参考 [3.12.3 的原始取证报告](https://blog.ferstar.org/posts/zcode-silent-workspace-snapshot-upload/)：
报告描述了业务域名获取凭证后向对象存储上传快照的链路，其后续更新称 3.14.0 已移除该链路。
这是版本相关的外部取证，不能外推所有版本；上述白名单无需枚举存储桶即可拒绝未授权上传目标，
但不会阻止本地读取、打包或通过已允许的模型请求传出内容。本预设未做真实账号联网验证。

当最终配置启用 Gateway capture 且有明确路由时，预设拒绝普通出口，保留配置好的 Gateway
通道。Gateway 自己仍按既有路由转发；这不提供推理 API 路径过滤。

预设通过 `--clear-pass-env` 清空配置文件中的 `run.pass_env`，并使已有 OverlayFS 使用 manual 提交；
`--clear-pass-env` 也可单独使用，之后的显式 `--pass-env NAME` 仍然生效。
对应的显式 CLI 参数可以重新授予或覆盖。`--safe` 现在自动启用 OverlayFS，用于执行文件规则，
已有容器挂载和 compose 层仍保留；项目 base、rootfs、executor 不变。
需要向 Agent 交付凭据时显式使用 `--pass-env`；使用已配置的 Gateway 可由可信侧持有上游 Key。

启动时会打印实际策略及覆盖提醒。必须注意：

- 不使用 `--safe` 时，host/container 选择性代理仍可绕过。
- 域名规则无法区分同域名下的推理、遥测与上传 API，也无法阻止内容被夹带在模型请求中。
- 通配符规则覆盖 OverlayFS 视图；`--safe` 同时限制视图之外的访问，但显式授权的额外路径仍需单独保护。
  文件名规则不能识别改名副本、源码中的密钥或 Git 历史中的内容，也没有批量读取或 tool-call 关联监控。
- 扩大共享范围或选择 `--rootfs host` 会增加可访问的数据；`--safe` 不能与关闭 OverlayNet 同时使用。

## 文件访问规则

```bash
pvisor run --overlayfs-deny '**/.ssh' --overlayfs-warn '**/.env*' -- my-agent
```

`--overlayfs-deny GLOB` 和 `--overlayfs-warn GLOB` 可重复使用，都会启用 OverlayFS。
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
[overlayfs.access_policy]
deny = ["**/.ssh", "secrets/private.pem"]
warn = ["**/.env", "**/*.key"]
```

显式 `--overlayfs-deny` 替换整个 deny 列表，`--overlayfs-warn` 替换整个 warn 列表；
不会在 safe 默认列表上追加。`--overlayfs-clear-rules` 先清空两类规则，再应用显式列表。
清空规则不会关闭 OverlayFS。优先级仍为 CLI > safe > 配置文件 > 默认值。
规则随运行记录、checkpoint/fork 和 inspect 挂载保留，并写入 Run Bundle。

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

`pvisor run` 只有一份规范 `RunConfig`。TOML 和命令行选项是同一组字段的两种
表示。`--spec` 是可选且显式的；JSON 对象按准备好的 RunSpec 处理，否则按
TOML RunConfig 处理。pVisor 不会发现隐藏的项目配置文件。

```bash
pvisor run \
  --name codex \
  --overlayfs-path /workspace \
  --overlayfs-compose /path/to/project \
  --overlayfs-backend directory \
  --overlayfs-commit manual \
  --overlaynet-allow api.openai.com:443 \
  --overlaynet-deny 169.254.0.0/16 \
  --overlaynet-limit 10mbps \
  --gateway-mode capture \
  --gateway-level dialogue \
  --gateway-route \
    'name="openai", provider="openai", upstream="https://api.openai.com/v1", api_key_env="OPENAI_API_KEY"' \
  --record-destination ./capture \
  -- codex
```

`--record-destination` 写入本地 EventRecord JSONL。本仓库不再附带单独的历史服务。

所有新持久化的记录都同时包含 `timestamp`（RFC3339 UTC）和
`timestamp_unix_ms`（Unix 毫秒）。它们描述同一观测时间，必须在一毫秒内
一致。记录顺序仍由 `source + seq` 定义；时间戳是关联元数据，不是顺序的
事实源。

等价 TOML 是：

```toml
[run]
agent = "codex"
executor = "container"
command = ["codex"]

[container]
runtime = "docker"
image = "example/codex-agent:latest"
network = "host"

[overlayfs]
path = "/workspace"
compose = ["/path/to/project"]
backend = "directory"
commit = "manual"

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

用 `pvisor run --spec run.toml` 运行。显式 CLI 标量替换 TOML 标量。提供
任一重复 CLI 字段（`--overlayfs-compose`、`--overlaynet-allow`、
`--overlaynet-deny`、`--overlaynet-limit` 或 `--gateway-route`）会替换该
完整 TOML 列表。`--` 之后的命令替换 `run.command`。

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
Podman 或 Buildah。未提供显式 rootfs 时，默认是 `ubuntu:latest`。
manifest 和 layer digest 会被校验，host 架构选择 `linux/arm64` 或
`linux/amd64`，解包后的 rootfs 成为 pVisor OverlayFS 的不可变 lower。
`--image-store` 覆盖平台缓存目录。OCI 缓存目标被标为不可变，且该保护在逻辑
checkpoint/fork 后仍然有效，因此 `pvisor apply` 不能改写被其他 Run 共享的
rootfs。

在 Linux 上，`--rootfs host` 选择 host `/` 作为 VM rootfs lower，并在省略
`--executor` 时选择 VM executor。`--rootfs <PATH>` 使用准备好的目录，
`--rootfs image=<PATH>` 使用 OCI 镜像或镜像路径；三者互斥，host rootfs 在
macOS 上被拒绝。这是统一 rootfs 语法；
`--overlayfs-path` 指定 Agent 看到的绝对路径；重复 `--overlayfs-compose` 可按命令行顺序从底层叠加到顶层，当前 workspace 是隐式底层。带 guest 工作区路径时，
省略 `--overlayfs-path` 时，视图内容默认来自当前目录；为避免 lower 与挂载点递归覆盖，pVisor 会把 merged mount 放在每个 Run 的受管路径中。
工作区外的写入使用临时 root upper，并在 VM 退出时丢弃；工作区改动使用
durable OverlayFS stage。

合并后的 rootfs 是 guest `/`，`/workspace` 成为 guest cwd。在 Linux 和
macOS 上，vendored libkrun 通过 virtio-fs 直接服务 pVisor 的 rootfs 与
工作区 copy-on-write union。VMM 从不重新导出 host FUSE mount，也不物化或
对账这两棵树。Linux 使用 KVM，Apple Silicon macOS 通过同一 executor 使用
HVF。libkrunfw 随 wheel 安装在 pVisor 旁边。源码构建否则把 pinned 官方
release 下载到经 SHA-256 校验的平台缓存；在 macOS 上 `/usr/bin/cc` 把它的
预构建 kernel bundle 转成所需 dylib。仍可用 `--vm-library-dir` 选择系统
目录。OverlayNet `auto` 使用不可绕过的 VM smoltcp IPv4 TCP/DNS driver，而
Gateway capture 使用经 guest virtual router 的内部路由。Linux 另外用
namespace 和 Landlock 约束 VMM。macOS VMM 仍拥有调用用户的 host 权限，因此
尽管有 guest-kernel 隔离，第一版 OCI-image 也不应被当成敌对多租户边界。

在 host/container 执行上，四个可见 OverlayNet 策略标志和 Gateway capture
会自动启用代理 driver。任一 `--overlayfs-path`、`--overlayfs-compose`、
`--stage`、`--overlayfs-backend` 或 `--overlayfs-commit` 选项会
自动启用 OverlayFS；没有单独的 mode 开关。workspace 是隐式 base，compose 层按给定顺序叠加；显式 `--stage`
是元数据、轨迹和文件系统状态的统一记录目录。当 stage 嵌在 base 或 compose 层内时，pVisor 从合并视图
隐藏该子树，并拒绝 guest 重建它。libkrun Run 不创建 live host mountpoint，
防止 host indexer 递归进入 `<stage>/merged`。反向拓扑——stage 包含 lower
层——会被拒绝。在 pVisor 能安全物化完整 merged-vs-base diff 之前，组合 Run
拒绝 `commit=apply` 和随后的 `pvisor apply` 命令。
在 host/container 执行上，OverlayNet 策略作用于经显式代理路由的流量，并不
声称不可绕过的 host 网络隔离。在 libkrun VM 上，`auto` 挂上不可绕过的
smoltcp IPv4 TCP/DNS；`off` 让 guest 离线。`--overlaynet-deny-all` 把同一
default-deny 策略交给当前 driver。host/container 直接 socket 仍是 ambient，
而 VM Gateway 路由仍可通过 guest 的 virtual router 用于已配置的模型流量。

## Run 项目发现

当前目录是默认项目关联。启用 OverlayFS 时，`--overlayfs-compose` 指定宿主机叠加层，
`--overlayfs-path` 指定 Agent 看到的视图路径。每个 Run 在 pVisor 默认记录根目录下获得独立目录。若该根会落在
所选 OverlayFS base 或 compose 层内，pVisor 改用系统临时 Run 根，以保持
可写 stage 分离：

```text
project/                         # reusable workspace / default base

~/.persisting/runs/
└── run-<uuid>/                  # one generated Run and default stage
    ├── run.json
    ├── run-bundle.json          # mode 0600; outcome + safety + changes + effects
    ├── overlay.json             # when OverlayFS is enabled
    ├── upper/                   # or a Run-named Jujutsu workspace upper
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

## 相关工作流

- [第一次运行](../start/first-run.md)：最短完整闭环。
- [执行环境](../guides/execution.md)：选择 provider。
- [审查并应用 Effect](../guides/review-apply.md)：过滤且可重复的 apply。
- [网络控制](../guides/network.md) 与 [捕获轨迹](../guides/capture.md)：其他
  Effect 维度。
