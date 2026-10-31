---
description: 在 Agent style stagebox 模式下接入现有 Agent，配置凭据和网络，审查并选择性接收结果。
---

# 接入你的 Agent

完成[离线暂存演示](../../start/first-run.md#agent-style)后，接入本机或镜像中已安装的 Agent。[Claude Code 修复任务](claude-code.md#repair-task)包含从准备项目到接收成果的完整步骤。

<div class="pv-guide-steps" markdown="1">

1. ### 确认 Agent 和认证方式 {#prepare-agent}

    在准备启动 Job 的终端检查命令，例如 Claude Code：

    ```bash title="Shell"
    command -v claude
    ```

    应输出可执行文件路径。使用 Host 时命令须已安装在本机；使用 VM 或 OCI 时则必须安装在镜像内，宿主安装不会自动带入镜像。

    按 [Claude Code](claude-code.md) 或 [Codex CLI](codex.md) 指南选择模型认证。使用 API Key 时，在当前终端通过你已有的凭据管理方式设置变量，再显式传入；不要把真实 Key 写进项目文件或示例命令。HOME 隔离可能使宿主登录状态不可见，需要检查 Agent 在 stagebox 模式内能否完成认证。

1. ### 从项目目录启动 {#launch-agent}

    在要修改的项目根目录执行。例如使用已设置的 `ANTHROPIC_API_KEY`：

    ```bash title="Shell"
    pvisor spawn --safe --pass-env ANTHROPIC_API_KEY -- claude
    ```

    给出明确目标、允许修改的文件范围和测试要求。等待 Agent 完成任务并退出，回到启动时的终端。若模型连接或依赖下载失败，按下方网络表检查目标与凭据；保留错误和 Job 记录，再调整配置重试。

1. ### 审查并接收结果 {#accept-results}

    ```bash title="Shell"
    pvisor status last
    pvisor review last --diff
    ```

    检查任务状态、候选文件和测试输出后，按实际路径选择合入，再在原项目运行关键测试。具体命令与剩余改动清理见[Agent style stagebox 成果交接](../index.md#handoff)。失败或取消的任务可能仍有候选文件，先检查状态再决定，不要直接删除 stage。

</div>

## 审批设置与验证范围 {#approval-scope}

保留 Agent 自身的审批设置，按实际提示决定是否允许工具操作。pVisor 不会自动关闭 Agent 或 IDE 的审批；认证、部署、推送和付费操作仍需各自控制。已有客户端版本证据、云模型教程的验收缺口和需要记录的提示，见 [Claude Code 验证范围](claude-code.md#verification)。

## 文件、凭据与网络 {#controls}

`--safe` 将工作区改动留在暂存区，审查后再 apply；预设拒绝访问视图内的 `.ssh`、`.gnupg` 与私钥文件。HOME 使用独立的私有 stage，运行结束后丢弃写入，不进入工作区 Run Bundle，不能依赖它保存登录状态。各平台的 HOME 与凭据处理见[凭据与环境变量](../policies/credentials.md)。

网络预设按可执行文件名匹配 Agent，自动放行它的模型 API 目标（HTTPS 443 端口），其他目标默认拒绝。凭据需要显式 `--pass-env NAME` 或由 Gateway 持有。

| Agent | 命令名 | `--safe` 默认放行 | 指南 |
| --- | --- | --- | --- |
| Claude Code | `claude` | `api.anthropic.com` | [Claude Code](claude-code.md) |
| Codex CLI | `codex` | `api.openai.com`、`chatgpt.com`、`ab.chatgpt.com` | [Codex CLI](codex.md) |
| Gemini CLI | `gemini` | `generativelanguage.googleapis.com` | 见 [`--safe` 参数预设](../../reference/cli.md#safe-参数预设) |
| ZCode | `zcode` | `api.z.ai`、`open.bigmodel.cn` | 见 [`--safe` 参数预设](../../reference/cli.md#safe-参数预设) |
| aider | `aider` | 无预设，需 `--overlaynet-allow` | 见下方「无预设 Agent」 |
| OpenCode | `opencode` | 无预设，需 `--overlaynet-allow` | 见下方「无预设 Agent」 |
| 任意脚本 | 任意 | 无；`bash`、`sh`、`zsh`、`fish` 沿用 Codex 的目标 | [任意脚本](custom-scripts.md) |

未知命令默认拒绝出站，用 `--overlaynet-allow HOST:PORT` 显式授权（会覆盖预设列表）。预设按**直接可执行文件名**匹配，shell 包装器不会触发 Agent 专属的适配。

## 无预设 Agent 的接入 {#custom-agent}

以 aider 为例：先在所选 executor 内安装 aider，并在 Agent 自身配置里选好 provider，然后显式放行它的 API 目标并传入凭据变量（变量名由 Agent 自身文档定义，如 `OPENAI_API_KEY`、`GEMINI_API_KEY`）：

```bash title="Shell"
pvisor spawn --safe --overlaynet-allow api.openai.com:443 \
  --pass-env OPENAI_API_KEY -- aider
pvisor status --review last
pvisor apply last --path src
```

登录、其他 provider 或依赖下载需要逐个补充放行目标。

运行后检查 Run Bundle 中实际安装的控制与观察结果。各 Agent 受支持的版本范围尚未系统测试，见[端到端任务](../../benchmarks/agent-tasks.md)。

!!! note "实验性接入"
    aider、Gemini CLI、OpenCode 的接入只使用已有 pVisor 选项，在出现固定版本的回归之前都属于实验性。Linux host 的选择性代理是协作式；需要强制边界时使用其中已安装 Agent 的 VM。
