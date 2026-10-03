# 5. Replay trajectories and choose restoration tools

Choose whether you need a file proposal, Agent history, or full machine execution state. Workspace checkpoints save files; pvisor-replay handles Agent-native trajectories; independent snapshot saves full VMs. TUI supports interactive review, Gateway handles model routing and capture, and cache/memory-pool serve execution infrastructure.

### S-USE-015: Prepare a trajectory without rerunning historical tools

Before resuming an Agent conversation, use prepare-only to check that prefix parsing does not execute historical tools. Install pvisor-replay alongside pvisor.

**Contract**: The prefix is prepared without creating marker; replayed_tool_calls is zero.

**Violation**: prepare-only executes historical commands.

```bash
journey_setup
cat > trajectory.json <<'JSON'
{"trajectory_format":"mini-swe-agent-1.1","info":{"mini_version":"2.4.6"},"messages":[{"role":"assistant","content":"historical action","extra":{"response":{},"actions":[{"tool_call_id":"call-1","command":"printf historical > marker"}]}},{"role":"tool","content":"old observation","extra":{"returncode":0}}]}
JSON
pvisor replay --agent mini-swe-agent --trajectory ./trajectory.json --after-step 1 --prepare-only --state-dir "$CASE_ROOT/replay-state" --output-dir "$CASE_ROOT/replay-output" > "$CASE_ROOT/prepared.json"
assert_absent marker
json_expect "$CASE_ROOT/prepared.json" /phase '"prepared"'
json_expect "$CASE_ROOT/prepared.json" /replayed_tool_calls 0
```

### S-USE-016: Check full execution checkpoint capabilities

Ordinary Jobs do not yet support CPU/RAM save and restore. Check capability and require explicit rejection instead of silently falling back to workspace fork.

**Contract**: Execution checkpoint, suspend, resume, and execution fork all report CAPABILITY_UNSUPPORTED and leave the Job tree unchanged.

**Violation**: An execution option is ignored or a refusal changes the Job.

```bash
journey_setup
pvisor run --stage "$CASE_ROOT/source" -- /bin/sh -c 'printf proposal > report.txt'
pvisor status "$CASE_ROOT/source" --json > "$CASE_ROOT/status.json"
json_expect "$CASE_ROOT/status.json" /checkpoint_capability/execution false
snapshot before "$CASE_ROOT/source"
expect_refused pvisor checkpoint create "$CASE_ROOT/source" --kind execution > "$CASE_ROOT/error.txt" 2>&1
journey_contains "$CASE_ROOT/error.txt" CAPABILITY_UNSUPPORTED
expect_refused pvisor suspend "$CASE_ROOT/source" > "$CASE_ROOT/error.txt" 2>&1
journey_contains "$CASE_ROOT/error.txt" CAPABILITY_UNSUPPORTED
expect_refused pvisor resume "$CASE_ROOT/source" > "$CASE_ROOT/error.txt" 2>&1
journey_contains "$CASE_ROOT/error.txt" CAPABILITY_UNSUPPORTED
expect_refused pvisor fork "$CASE_ROOT/source" --state execution > "$CASE_ROOT/error.txt" 2>&1
journey_contains "$CASE_ROOT/error.txt" CAPABILITY_UNSUPPORTED
assert_unchanged before "$CASE_ROOT/source"
```

[Learning path](index.md)
