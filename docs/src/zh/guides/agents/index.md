# 接入你的 Agent

pVisor 用一个统一入口运行现有的 Agent CLI、脚本和自动化命令，不需要修改 Agent。先在所选 executor 内安装命令并配置 Agent 自身的 provider；使用镜像时，宿主安装的 Agent 不会自动出现在镜像中。下面的 `<agent-command>` 要替换为实际命令：

```bash
pvisor run --safe -- <agent-command>
pvisor status --review last
pvisor apply last --path src
```

`--safe` 按可执行文件名匹配 Agent，自动放行它的模型 API 目标（HTTPS 443 端口），其他目标默认拒绝。凭据需要显式 `--pass-env NAME` 或由 Gateway 持有；HOME 写入在运行结束后丢弃，不能依赖它保存登录状态。具体交付方式见[凭据与环境变量](../policies/credentials.md)。

| Agent | 命令名 | `--safe` 默认放行 | 指南 |
| --- | --- | --- | --- |
| Claude Code | `claude` | `api.anthropic.com` | [Claude Code](claude-code.md) |
| Codex CLI | `codex` | `api.openai.com`、`chatgpt.com`、`ab.chatgpt.com` | [Codex CLI](codex.md) |
| Gemini CLI | `gemini` | `generativelanguage.googleapis.com` | [Gemini CLI](gemini-cli.md) |
| ZCode | `zcode` | `api.z.ai`、`open.bigmodel.cn` | 见 [`--safe` 参数预设](../../reference/cli.md#safe-参数预设) |
| aider | — | 无预设，需 `--overlaynet-allow` | [aider](aider.md) |
| OpenCode | — | 无预设，需 `--overlaynet-allow` | [OpenCode](opencode.md) |
| 任意脚本 | 任意 | 无；`bash`、`sh`、`zsh`、`fish` 沿用 Codex 的目标 | [任意脚本](custom-scripts.md) |

未知命令默认拒绝出站，用 `--overlaynet-allow HOST:PORT` 显式授权（会覆盖预设列表）。预设按**直接可执行文件名**匹配，shell 包装器不会触发 Agent 专属的适配。

运行后检查 Run Bundle 中实际安装的控制与观察结果。各 Agent 受支持的版本范围尚未系统测试，见[端到端任务](../../benchmarks/agent-tasks.md)。

!!! note "实验性接入"
    aider、Gemini CLI、OpenCode 的接入页只使用已有 pVisor 选项，在出现固定版本的回归之前都属于实验性。Linux host 的选择性代理是协作式；需要强制边界时使用其中已安装 Agent 的 VM。
