# S-DOC-049：K01 审查并只读查看暂存文件


准备：Linux user/mount namespace 可用。

**语义**：审查结果列出一个文件；只读视图能读到 `staged`，写入被拒绝，原 workspace 不变。

**理由**：运行后通过 `status --review` 查看变更，再用 `inspect` 读取暂存视图，并确认 inspect 无法写入。

**违反示例**：inspect 可以写暂存视图，或原工作区出现 note.txt。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-049 --timeout 180 <<'CASE'
> require_python3
> require_rootless
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --stage "$CASE_ROOT/review-stage" -- /bin/sh -c 'printf staged > note.txt'
> pvisor status --review --json "$CASE_ROOT/review-stage" > status.json
> pvisor inspect "$CASE_ROOT/review-stage" -- /bin/cat note.txt
> if pvisor inspect "$CASE_ROOT/review-stage" -- /bin/sh -c 'printf changed > note.txt'; then
>   exit 1
> fi
> CASE_COMMAND
>
> stdout_has staged
> test ! -e note.txt
> python3 -c 'import json; d=json.load(open("status.json")); assert d["filesystem"]["changed_files"] == 1'
> CASE
PASS S-DOC-049
```
