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

`adapter/` owns parsing, tool semantics and launch selection; `bridge/` owns model protocol bridges for Claude/Codex/OpenCode. See [Replay guide](../guides/replay.md) for adapters/versions; pinned support does not promise compatibility with arbitrary new versions.

Replaying tools repeats file/command and potentially remote effects. Filesystem checkpoints do not preserve external state. Unrefreshable observations fail closed by default; explicit permission records degradation. Fresh tool outputs can differ and the continuing model can choose different actions. Successful continuation is not deterministic reproduction of the original trajectory.

Historical Claude `Read` accepts only regular files within the workspace. Existing symlinks resolving inside the workspace remain supported. The actual open walks directory FDs without following replacement symlinks, uses nonblocking opens, and checks the opened inode to reject FIFOs, directories and devices. A FIFO returns a tool error without a writer or an outer Run timeout.

Each `Read` scans at most 16 MiB, probing one additional byte to detect truncation, and returns at most 2,000 file-content lines, with the entire tool content bounded to 4 MiB. `offset` starts at 1; explicit `limit` keeps its meaning within these budgets. Reaching a scan, line or output-byte budget retains the available content, appends one `Read truncated by pvisor replay` notice line, and sets the observation's `truncated=true`. Including this notice, the result has at most 2,001 lines. An explicit range completed before the scan boundary is not marked truncated merely because the rest of the file is large. Offsets beyond the scan budget also receive the notice; this result does not establish EOF for the full file. Regular files can still change concurrently. These budgets provide neither a file snapshot nor an I/O deadline for remote or faulty filesystems.

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
