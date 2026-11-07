---
description: 通过 Homebrew 或 pip 安装 pVisor，接入 Pi、DSH、Codex 插件，沿用原生命令交接成果。
---

# Pi、DSH 和 Codex 插件

在 Agent 中调用 pVisor 暂存任务，完成后使用原生 `spawn → status/review → apply/drop` 流程交接成果。通过 Homebrew 或 pip 安装 runtime，npm 仅分发 Agent 适配器。

!!! note "首次发布待完成"
    npm 包及远程 marketplace 提交尚未上架。以下是发布后的用户安装入口，不代表当前已经可以从 registry 安装。维护者须完成发布授权和发行验收后，再移除此说明。

## 安装 {#install}

准备 Node.js 22.19+、目标 Agent，并通过 [Homebrew 或 pip](../../start/installation.md) 单独安装 pVisor。Host 暂存支持 Linux x64、macOS arm64，需要满足 [Host 暂存前提](../../start/installation.md#local-prerequisites)。插件不会安装 runtime、自行提权安装 FUSE、启用扩展或改变 namespace 策略。

启动 Agent 宿主时，确保其 PATH 中可以找到 `pvisor`。桌面应用可能不继承终端 PATH，必要时在宿主环境中设置 `PVISOR_PLUGIN_BINARY` 为已安装 CLI 的绝对路径。插件忽略空或相对 PATH 项，并在工作前检查必需的 CLI 参数。不兼容时拒绝执行并提示升级；缺少选择性 apply 的旧版（例如 0.3.1）不满足要求。插件与 runtime 独立版本管理。

选择你实际使用的宿主：

```sh
# Pi
pi install npm:pvisor-agent-plugins

# DSH: install only the profile you use
dsh plugin --profile web add pvisor-agent-plugins
dsh plugin --profile desktop add pvisor-agent-plugins

# Codex CLI: run from your project
codex plugin marketplace add DeepLink-org/pvisor --ref plugins-v0.1.0
codex plugin add pvisor@pvisor
codex mcp add pvisor -- npx --yes --package pvisor-agent-plugins@0.1.0 pvisor-agent-mcp --project "$PWD"
```

DSH Desktop 也可以在 **Plugins → Add plugin** 中输入 `pvisor-agent-plugins`。只安装你实际使用的 profile。

Codex marketplace 安装工作流 skill，原生 `codex mcp add` 用当前项目绑定工具；`npx` 仅安装固定版本的插件，无需全局安装，runtime 必须预先安装。没有插件管理能力时可仅用 MCP。捆绑 MCP 的 cwd 可能是插件缓存目录，不能用它推断用户项目。

安装后从目标项目启动新会话。调用 `pvisor_doctor`，检查二进制路径、项目路径和 runtime 版本；它检查必需的 CLI 能力，但不证明 FUSE 挂载或安全隔离可用。Pi 使用调用上下文的 cwd，DSH 使用发起 Agent 的 cwd。MCP 缺少显式项目时拒绝启动，不回退到插件目录。

## 执行任务 {#spawn}

要求 Agent 调用 `pvisor_spawn`，例如传入：

```json
{
  "argv": [
    "sh",
    "-c",
    "printf 'candidate\\n' > accepted.txt; printf 'temporary\\n' > discarded.txt"
  ],
  "timeout_seconds": 120
}
```

该工具调用原生 `pvisor spawn`，固定使用 Host、`--safe`、拒绝全部出站网络、捕获 stdio 和执行期限；不加载个人 Agent 预设。期限为 1–600 秒，默认 120 秒。每次调用创建新的 Session，相关编辑与测试必须放在同一次调用中，后续任务看不到未接收的成果。

保存返回的绝对 stage selector。插件工具 `pvisor_status`、`pvisor_review`、`pvisor_apply_preview` 与原生 CLI 使用同一个 selector，无需转换插件 ID。不要使用 `last`。失败、取消或未知完成状态仍可能留下候选文件，不自动重试或清理。

## 审查并接收 {#handoff}

使用已经通过 Homebrew 或 pip 安装的原生 CLI：

```sh
pvisor --version
```

将返回的 selector 保存为终端变量 `stage` 后：

```sh
pvisor status "$stage" --json
pvisor review "$stage" --diff
pvisor apply "$stage" --path accepted.txt --dry-run --diff
pvisor apply "$stage" --path accepted.txt
pvisor drop "$stage"
```

检查已接收文件并重新运行相关测试。仅在剩余候选不再需要时 drop；全部接收后跳过。原生所有权、完成凭据和冲突检查保持有效，插件不会绕过运行 owner 的 stage 锁。Drop 不撤销已 apply 文件或外部副作用，详见[成果审查](../review-apply.md)。

Pi 也提供人工命令，每次修改前要求交互确认：

```text
/pvisor status STAGE_FROM_RESULT
/pvisor review STAGE_FROM_RESULT
/pvisor apply {"selector":"STAGE_FROM_RESULT","paths":["accepted.txt"]}
/pvisor drop STAGE_FROM_RESULT
```

DSH 的 `pvisor_apply` 和 `pvisor_drop` 复用 Harness 审批，仅 `Allow once` 可以执行；拒绝、取消或审批不可用时不写回。Apply 在审批前展示原生预览。Codex MCP 不提供发布或丢弃工具，由用户在终端执行上面的原生命令。保留宿主自己的审批设置，skill 不能限制已有宿主 Shell 权限的 Agent。

插件预览前先调用原生 status 核对并恢复 Session 控制状态，再调用 `apply --dry-run`；不会直接创建内部锁文件，也不会接受项目文件。

## 配置与边界 {#scope}

| 环境变量 | 默认值 | 含义 |
| --- | --- | --- |
| `PVISOR_PLUGIN_BINARY` | 宿主 PATH 中的 `pvisor` | 已安装 CLI 的绝对路径；覆盖项无效时不回退 |
| `PVISOR_PLUGIN_STATE_DIR` | `~/.local/state/pvisor/agent-plugins/v1` | 项目外的私有任务记录和 stage |
| `PVISOR_PLUGIN_PROJECT` | 未设置 | 手动启动 MCP 时的显式项目；建议用原生 `codex mcp add ... --project DIR`；Pi、DSH 使用调用上下文 |

状态目录要求当前用户所有、权限 0700。任务不能读取其他项目的插件记录。任务记录不保存 argv 或凭据，runtime 输出和记录仍按原有规则保留，避免输出密钥。

每个任务使用新的私有 HOME 和 XDG 状态路径，不继承宿主 HOME 的登录状态、用户配置或缓存。持久化 stage 仍在配置的状态目录中；失败或中断后可能保留临时 HOME，它不属于待接收的项目成果。依赖用户 HOME 配置的工具需要单独准备，不应通过关闭隔离绕过。

这些是任务级适配器，不覆盖宿主的内置文件、Shell、补丁、后台任务或其他 MCP 工具。尚未提供网络任务、VM 选择、PTY、stdin、运行中 Session 延续或工具级保存点。需要整个 Codex 进入 stagebox 时使用已有 [Codex 托管启动](codex.md#run-codex)，不要嵌套任务插件。

## 验证与维护 {#verify}

在临时项目中调用 `pvisor_doctor`，再运行前面的双文件任务。确认宿主未提前出现文件，review 后只 apply `accepted.txt`，drop 另一项，已接收文件仍应保留。Pi、DSH 同时检查批准与拒绝两条路径。启动失败时保留安全设置，按报错修复系统前提。

宿主 fixture 不等于真实 Pi UI、DSH Web/Desktop、Electron 或 Codex 插件加载验收。实际接入时记录 Node、pVisor、Agent 版本、工具目录、项目路径和审批行为；不据此宣称云模型端到端或免审批验证通过。

升级前审查待处理任务。通过宿主包管理器更新插件，通过 Homebrew 或 pip 单独更新 runtime，再重启并运行 Doctor 检查兼容性。不要在任务运行时替换 runtime。Codex 固定的 marketplace ref 与 npx 版本需一起更新。通过宿主管理器卸载 `npm:pvisor-agent-plugins`、`pvisor-agent-plugins` 或 `pvisor@pvisor`；独立 MCP 配置用 `codex mcp remove pvisor` 删除。卸载插件保留 runtime 和候选文件，不递归清空状态目录。
