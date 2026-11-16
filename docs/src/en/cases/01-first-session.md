# 1. From a command to a Session

Start with familiar shell commands. Ordinary host execution writes directly to the workspace and uses ambient networking. Continue to chapter 2 to review changes before accepting them. Each case runs independently.

### S-USE-001: Run and write directly

Use this for a trusted script whose execution you want to record. The original command follows `--`; `run` is optional.

**Contract**: The command writes hello.txt in the workspace; the Session completes without a staged changeset.

**Violation**: A successful command writes elsewhere or silently stages the file.

Execution and assertions: [Scrut S-USE-001](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-USE-001.md).

### S-USE-002: Inspect a failed Session

When a script fails, inspect its Session exit code instead of treating a file as success.

**Contract**: The CLI preserves child exit code 7 and records failed.

**Violation**: The exit code is lost or failure is recorded as completion.

Execution and assertions: [Scrut S-USE-002](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-USE-002.md).

### S-USE-003: Bound a long task

Use `--timeout` for unattended work to bound a stalled script.

**Contract**: A sleeping task is stopped by its deadline and reports deadline_exceeded.

**Violation**: The task sleeps to completion or reports success.

Execution and assertions: [Scrut S-USE-003](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-USE-003.md).

### S-USE-004: Make a task repeatable

Use explicit `--config` to keep a stable task command and name; CLI arguments can override them.

**Contract**: CLI command and name override the TOML values; captured output is override.

**Violation**: Implicit configuration is loaded or CLI overrides are ignored.

Execution and assertions: [Scrut S-USE-004](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-USE-004.md).

[Learning path](index.md) · [2. Review and accept file changes](02-review-and-decide.md)
