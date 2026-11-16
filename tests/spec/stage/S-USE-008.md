# S-USE-008：终止任务后再决定

任务卡住时，先 `kill`，确认已经停止，再丢弃文件。不要对 live Session 做提交。

**语义**：live Session 拒绝 apply；kill 后状态为 cancelled 且 live 为 false，原件没有改动。

**违反示例**：live apply 成功，kill 后仍在运行或文件穿透。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-USE-008 --timeout 180 <<'CASE'
> journey_setup
> pvisor spawn --stage "$CASE_ROOT/live" -- /bin/sh -c 'printf ready > ready.txt; exec /bin/sleep 30' > "$CASE_ROOT/live.log" 2>&1 &
> runner=$!
> trap 'kill "$runner" 2>/dev/null || true; wait "$runner" 2>/dev/null || true' EXIT
> journey_wait_file "$CASE_ROOT/live/upper/ready.txt" "$runner"
> expect_refused pvisor apply "$CASE_ROOT/live" --all
> pvisor kill "$CASE_ROOT/live"
> expect_refused wait "$runner"
> trap - EXIT
> pvisor status "$CASE_ROOT/live" --json > "$CASE_ROOT/status.json"
> json_expect "$CASE_ROOT/status.json" /run/state '"cancelled"'
> json_expect "$CASE_ROOT/status.json" /live false
> pvisor drop "$CASE_ROOT/live"
> assert_absent ready.txt
> CASE
PASS S-USE-008
```
