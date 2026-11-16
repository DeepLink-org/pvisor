# S-DOC-052：K04 终止运行中的 Session


准备：Linux user/mount namespace 可用。

**语义**：Session 在睡眠结束前退出，`status --json` 报告 `cancelled`，不再处于 live 状态。

**理由**：让一个长时间运行的 Session 进入后台，然后按 stage 路径请求正常终止。

**违反示例**：kill 后 Session 继续 live，或终态不是 cancelled。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-052 --timeout 180 <<'CASE'
> require_python3
> require_rootless
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --stage "$CASE_ROOT/live-stage" -- /bin/sleep 30 > live.log 2>&1 &
> session_pid=$!
> for ((execution=0; execution<100; execution++)); do
>   test -f "$CASE_ROOT/live-stage/run.json" && break
>   sleep 0.05
> done
> pvisor kill "$CASE_ROOT/live-stage"
> if wait "$session_pid"; then exit 1; fi
> pvisor status --json "$CASE_ROOT/live-stage" > stopped.json
> CASE_COMMAND
>
> python3 -c 'import json; d=json.load(open("stopped.json")); assert d["run"]["state"] == "cancelled" and d["live"] is False'
> CASE
PASS S-DOC-052
```
