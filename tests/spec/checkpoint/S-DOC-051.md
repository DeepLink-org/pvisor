# S-DOC-051：K03 从已停止 Session fork


准备：Linux user/mount namespace 可用。

**语义**：子 Session 读到 `inherited`，但两个 Session 的变更都没有直接写入原 workspace。

**理由**：从源 Session 的暂存视图启动一个子 Session。子 Session 可以读取源改动，同时产生自己的独立变更。

**违反示例**：fork 读不到源 Session 变更，或子 Session 写入穿透原工作区。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-051 --timeout 180 <<'CASE'
> require_python3
> require_rootless
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --stage "$CASE_ROOT/source-stage" -- /bin/sh -c 'printf inherited > inherited.txt'
> pvisor fork "$CASE_ROOT/source-stage" -- /bin/sh -c \
>   'cat inherited.txt; printf child > child.txt' > child.out
> CASE_COMMAND
>
> test "$(cat child.out)" = inherited
> test ! -e inherited.txt
> test ! -e child.txt
> bundle_contains filesystem.changes inherited.txt "$PVISOR_CASE_ROOT/source-stage"
> bundle_contains filesystem.changes child.txt "$PVISOR_CASE_RECORDS"
> CASE
PASS S-DOC-051
```
