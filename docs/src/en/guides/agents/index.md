---
description: Run Claude Code, Codex CLI, or any script as a pVisor Job with workspace staging, network policy, and a recorded Run Bundle.
---

# Connect your agent

After the [offline staging demo](../../start/first-run.md#agent-style), connect an agent installed on your machine or in your image. The [Claude Code repair task](claude-code.md#repair-task) covers project setup through accepting the results.

<div class="pv-guide-steps" markdown="1">

1. ### Check the agent and authentication {#prepare-agent}

    Check the command in the terminal where you will launch the Job, for example Claude Code:

    ```bash title="Shell"
    command -v claude
    ```

    Expect an executable path. Host execution needs the command installed locally. VM or OCI execution needs it installed in the image; the host installation does not automatically carry over.

    Choose model authentication with the [Claude Code](claude-code.md) or [Codex CLI](codex.md) guide. For API keys, set the variable in the current terminal using your existing credential manager and pass it explicitly. Keep real keys out of project files and example commands. HOME isolation may hide host login state; check that the agent can authenticate inside stagebox mode.

1. ### Launch from the project directory {#launch-agent}

    Run at the root of the project to modify, for example with `ANTHROPIC_API_KEY` already set:

    ```bash title="Shell"
    pvisor spawn --safe --pass-env ANTHROPIC_API_KEY -- claude
    ```

    Specify the goal, allowed file scope, and required tests. Wait for the agent to finish and exit back to the launching terminal. For model connectivity or dependency-download failures, check destinations and credentials against the network table below. Retain errors and Job records before changing configuration and retrying.

1. ### Review and accept results {#accept-results}

    ```bash title="Shell"
    pvisor status last
    pvisor review last --diff
    ```

    Inspect task status, candidate files, and test output before accepting actual paths. Run key tests again in the original project. See [Agent style stagebox handoff](../index.md#handoff) for commands and remaining-change cleanup. Failed or cancelled tasks may still have candidate files; inspect status before deciding rather than deleting the stage directly.

</div>

## Approval settings and verification scope {#approval-scope}

Retain the agent's own approval settings and decide from actual tool prompts. pVisor does not automatically disable agent or IDE approvals; authentication, deployment, pushes, and paid operations retain their own controls. See [Claude Code verification scope](claude-code.md#verification) for recorded client-version evidence, cloud-tutorial acceptance gaps, and prompts to record.

## Files, credentials, and networking {#controls}

`--safe` keeps workspace changes staged for review before apply and denies access to `.ssh`, `.gnupg` and private-key files within the view. HOME uses a separate private stage; writes are discarded after execution and do not enter the workspace Run Bundle, so do not depend on them to persist login state. See [credentials and environment](../policies/credentials.md) for platform-specific HOME and credential handling.

Network presets match the executable filename and allow its model API destinations on HTTPS port 443; other destinations are denied. Deliver credentials explicitly with `--pass-env NAME` or let Gateway hold them.

| Agent | Executable | `--safe` default allowlist | Guide |
| --- | --- | --- | --- |
| Claude Code | `claude` | `api.anthropic.com` | [Claude Code](claude-code.md) |
| Codex CLI | `codex` | `api.openai.com`, `chatgpt.com`, `ab.chatgpt.com` | [Codex CLI](codex.md) |
| Gemini CLI | `gemini` | `generativelanguage.googleapis.com` | See [`--safe` presets](../../reference/cli.md#safe-参数预设) |
| ZCode | `zcode` | `api.z.ai`, `open.bigmodel.cn` | See [`--safe` presets](../../reference/cli.md#safe-参数预设) |
| aider | `aider` | No preset; needs `--overlaynet-allow` | See "Agents without presets" below |
| OpenCode | `opencode` | No preset; needs `--overlaynet-allow` | See "Agents without presets" below |
| Any script | Any | None; `bash`, `sh`, `zsh`, `fish` reuse the Codex destinations | [Any script](custom-scripts.md) |

Unknown commands deny outbound access. `--overlaynet-allow HOST:PORT` grants it explicitly and replaces preset lists. Matching uses the **direct executable filename**; shell wrappers do not trigger agent-specific adaptation.

## Agents without presets {#custom-agent}

Take aider as the example: install it in the selected executor, pick the provider in the agent's own configuration, then explicitly allow its API destination and pass the credential variable (variable names come from the agent's own documentation, such as `OPENAI_API_KEY` or `GEMINI_API_KEY`):

```bash title="Shell"
pvisor spawn --safe --overlaynet-allow api.openai.com:443 \
  --pass-env OPENAI_API_KEY -- aider
pvisor status --review last
pvisor apply last --path src
```

Logins, other providers and dependency downloads each need their own allowed destination.

After a run, check the installed controls and observations in the Run Bundle. Supported agent version ranges are not systematically tested; see [end-to-end tasks](../../benchmarks/agent-tasks.md).

!!! note "Experimental integrations"
    The aider, Gemini CLI, and OpenCode integrations use only existing pVisor options and stay experimental until a pinned-version regression exists. The Linux host selective proxy is cooperative; use a prepared VM when you need enforcement.
