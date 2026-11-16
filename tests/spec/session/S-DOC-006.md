# S-DOC-006：A06 显式环境投影


建议场景：适合第一次使用 pVisor、确认命令和 Session 身份。

**语义**：子进程可见 `TEST_PVISOR_VALUE=visible`；运行记录列出这个变量，但不声明整体继承宿主环境。

**理由**：只把指定的宿主环境变量传给子进程。变量仅为这条命令设置，通过 `--pass-env` 显式允许投影。

**违反示例**：显式允许的变量未传给 Agent，或记录错误声明继承了整个宿主环境。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-006 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run success <<'CASE_COMMAND'
> TEST_PVISOR_VALUE=visible pvisor --pass-env TEST_PVISOR_VALUE -- /usr/bin/env
> CASE_COMMAND
>
> stdout_has "TEST_PVISOR_VALUE=visible"
> bundle_contains environment.projected_keys TEST_PVISOR_VALUE
> bundle_expect environment.inherits_host false
> CASE
PASS S-DOC-006
```
