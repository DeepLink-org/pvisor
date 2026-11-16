# S-DOC-039：G06 关闭 OverlayNet 时拒绝策略参数


建议场景：适合配置出站网络、代理访问或禁止网络的任务。

**语义**：启动前失败，错误提示策略需要 `auto` 或 `proxy`。如果只想关闭 OverlayNet，请不要附带 allow/deny/limit。

**理由**：检查关闭 OverlayNet 后不能继续提供网络策略。

**违反示例**：关闭 OverlayNet 后仍静默接受 allow 策略。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-039 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run nonzero <<'CASE_COMMAND'
> pvisor --overlaynet off --overlaynet-allow example.com:443 -- "$CASE_TRUE"
> CASE_COMMAND
>
> stdout_has "OverlayNet policy options require --overlaynet auto or proxy"
> CASE
PASS S-DOC-039
```
