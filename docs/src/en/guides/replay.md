# SandboxReplay

SandboxReplay continues agent trajectories in a fresh sandbox you prepared. It reruns tools before a boundary, uses fresh observations to reconstruct native context, then continues live. It creates a derived Run in the [execution model](../design/execution-model.md).

## 1. Concepts

A trajectory is:

~~~text
Task → A1 → O1 → ... → AN → ON → A(N+1) → ...
~~~

- Ai: assistant action i, including visible text/reasoning and tools.
- Oi: observation returned after executing Ai.
- N: selected recovery boundary.
- A(N+1): original next model reply.
- A′(N+1): first live reply after replay in the new sandbox.

Parallel tools in one assistant response form one action batch. Boundaries cannot split it.

## 2. Semantics

1. Rerun A1…AN tools in order to produce fresh O′1…O′N.
2. Preserve native system prompt, tool definitions, task, and actions; replace old observations with fresh ones and request the next inference.

The default first live request ends exactly at O′N:

~~~text
Native system prompt + tool definitions + original task
    + A1 → O′1 → ... → AN → O′N
                              ^ request ends here
~~~

No extra continuation message is appended. Correct reconstruction/message boundaries do not guarantee byte-identical next actions: files, dynamic tool output, and model sampling can change them.

An explicit boundary_user_prompt changes the first inference after the boundary:

~~~text
Native system prompt + tool definitions + original task
    + A1 → O′1 → ... → AN → O′N
    + boundary_user_prompt → A′(N+1)
~~~

It is appended once after O′N without replacing the original task. Prepare-only/replay-only send no live request and do not inject it. Without it, the original boundary semantics remain.

## 3. Adapters

### 3.1 Claude Code

Parses native JSONL/UUID sessions along the active parent-UUID chain, replays complete batches, writes fresh observations into a rebuilt session, then uses native `--resume`. A local bridge validates/removes the temporary resume envelope before the first inference so it still ends at O′N. This is transport compatibility, not a new system prompt. Agent/TaskOutput remain native tools; choose an unambiguous reconstructable boundary.

### 3.2 OpenHands

Uses native event trajectories and ReplayManager. The runtime executes prefix Actions in the new sandbox and emits fresh Observations; continuation starts directly when the replay queue empties.

### 3.3 mini-swe-agent

Uses mini-swe-agent-1.1 messages. The runner retains original system/task messages, reruns actions, formats fresh observations natively into agent.messages, then calls agent.step().

### 3.4 Pi agent

Pins @earendil-works/pi-coding-agent 0.83.0 and native RPC event JSONL. A replay step is a complete turn_end tool batch. Pi's own read/bash/edit/write tools produce observations for a new v3 session; unknown tools reject rather than silently changing semantics. Continuation uses continue() without a prompt, or prompt() once after O′N when configured.

### 3.5 OpenCode

Pins 1.17.7 and native opencode run --format=json events. User, step_start, text, reasoning, tool_use, and step_finish group into complete batches. Command/read/write/edit tools rerun and state.output reconstructs the prefix; continuation uses --session. Intermediate tool_use updates merge to avoid duplicate replay.

This version has no CLI step budget and ignores agent.build.steps on resume. Two external watchdogs apply only to OpenCode: stderr progress counts trigger SIGINT at remaining step budget; ten minutes without output triggers idle termination. Budget results use agent_status=max_steps and opencode_step_budget metadata. See [historical diagnostics](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/replay/qwen3.6-results.md).

### 3.6 Codex

Pins CLI 0.149.0 and native rollout JSONL. A step comprises complete function_call/custom_tool_call batches and outputs. Prefix tools rerun, response_item context is written into isolated CODEX_HOME, and codex exec resume session-id --json continues. Native ID comes from session_meta. Unknown tools or missing ID fail explicitly without starting a fresh session.

Native system/tools/task remain; observations alone are replaced. A local Responses bridge removes transport nonces for older resume CLI compatibility, forwards model requests, and cleans the continued trajectory afterward. Default input condition is replayed_boundary_only. Explicit boundary prompts are appended once and retained, using boundary_user_prompt_appended. Bridge validation failure refuses continuation instead of bypassing it.

## 4. Usage

### 4.1 Install

Install inside the sandbox (Python 3.10+):

~~~bash
python -m pip install pvisor

command -v pvisor
pvisor --version
pvisor replay --help
~~~

To test unreleased code:

~~~bash
git clone https://github.com/DeepLink-org/pvisor.git
cd pvisor
just install-cli

export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
pvisor replay --help
~~~

Or use a local build:

~~~bash
just build release
./target/release/pvisor replay --help
~~~

pVisor must be installed or mounted read-only into the new sandbox's PATH. Copied binaries must match OS, architecture, and dynamic runtime. Agent versions must match the pinned profile.

### 4.2 CLI and TOML

Run inside a fresh prepared sandbox:

~~~bash
pvisor replay \
  --agent claude-code \
  --trajectory /input/session.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/claude \
  --boundary-user-prompt '请检查新的 observation 后继续任务'
~~~

Pi runtime at /opt/pi-agent:

~~~bash
pvisor replay \
  --agent pi-agent \
  --trajectory /input/pi-agent.events.jsonl \
  --after-step 30 \
  --agent-entrypoint /opt/pi-agent/bin/pi
~~~

Equivalent Pi TOML:

~~~toml
[replay]
agent = "pi-agent"
trajectory = "/input/pi-agent.events.jsonl"
after_step = 30
agent_entrypoint = "/opt/pi-agent/bin/pi"
max_steps = 200
disable_thinking = true
~~~

OpenCode native events:

~~~bash
pvisor replay \
  --agent opencode \
  --trajectory /input/opencode.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/opencode
~~~

Codex native rollout; session ID is extracted automatically:

~~~bash
pvisor replay \
  --agent codex \
  --trajectory /input/rollout.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/codex
~~~

The same TOML structure works with corresponding agent, trajectory, and entrypoint values.

Claude Code TOML:

~~~toml
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 30
agent_entrypoint = "/usr/bin/claude"
max_steps = 200
session_id = "task-291-attempt-1"
replay_only = false
disable_thinking = true
boundary_user_prompt = "请检查新的 observation 后继续任务"
~~~

### 4.3 Modes and results

- Default: execute the prefix, then continue live.
- `--replay-only`: execute the prefix, stop before inference.
- `--prepare-only`: validate/build context, execute no tools/agent, require no runtime.

--max-steps includes the prefix. --after-step 30/--max-steps 50 allows at most 20 continuation actions. Replay-only must cover the prefix; continuation must leave at least one live action.

sandbox-playback.result/v3 uses phase prepared/replayed/continued, quality verified/degraded, and agent_status not_started/completed/max_steps/failed. Failures retain generated logs/trajectories. An OpenHands fatal controller status means failure even if its process exits 0.

Metadata records whether a boundary prompt was requested/injected, its character length and SHA-256. Journal omits prompt plaintext; native prepared/continued trajectories may include it. Claude's in-memory bridge appends it to the first cleaned upstream request without changing the rebuilt session. Next-action comparison uses boundary_user_prompt_appended: similarity then cannot be interpreted as same-input replay fidelity.

Unregenerable Claude observations fail unless --allow-stale-observations explicitly permits reuse with degraded quality.

--disable-thinking is also a CLI option. Replay has separate outer `--safe`, `--executor`, `--overlayfs-path` and `--overlayfs-compose` options or run/overlayfs/overlaynet TOML for managed execution. Use replay help; do not copy run `--stage`/`--mount` grammar directly. See [CLI replay](../reference/cli.md#replay-an-agent-trajectory).

## Experiments and development

The semantics and pinned adapter versions above define usage. The [Qwen3.6 experiment](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/replay/qwen3.6-results.md) preserves per-task results, next-action comparisons, and historical OpenCode diagnostics, without establishing compatibility or determinism.
