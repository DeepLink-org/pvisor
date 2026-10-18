# Replay design

Replay turns a history of agent tool calls into fresh results from the current workspace, then supplies those results to the agent for continuation. It supports reproducing failures, comparing behavior after a fix, and constructing training prefixes.

Follow the [trajectory replay guide](../guides/replay.md) to use it. The design below explains boundary selection, tool re-execution, and native agent sessions so you can decide whether replay results are comparable.

## Current processing pipeline

`engine.rs::execute` validates requests/directories, calls `adapter::build_plan` to parse native trajectories/select complete tool batches, validates budgets/pinned agent versions, then allocates separate state/output directories. `after_step` must fall within existing complete batches; it cannot split calls from one turn.

| Phase | Purpose and artifacts |
| --- | --- |
| Prepare | Parse history, select boundary and write reconstructed files; `--prepare-only` executes no historical tools and launches no live agent |
| Replay | Re-execute supported tool prefixes in the current workspace, replacing old results with fresh observations; `--replay-only` stops at the boundary |
| Continue | Hand native reconstructed context to the pinned agent and validate that its first continuation request uses the prefix; inject boundary prompts after fresh observations |
| Finalize | Save results, quality, failures, reconstructed trajectories and continuation artifacts with actionable failure locations |

`adapter/` owns parsing, tool semantics and launch selection; `bridge/` owns model protocol bridges for Claude/Codex/OpenCode. Adapter mechanisms and pinned versions are listed below; a pinned version declares "this version is verified", not "compatible with arbitrary new versions".

## Adapter mechanisms and pinned versions

### Claude Code

Claude Code uses native JSONL/UUID sessions. SandboxReplay walks the active parent-UUID chain, replays the first N complete tool batches, writes the new observations into a rebuilt session, and then starts continuation through `claude --resume`.

Claude Code's resume transport inserts temporary messages before the model request, so SandboxReplay starts a local protocol bridge that serves this continuation only. The bridge validates and removes the exactly matching temporary envelope, so the first model request still ends at `O′N`. It does not enable the pVisor Gateway and neither captures nor persists model traffic.

Asynchronous sub-agent `Agent` and `TaskOutput` calls are treated as native Claude Code tools. The recovery boundary must be chosen where the native session can rebuild it unambiguously.

### OpenHands

OpenHands uses its native event trajectory and ReplayManager. SandboxReplay extracts the first N Actions; the OpenHands Runtime executes them in the new sandbox and produces fresh Observations. When the replay queue empties, OpenHands issues the continuation request directly.

### mini-swe-agent

mini-swe-agent uses native `mini-swe-agent-1.1` messages. The companion runner keeps the original system and task messages, replays the first N actions in the new sandbox, adds the results to `agent.messages` with the native observation formatter, and then calls the next `agent.step()` directly.

### Pi agent

The Pi agent adapter pins `@earendil-works/pi-coding-agent` `0.83.0` and takes native Pi RPC event JSONL as input. One replay step is one complete `turn_end` tool batch. SandboxReplay reruns `read`, `bash`, `edit`, and `write` with Pi's own tool implementations, writes the new observations into a fresh Pi v3 session, and then continues from the boundary through the Pi SDK. Trajectories that call any tool outside these four are refused, so tool semantics never change silently. Without a configured boundary prompt it calls Pi's native `continue()`; with one, it appends a single user message through `prompt()` after `O′N`.

### OpenCode

The OpenCode adapter pins `1.17.7` and takes the native event JSONL produced by `opencode run --format=json`. Events `user`, `step_start`, `text`, `reasoning`, `tool_use`, and `step_finish` are grouped by step; one replay step is one complete tool-call batch. SandboxReplay reruns command tools and the `read`, `write`, and `edit` file tools in the new sandbox, rebuilds the prefix from the new `state.output`, and continues from the boundary through `opencode run --format=json --session`. Intermediate `tool_use` states during tool execution are merged so the same call is not replayed twice.

`opencode run` has no CLI step-count option, and 1.17.7 does not enforce the `agent.build.steps` budget on a resumed session. SandboxReplay therefore adds two external watchdogs for OpenCode only: pVisor watches the stderr progress log for continuation steps and sends SIGINT once the remaining budget is reached; ten minutes with no output triggers idle termination. A budget trigger produces `agent_status = max_steps` and records `opencode_step_budget` in the metadata. These mechanisms target the pinned adapter version; for the original measurements and diagnostics see the [experiment report](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/replay/qwen3.6-results.md).

### Codex

The Codex adapter pins CLI `0.149.0` and takes native Codex rollout JSONL as input. One replay step is one complete set of `function_call`/`custom_tool_call` entries and their outputs. SandboxReplay replays the prefix tools in the new sandbox, writes the native `response_item` prefix into an isolated `CODEX_HOME`, and runs `codex exec resume <session-id> --json`. The native session ID is extracted automatically from `session_meta`; unknown tools or a missing native session ID fail explicitly instead of degrading into a brand-new session.

Codex keeps its own system prompt, tool definitions, and task context; SandboxReplay replaces only the observations before the boundary. To stay compatible with older CLIs that require a resume prompt, SandboxReplay uses a local Responses bridge that removes the transport nonce before forwarding the request to the model service, and cleans the nonce from the native trajectory after continuation. The default mode's input condition is `replayed_boundary_only` and injects no extra prompt into the model.

When `boundary_user_prompt` is configured, the prompt is injected once after `O′N` and retained; the input condition is then marked `boundary_user_prompt_appended`. A bridge validation failure refuses continuation rather than degrading to a direct connection.

Replaying tools repeats file/command and potentially remote effects. Filesystem checkpoints do not preserve external state. Unrefreshable observations fail closed by default; explicit permission records degradation. Fresh tool outputs can differ and the continuing model can choose different actions. Successful continuation is not deterministic reproduction of the original trajectory.

Historical Claude `Read` accepts only regular files within the workspace. Existing symlinks resolving inside the workspace remain supported. The actual open walks directory FDs without following replacement symlinks, uses nonblocking opens, and checks the opened inode to reject FIFOs, directories and devices. A FIFO returns a tool error without a writer or an outer Run timeout.

Each `Read` scans at most 16 MiB, probing one additional byte to detect truncation, and returns at most 2,000 file-content lines, with the entire tool content bounded to 4 MiB. `offset` starts at 1; explicit `limit` keeps its meaning within these budgets. Reaching a scan, line or output-byte budget retains the available content, appends one `Read truncated by pvisor replay` notice line, and sets the observation's `truncated=true`. Including this notice, the result has at most 2,001 lines. 

An explicit range completed before the scan boundary is not marked truncated merely because the rest of the file is large. Offsets beyond the scan budget also receive the notice; this result does not establish EOF for the full file.

Regular files can still change concurrently. These budgets provide neither a file snapshot nor an I/O deadline for remote or faulty filesystems.

## Code and validation

```bash
just test pvisor-replay
```

- `adapter/claude_code.rs`: complete-batch grouping, prepare-only without execution, stale-observation rejection/explicit degradation, Read ranges/output budgets, historical command deadlines/process-group cleanup;
- `io.rs`: Read regular-file checks, directory-FD confinement and regression coverage for symlink substitution after resolution;
- `adapter/generic.rs`: Codex/OpenCode native identities, complete tool turns, continuation-prefix validation and transport nonce filtering;
- `tests/replay_contract.rs`: mini-swe-agent, SWE-agent, OpenHands and Pi replay-only boundaries, prompt injection and step budgets;
- `bridge/`: request/response mappings and boundary validation.

See [Replay fidelity](../benchmarks/replay-fidelity.md) for native-prefix preparation results across six adapters. The experiment validates prefixes and zero-execution preparation; continuation quality requires real model tasks.
