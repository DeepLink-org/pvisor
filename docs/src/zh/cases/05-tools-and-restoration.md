# 5. 回放轨迹与选择恢复工具

先判断要恢复的是文件提案、Agent 历史，还是完整机器执行状态。workspace checkpoint 用于文件提案；`pvisor-replay` 处理 Agent 原生轨迹；Job execution checkpoint 保存受支持 VM 的完整状态。TUI 用于交互审查，Gateway 用于模型路由和捕获，cache/memory-pool 用于执行基础设施，不是 Job checkpoint 的替代入口。

### S-USE-015：离线准备轨迹，不重跑历史工具

恢复 Agent 对话前先 prepare-only，确认解析前缀不会执行历史工具命令。需要同目录安装 pvisor-replay。

**语义**：回放前缀解析为 prepared，历史工具没有创建 marker，重放工具数量为零。

**违反示例**：prepare-only 执行历史命令。

<!-- semspec: case id=S-USE-015 -->
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

### S-USE-016：完整执行 checkpoint 必须检查能力

本例的 host Job 不支持 CPU/RAM 保存和恢复；兼容的原生 VM Job 支持。检查 capability，要求不支持的操作明确拒绝，不要偷偷改成 workspace fork。

**语义**：execution checkpoint、suspend、resume 和 execution fork 均报告 CAPABILITY_UNSUPPORTED，Job 文件树不变。

**违反示例**：忽略 execution 选项或拒绝后修改 Job。

<!-- semspec: case id=S-USE-016 -->
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

[返回学习路线](index.md)
