# S-DOC-019：D02 显式 host executor


建议场景：适合检查 OverlayFS 视图和 host 安全边界。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：Linux 记录为 `rootless_process`，macOS 记录为 `sandboxed_process`；两者都不应降级为 host process。

**理由**：显式选择 host executor，观察当前系统上的隔离类型。

**违反示例**：显式 host 请求降级为 host_process，而记录仍被当作隔离成功。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-019 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --executor host -- "$CASE_TRUE"
> CASE_COMMAND
>
> bundle_expect run.executor.kind process
> if [ "$(uname -s)" = "Darwin" ]; then
>   bundle_expect run.executor.isolation sandboxed_process
> else
>   bundle_expect run.executor.isolation rootless_process
> fi
> bundle_expect safety.host_process false
> CASE
PASS S-DOC-019
```
