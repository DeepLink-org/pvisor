# S-USE-003：给长任务设置期限

无人值守任务使用 `--timeout`，避免脚本无限等待。

**语义**：休眠任务被期限中止，Run Bundle 报告 deadline_exceeded。

**违反示例**：任务睡到结束或被记成普通成功。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-USE-003 --timeout 180 <<'CASE'
> journey_setup
> expect_refused pvisor spawn --timeout 100ms --stdio capture -- /bin/sleep 30
> journey_bundle > "$CASE_ROOT/bundle.json"
> json_expect "$CASE_ROOT/bundle.json" /run/failure/kind '"deadline_exceeded"'
> CASE
PASS S-USE-003
```
