# S-DOC-005：A05 严格执行模式拒绝 best-effort 边界


建议场景：适合第一次使用 pVisor、确认命令和 Session 身份。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：当前 host / container / VM 执行路径在启动 Agent 前均因缺少 Subprocess
  enforcement 证据而拒绝请求（`UnsupportedPolicy`）。此例验证 fail-closed，
  不代表 `--strict` 当前在任一 executor 上可达“更强沙箱已就绪”。

**理由**：要求严格执行能力检查。`--strict` 不接受所请求能力缺少强制执行证据；这里同时要求禁止网络。

**违反示例**：缺少请求能力的强制证据时仍启动 Agent，或拒绝时没有说明缺失的能力。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-005 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run nonzero <<'CASE_COMMAND'
> pvisor --strict --overlaynet-deny-all -- "$CASE_TRUE"
> CASE_COMMAND
>
> stdout_has "lacks enforced evidence for requested capability dimensions"
> CASE
PASS S-DOC-005
```
