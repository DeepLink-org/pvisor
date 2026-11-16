# S-DOC-050：K02 选择性 apply 后丢弃剩余改动


准备：Linux user/mount namespace 可用。

**语义**：原 workspace 仅出现 `one.txt`；`two.txt` 从未进入 lower。

**理由**：只提交 `one.txt`，保留 `two.txt` 在 stage 中等待决定，然后显式丢弃剩余改动。

**违反示例**：仅选 one.txt 却把 two.txt 一并落地，或剩余 stage 未被丢弃。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-050 --timeout 180 <<'CASE'
> require_python3
> require_rootless
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --stage "$CASE_ROOT/partial-stage" -- /bin/sh -c \
>   'printf one > one.txt; printf two > two.txt'
> pvisor apply "$CASE_ROOT/partial-stage" --path one.txt
> pvisor drop "$CASE_ROOT/partial-stage"
> CASE_COMMAND
>
> test "$(cat one.txt)" = one
> test ! -e two.txt
> record_expect overlay.state discarded "$PVISOR_CASE_ROOT/partial-stage"
> CASE
PASS S-DOC-050
```
