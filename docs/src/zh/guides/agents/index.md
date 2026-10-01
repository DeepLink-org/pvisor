# 接入你的 Agent

pVisor 用一个统一入口运行现有的 Agent CLI、脚本和自动化命令：

```bash
pvisor run --safe --stage ../stage-001 -- <agent-command>
```

| Agent | 指南 | 状态 |
| --- | --- | --- |
| Claude Code | [claude-code.md](claude-code.md) | 新建 |
| Codex CLI | [codex.md](codex.md) | 新建 |
| Gemini CLI | [gemini-cli.md](gemini-cli.md) | 规划中 |
| aider | [aider.md](aider.md) | 规划中 |
| OpenCode | [opencode.md](opencode.md) | 规划中 |
| 任意脚本 | [custom-scripts.md](custom-scripts.md) | 新建 |

兼容性由 Run Bundle 中的实际能力证据决定，而不是由 Agent 名称推定。

!!! note "TODO"
    补兼容矩阵：Agent 版本 × 注入方式（代理／base URL） × 已知限制。

