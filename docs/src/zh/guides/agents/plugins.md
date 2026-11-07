---
description: 通过 npm 和远程 marketplace 安装 Pi、DSH、Codex 插件，自动部署匹配的 pVisor runtime，沿用原生命令交接成果。
---

# Pi、DSH 和 Codex 插件

在 Agent 中调用 pVisor 暂存任务，完成后使用原生 `spawn → status/review → apply/drop` 流程交接成果。npm 按平台安装配套 runtime，无需检出源码、安装 Rust 或手工填写二进制路径。

!!! note "首次发布待完成"
    npm 包及远程 marketplace 提交尚未上架。以下是发布后的用户安装入口，不代表当前已经可以从 registry 安装。维护者须完成发布授权和发行验收后，再移除此说明。

## 安装 {#install}

准备 Node.js 22.19+ 和目标 Agent。支持 Linux x64、macOS arm64；安装插件时自动安装匹配的原生平台包及完整 CLI 组件。系统仍需满足 [Host 暂存前提](../../start/installation.md#local-prerequisites)，插件不会自行提权安装 FUSE、启用扩展或改变 namespace 策略。

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

Codex marketplace 安装工作流 skill，原生 `codex mcp add` 用当前项目绑定工具；`npx` 自动安装固定版本的插件与 runtime，不需要全局安装。没有插件管理能力时可仅用 MCP。捆绑 MCP 的 cwd 可能是插件缓存目录，不能用它推断用户项目。

安装后从目标项目启动新会话。调用 `pvisor_doctor`，检查项目路径和 runtime 版本；它不证明 FUSE 挂载或安全隔离可用。Pi 使用调用上下文的 cwd，DSH 使用发起 Agent 的 cwd。MCP 缺少显式项目时拒绝启动，不回退到插件目录。

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

需要在终端使用原生 CLI 时，可以额外安装同一个 npm 包：

```sh
npm install --global pvisor-agent-plugins
pvisor --version
```

这会提供 `pvisor` 原生命令入口，不引入 `commit/abort` 别名。将返回的 selector 保存为终端变量 `stage` 后：

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
| `PVISOR_PLUGIN_BINARY` | 配套的原生平台包 | 可选的维护者覆盖项，正常安装无需设置 |
| `PVISOR_PLUGIN_STATE_DIR` | `~/.local/state/pvisor/agent-plugins/v1` | 项目外的私有任务记录和 stage |
| `PVISOR_PLUGIN_PROJECT` | 未设置 | 手动启动 MCP 时的显式项目；建议用原生 `codex mcp add ... --project DIR`；Pi、DSH 使用调用上下文 |

状态目录要求当前用户所有、权限 0700。任务不能读取其他项目的插件记录。任务记录不保存 argv 或凭据，runtime 输出和记录仍按原有规则保留，避免输出密钥。

这些是任务级适配器，不覆盖宿主的内置文件、Shell、补丁、后台任务或其他 MCP 工具。尚未提供网络任务、VM 选择、PTY、stdin、运行中 Session 延续或工具级保存点。需要整个 Codex 进入 stagebox 时使用已有 [Codex 托管启动](codex.md#run-codex)，不要嵌套任务插件。

## 验证与维护 {#verify}

在临时项目中调用 `pvisor_doctor`，再运行前面的双文件任务。确认宿主未提前出现文件，review 后只 apply `accepted.txt`，drop 另一项，已接收文件仍应保留。Pi、DSH 同时检查批准与拒绝两条路径。启动失败时保留安全设置，按报错修复系统前提。

宿主 fixture 不等于真实 Pi UI、DSH Web/Desktop、Electron 或 Codex 插件加载验收。实际接入时记录 Node、pVisor、Agent 版本、工具目录、项目路径和审批行为；不据此宣称云模型端到端或免审批验证通过。

升级前审查待处理任务，通过宿主包管理器一起更新插件与原生依赖，再重启。Codex 固定的 marketplace ref 与 npx 版本需一起更新。通过宿主管理器卸载 `npm:pvisor-agent-plugins`、`pvisor-agent-plugins` 或 `pvisor@pvisor`；独立 MCP 配置用 `codex mcp remove pvisor` 删除。全局 CLI 用 `npm uninstall --global pvisor-agent-plugins` 卸载。卸载不删除候选文件，不递归清空状态目录。
