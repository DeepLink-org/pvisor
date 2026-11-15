# S-DOC-022：D05 显式共享路径的直接写入


准备：Linux user/mount namespace 可用。

**语义**：共享目录的 `out` 直接写入宿主 lower；无需对它执行 `pvisor apply`。

**理由**：用 `--mount SOURCE:write` 授予一个工作区之外的宿主目录可写访问。

**违反示例**：明确授予 write 的共享路径没有获得 mounted 内容。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-022 --timeout 180 <<'CASE'
> require_python3
> require_rootless
> case_setup
> case_run success <<'CASE_COMMAND'
> mkdir -p "$CASE_ROOT/shared"
> pvisor --mount "$CASE_ROOT/shared":write -- \
>   /bin/sh -c 'printf mounted > "$1"' sh "$CASE_ROOT/shared/out"
> CASE_COMMAND
>
> test "$(cat "$PVISOR_CASE_ROOT/shared/out")" = mounted
> CASE
PASS S-DOC-022
```
