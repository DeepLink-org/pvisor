# Replay recorded trajectories

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

## 3. Install and run location

`pvisor replay` usually runs directly inside the new sandbox you prepared, so pVisor must be installed in that sandbox or mounted read-only on the sandbox's `PATH`. When the sandbox has Python 3.10 or newer, install the released version directly:

~~~bash
python -m pip install pvisor

command -v pvisor
pvisor --version
pvisor replay --help
~~~

If you build outside the sandbox and copy the binary in, the build machine and the target sandbox must share a compatible operating system, CPU architecture, and dynamic runtime. The agent runtime must also match the pinned version of the replay profile you select; adapter mechanisms and pinned versions are documented in [Replay design](../design/replay.md).

## 4. Minimal usage

~~~bash
pvisor replay \
  --agent claude-code \
  --trajectory /input/session.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/claude
~~~

Execution modes (default continuation, `--replay-only`, `--prepare-only`), step budgets, the boundary prompt, the `sandbox-playback.result/v3` result protocol, and per-agent CLI/TOML forms are governed by the [`pvisor replay` command reference](../reference/cli.md#replay-an-agent-trajectory).

## Experiments and development material

Usage and guarantees are governed by the replay semantics and pinned adapter versions above. The [Qwen3.6 experiment report](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/replay/qwen3.6-results.md) preserves per-task results, next-action comparisons, and the OpenCode diagnostics of the time; it is not a compatibility or determinism guarantee.
