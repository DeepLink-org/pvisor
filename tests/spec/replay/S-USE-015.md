# S-USE-015：离线准备轨迹，不重跑历史工具

恢复 Agent 对话前先 prepare-only，确认解析前缀不会执行历史工具命令。需要同目录安装 pvisor-replay。

**语义**：回放前缀解析为 prepared，历史工具没有创建 marker，重放工具数量为零。

**违反示例**：prepare-only 执行历史命令。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-USE-015 --timeout 180 <<'CASE'
> journey_setup
> cat > trajectory.json <<'JSON'
> {"trajectory_format":"mini-swe-agent-1.1","info":{"mini_version":"2.4.6"},"messages":[{"role":"assistant","content":"historical action","extra":{"response":{},"actions":[{"tool_call_id":"call-1","command":"printf historical > marker"}]}},{"role":"tool","content":"old observation","extra":{"returncode":0}}]}
> JSON
> pvisor replay --agent mini-swe-agent --trajectory ./trajectory.json --after-step 1 --prepare-only --state-dir "$CASE_ROOT/replay-state" --output-dir "$CASE_ROOT/replay-output" > "$CASE_ROOT/prepared.json"
> assert_absent marker
> json_expect "$CASE_ROOT/prepared.json" /phase '"prepared"'
> json_expect "$CASE_ROOT/prepared.json" /replayed_tool_calls 0
> CASE
PASS S-USE-015
```
