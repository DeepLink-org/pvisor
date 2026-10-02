# SandboxReplay

SandboxReplay replays an agent trajectory inside a fresh sandbox you prepared: it reruns the tool calls before a recovery boundary, rebuilds the agent's native context from the observations that new sandbox produces, and then continues running from the boundary. It creates a derived Run in the [Run and Attempt model](../design/execution-model.md).

## 1. Concepts

An agent trajectory can be written as:

~~~text
Task → A1 → O1 → ... → AN → ON → A(N+1) → ...
~~~

- `Ai`: the i-th action produced by the model, which may contain visible text, reasoning, and one or more tool calls.
- `Oi`: the observation returned to the model after executing `Ai`.
- `N`: the selected recovery boundary.
- `A(N+1)`: the original trajectory's next model reply after the recovery boundary.
- `A′(N+1)`: the first continuation reply the model produces after replaying the first N actions in the new sandbox.

When an assistant response contains several parallel tool calls, they form one action batch, and the recovery boundary cannot fall inside a batch.

## 2. Replay semantics

SandboxReplay does two things in order:

1. It reruns the tool calls of `A1...AN` in the new sandbox, in the original order, producing the new sandbox's real observations `O′1...O′N`.
2. It keeps the agent's native system prompt, tool definitions, task, and prior actions, replaces the old observations with `O′1...O′N`, and then requests the next model inference directly.

The first continuation request must end exactly at `O′N`:

~~~text
Native system prompt + tool definitions + original task
    + A1 → O′1 → ... → AN → O′N
                              ^ request ends here
~~~

Do not append extra messages such as "please continue" or "Continue from where you left off" after `O′N`. Replay guarantees a correct recovery flow and message boundary, but it does not guarantee that `A′(N+1)` matches `A(N+1)` verbatim: file state, dynamic fields in tool output, and model sampling can all change the next action.

When you explicitly want to change the first inference after the boundary, configure `boundary_user_prompt`:

~~~text
Native system prompt + tool definitions + original task
    + A1 → O′1 → ... → AN → O′N
    + boundary_user_prompt → A′(N+1)
~~~

The prompt is injected once after `O′N` and before the first live model inference, and it does not replace the original task. `prepare-only` and `replay-only` send no live model request, so they never inject it. Without configuration, the original "the request ends exactly at `O′N`" semantics still hold.

## 3. Agent adapters

### 3.1 Claude Code

Claude Code uses native JSONL/UUID sessions. SandboxReplay walks the active parent-UUID chain, replays the first N complete tool batches, writes the new observations into a rebuilt session, and then starts continuation through `claude --resume`.

Claude Code's resume transport inserts temporary messages before the model request, so SandboxReplay starts a local protocol bridge that serves this continuation only. The bridge validates and removes the exactly matching temporary envelope, so the first model request still ends at `O′N`. It does not enable the pVisor Gateway and neither captures nor persists model traffic.

Asynchronous sub-agent `Agent` and `TaskOutput` calls are treated as native Claude Code tools. The recovery boundary must be chosen where the native session can rebuild it unambiguously.

### 3.2 OpenHands

OpenHands uses its native event trajectory and ReplayManager. SandboxReplay extracts the first N Actions; the OpenHands Runtime executes them in the new sandbox and produces fresh Observations. When the replay queue empties, OpenHands issues the continuation request directly.

### 3.3 mini-swe-agent

mini-swe-agent uses native `mini-swe-agent-1.1` messages. The companion runner keeps the original system and task messages, replays the first N actions in the new sandbox, adds the results to `agent.messages` with the native observation formatter, and then calls the next `agent.step()` directly.

### 3.4 Pi agent

The Pi agent adapter pins `@earendil-works/pi-coding-agent` `0.83.0` and takes native Pi RPC event JSONL as input. One replay step is one complete `turn_end` tool batch. SandboxReplay reruns `read`, `bash`, `edit`, and `write` with Pi's own tool implementations, writes the new observations into a fresh Pi v3 session, and then continues from the boundary through the Pi SDK. Trajectories that call any tool outside these four are refused, so tool semantics never change silently. Without a configured boundary prompt it calls Pi's native `continue()`; with one, it appends a single user message through `prompt()` after `O′N`.

### 3.5 OpenCode

The OpenCode adapter pins `1.17.7` and takes the native event JSONL produced by `opencode run --format=json`. Events `user`, `step_start`, `text`, `reasoning`, `tool_use`, and `step_finish` are grouped by step; one replay step is one complete tool-call batch. SandboxReplay reruns command tools and the `read`, `write`, and `edit` file tools in the new sandbox, rebuilds the prefix from the new `state.output`, and continues from the boundary through `opencode run --format=json --session`. Intermediate `tool_use` states during tool execution are merged so the same call is not replayed twice.

`opencode run` has no CLI step-count option, and 1.17.7 does not enforce the `agent.build.steps` budget on a resumed session. SandboxReplay therefore adds two external watchdogs for OpenCode only; they are not enabled for other agents.

pVisor watches the stderr progress log for continuation steps and sends SIGINT once the remaining budget is reached. Ten minutes with no output triggers idle termination. A budget trigger produces `agent_status = max_steps` and records `opencode_step_budget` in the metadata. These mechanisms target the pinned adapter version; for the original measurements and diagnostics see the [experiment report](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/replay/qwen3.6-results.md).

### 3.6 Codex

The Codex adapter pins CLI `0.149.0` and takes native Codex rollout JSONL as input. One replay step is one complete set of `function_call`/`custom_tool_call` entries and their outputs. SandboxReplay replays the prefix tools in the new sandbox, writes the native `response_item` prefix into an isolated `CODEX_HOME`, and runs `codex exec resume <session-id> --json`. The native session ID is extracted automatically from `session_meta`; unknown tools or a missing native session ID fail explicitly instead of degrading into a brand-new session.

Codex keeps its own system prompt, tool definitions, and task context; SandboxReplay replaces only the observations before the boundary. To stay compatible with older CLIs that require a resume prompt, SandboxReplay uses a local Responses bridge that removes the transport nonce before forwarding the request to the model service, and cleans the nonce from the native trajectory after continuation. The default mode's input condition is `replayed_boundary_only` and injects no extra prompt into the model.

When `boundary_user_prompt` is configured, the prompt is injected once after `O′N` and retained; the input condition is then marked `boundary_user_prompt_appended`. A bridge validation failure refuses continuation rather than degrading to a direct connection.

## 4. Usage

### 4.1 Install pVisor

The pVisor CLI ships in the `pvisor` wheel. When the sandbox has Python 3.10 or newer, install the released version directly:

~~~bash
python -m pip install pvisor

command -v pvisor
pvisor --version
pvisor replay --help
~~~

To test unreleased SandboxReplay code, install only pVisor from source inside the target sandbox:

~~~bash
git clone https://github.com/DeepLink-org/pvisor.git
cd pvisor
just install-cli

export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
pvisor replay --help
~~~

During development you can skip installation and build and use the in-repo binary:

~~~bash
just build release
./target/release/pvisor replay --help
~~~

`pvisor replay` usually runs directly inside the new sandbox you prepared, so pVisor must be installed in that sandbox or mounted read-only on the sandbox's `PATH`. If you build outside the sandbox and copy the binary in, the build machine and the target sandbox must share a compatible operating system, CPU architecture, and dynamic runtime. The agent runtime must also match the pinned version of the replay profile you select.

### 4.2 CLI and TOML

SandboxReplay assumes by default that you have already created a new sandbox and are running inside it:

~~~bash
pvisor replay \
  --agent claude-code \
  --trajectory /input/session.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/claude \
  --boundary-user-prompt '请检查新的 observation 后继续任务'
~~~

When the Pi agent runtime is installed at `/opt/pi-agent`, the command is:

~~~bash
pvisor replay \
  --agent pi-agent \
  --trajectory /input/pi-agent.events.jsonl \
  --after-step 30 \
  --agent-entrypoint /opt/pi-agent/bin/pi
~~~

The equivalent pVisor TOML for the Pi agent is:

~~~toml
[replay]
agent = "pi-agent"
trajectory = "/input/pi-agent.events.jsonl"
after_step = 30
agent_entrypoint = "/opt/pi-agent/bin/pi"
max_steps = 200
disable_thinking = true
~~~

OpenCode uses native event JSONL:

~~~bash
pvisor replay \
  --agent opencode \
  --trajectory /input/opencode.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/opencode
~~~

Codex uses native rollout JSONL; SandboxReplay reads the Codex native session ID from the trajectory, so you never fill in `session_id` by hand:

~~~bash
pvisor replay \
  --agent codex \
  --trajectory /input/rollout.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/codex
~~~

Both can also share one TOML file; just change `[replay].agent`, `trajectory`, and `agent_entrypoint` to `opencode` or `codex`.

The equivalent pVisor TOML for Claude Code is:

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

- The default mode executes the selected prefix, then starts the agent to continue running.
- `--replay-only` executes the prefix but stops before the next model request.
- `--prepare-only` only validates and builds the prefix; it runs no tools, starts no agent, and requires no agent runtime.

`--max-steps` is the agent's total action budget including the replayed prefix. For example, `--after-step 30 --max-steps 50` leaves at most 20 continuation actions. Replay-only's budget must cover the prefix; continuation must leave at least one live action.

The result protocol is `sandbox-playback.result/v3`: `phase` is `prepared`, `replayed`, or `continued`; `quality` is `verified` or `degraded`; `agent_status` distinguishes `not_started`, `completed`, `max_steps`, and `failed`. Failed results keep the logs and native trajectory already produced. Even when the OpenHands process exits 0, the result is still a failure if the controller reports a fatal status.

Metadata on a successful result records whether the boundary prompt was requested and injected, along with its character length and SHA-256; the replay journal never records the prompt plaintext, and the agent's native prepared or continued trajectory may contain this user message. For Claude Code, the in-memory bridge adds the prompt only to the first cleaned upstream request and does not modify the rebuilt native session. After a prompt is configured, `next-action-comparison.json` uses the input condition `boundary_user_prompt_appended`. Text similarity and tool agreement are then observational only and cannot be read as replay fidelity under identical input.

Claude observations that cannot be regenerated fail by default; only an explicit `--allow-stale-observations` reuses them and marks quality `degraded`.

`disable_thinking` can also be set with `--disable-thinking`. Replay takes its own outer run options, separate from the agent flags: `--safe`, `--executor`, `--overlayfs-path`, `--overlayfs-compose`, and other managed-execution options, or the `[run]`, `[overlayfs]`, and `[overlaynet]` TOML tables. Its filesystem parameters are governed by `pvisor replay --help`; do not copy `run`'s `--stage/--mount` grammar directly.

See the [`pvisor replay` command reference](../reference/cli.md#replay-an-agent-trajectory) for the full parameter list.

## Experiments and development material

Usage and guarantees are governed by the replay semantics and pinned adapter versions above. The [Qwen3.6 experiment report](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/replay/qwen3.6-results.md) preserves per-task results, next-action comparisons, and the OpenCode diagnostics of the time; it is not a compatibility or determinism guarantee.
