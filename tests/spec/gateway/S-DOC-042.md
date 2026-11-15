# S-DOC-042：H02 JSON 记录


建议场景：适合接入 Gateway、模型路由或记录轨迹的任务。

**语义**：指定文件非空，首行具有 JSON 对象形式。每行应是一个事件；本例只检查文件建立和首行外观。

**理由**：把本次运行事件写成 JSONL 文件。

**违反示例**：事件文件为空，或首行不是 JSON 对象。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-042 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --record-destination "$CASE_ROOT/events.jsonl" -- "$CASE_TRUE"
> CASE_COMMAND
>
> test -s "$PVISOR_CASE_ROOT/events.jsonl"
> head -n 1 "$PVISOR_CASE_ROOT/events.jsonl" | grep -q '^{'
> CASE
PASS S-DOC-042
```
