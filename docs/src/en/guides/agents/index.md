# Connect your agent

Run existing Agent CLIs, scripts, and automation through one entry point, without changing the agent:

```bash
pvisor run --safe -- <agent-command>
pvisor status --review last
pvisor apply last --path src
```

`--safe` mode matches the executable filename and allows model API destinations on HTTPS port 443; other destinations are denied.

| Agent | Executable | Safe destinations | Guide |
| --- | --- | --- | --- |
| Claude Code | claude | api.anthropic.com | [Claude Code](claude-code.md) |
| Codex CLI | codex | api.openai.com, chatgpt.com, ab.chatgpt.com | [Codex](codex.md) |
| Gemini CLI | gemini | generativelanguage.googleapis.com | [Gemini (planned)](gemini-cli.md) |
| ZCode | zcode | api.z.ai, open.bigmodel.cn | [`--safe` presets](../../reference/cli.md#safe-参数预设) |
| aider | — | No preset; grant explicitly | [aider (planned)](aider.md) |
| OpenCode | — | No preset; grant explicitly | [OpenCode (planned)](opencode.md) |
| Scripts | Any | None; bash/sh/zsh/fish use Codex destinations | [Scripts](custom-scripts.md) |

Unknown commands deny outbound access. `--overlaynet-allow HOST:PORT` grants it explicitly and replaces preset lists. Matching uses the **direct executable filename**; shell wrappers do not trigger agent-specific adaptation.

Compatibility comes from observed controls in the Bundle, not agent names. Supported version ranges are not systematically tested; see [end-to-end tasks (planned)](../../benchmarks/agent-tasks.md).
