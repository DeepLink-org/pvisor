# 1. From a command to a Job

Start with familiar shell commands. Ordinary host execution writes directly to the workspace and uses ambient networking. Continue to chapter 2 to review changes before accepting them. Each case runs independently.

### S-USE-001: Run and write directly

Use this for a trusted script whose execution you want to record. The original command follows `--`; `run` is optional.

**Contract**: The command writes hello.txt in the workspace; the Job completes without a staged changeset.

**Violation**: A successful command writes elsewhere or silently stages the file.

```bash
journey_setup
pvisor --name hello --stdio capture -- /bin/sh -c 'printf hello > hello.txt; cat hello.txt'
assert_content hello.txt hello
pvisor status last --json > "$CASE_ROOT/status.json"
json_expect "$CASE_ROOT/status.json" /run/state '"completed"'
json_expect "$CASE_ROOT/status.json" /run/overlay null
```

### S-USE-002: Inspect a failed Job

When a script fails, inspect its Job exit code instead of treating a file as success.

**Contract**: The CLI preserves child exit code 7 and records failed.

**Violation**: The exit code is lost or failure is recorded as completion.

```bash
journey_setup
expect_exit 7 pvisor run --stdio capture -- /bin/sh -c 'printf failed >&2; exit 7'
pvisor status last --json > "$CASE_ROOT/status.json"
json_expect "$CASE_ROOT/status.json" /run/state '"failed"'
```

### S-USE-003: Bound a long task

Use `--timeout` for unattended work to bound a stalled script.

**Contract**: A sleeping task is stopped by its deadline and reports deadline_exceeded.

**Violation**: The task sleeps to completion or reports success.

```bash
journey_setup
expect_refused pvisor run --timeout 100ms --stdio capture -- /bin/sleep 30
journey_bundle > "$CASE_ROOT/bundle.json"
json_expect "$CASE_ROOT/bundle.json" /run/failure/kind '"deadline_exceeded"'
```

### S-USE-004: Make a task repeatable

Use explicit `--config` to keep a stable task command and name; CLI arguments can override them.

**Contract**: CLI command and name override the TOML values; captured output is override.

**Violation**: Implicit configuration is loaded or CLI overrides are ignored.

```bash
journey_setup
cat > task.toml <<'TOML'
[run]
agent = "configured"
command = ["/bin/sh", "-c", "printf configured"]
TOML
pvisor run --config task.toml --name explicit --stdio capture -- /bin/sh -c 'printf override'
journey_bundle > "$CASE_ROOT/bundle.json"
json_expect "$CASE_ROOT/bundle.json" /run/agent '"explicit"'
json_expect "$CASE_ROOT/bundle.json" /run/output/stdout '"override"'
```

[Learning path](index.md) · [2. Review and accept file changes](02-review-and-decide.md)
