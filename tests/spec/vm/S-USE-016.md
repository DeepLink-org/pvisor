# S-USE-016：完整执行 checkpoint 必须检查能力

本例的 host Session 不支持 CPU/RAM 保存和恢复；兼容的原生 VM Session 支持。检查 capability，要求不支持的操作明确拒绝，不要偷偷改成 workspace fork。

**语义**：execution checkpoint、suspend、resume 和 execution fork 均报告 CAPABILITY_UNSUPPORTED，Session 文件树不变。

**违反示例**：忽略 execution 选项或拒绝后修改 Session。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-USE-016 --timeout 180 <<'CASE'
> journey_setup
> pvisor spawn --stage "$CASE_ROOT/source" -- /bin/sh -c 'printf proposal > report.txt'
> pvisor status "$CASE_ROOT/source" --json > "$CASE_ROOT/status.json"
> json_expect "$CASE_ROOT/status.json" /checkpoint_capability/execution false
> snapshot before "$CASE_ROOT/source"
> expect_refused pvisor checkpoint create "$CASE_ROOT/source" --kind execution > "$CASE_ROOT/error.txt" 2>&1
> journey_contains "$CASE_ROOT/error.txt" CAPABILITY_UNSUPPORTED
> expect_refused pvisor suspend "$CASE_ROOT/source" > "$CASE_ROOT/error.txt" 2>&1
> journey_contains "$CASE_ROOT/error.txt" CAPABILITY_UNSUPPORTED
> expect_refused pvisor resume "$CASE_ROOT/source" > "$CASE_ROOT/error.txt" 2>&1
> journey_contains "$CASE_ROOT/error.txt" CAPABILITY_UNSUPPORTED
> expect_refused pvisor fork "$CASE_ROOT/source" --state execution > "$CASE_ROOT/error.txt" 2>&1
> journey_contains "$CASE_ROOT/error.txt" CAPABILITY_UNSUPPORTED
> assert_unchanged before "$CASE_ROOT/source"
> CASE
PASS S-USE-016
```
