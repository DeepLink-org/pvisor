# S-DOC-003：A03 Session 名称和 stdio capture


建议场景：适合第一次使用 pVisor、确认命令和 Session 身份。

**语义**：Session 名称为 `smoke`，结果中的标准输出为 `hello`，未被截断。

**理由**：为这次运行命名，并将 Agent 输出保存到运行结果。`--name smoke` 指定显示名，`--stdio capture` 开启输出采集。

**违反示例**：Agent 名称丢失，或捕获的 hello 被标记为截断。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-003 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --name smoke --stdio capture -- /bin/sh -c 'printf hello'
> CASE_COMMAND
>
> bundle_expect run.agent smoke
> bundle_expect run.output.stdout hello
> bundle_expect run.output.stdout_truncated false
> CASE
PASS S-DOC-003
```
