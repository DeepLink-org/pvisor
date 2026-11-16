# S-DOC-055：M01 离线准备回放前缀


**语义**：输出结果的 phase 为 `prepared`，历史命令没有创建 `marker`。

**理由**：用一个最小 mini-swe-agent 原生轨迹验证 `replay --prepare-only`。该模式解析前缀，不启动 Agent，也不执行历史工具。

**违反示例**：prepare-only 执行了历史命令，创建 marker，或报告回放过工具调用。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-055 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run success <<'CASE_COMMAND'
> cat > trajectory.json <<'JSON'
> {"trajectory_format":"mini-swe-agent-1.1","info":{"mini_version":"2.4.6"},"messages":[{"role":"assistant","content":"historical action","extra":{"response":{},"actions":[{"tool_call_id":"call-1","command":"printf should-not-run > marker"}]}},{"role":"tool","content":"old observation","extra":{"returncode":0}}]}
> JSON
> pvisor replay --agent mini-swe-agent --trajectory ./trajectory.json \
>   --after-step 1 --prepare-only \
>   --state-dir "$CASE_ROOT/replay-state" \
>   --output-dir "$CASE_ROOT/replay-output" > prepared.json
> CASE_COMMAND
>
> test ! -e marker
> python3 -c 'import json; d=json.load(open("prepared.json")); assert d["phase"] == "prepared" and d["replayed_tool_calls"] == 0'
> CASE
PASS S-DOC-055
```
