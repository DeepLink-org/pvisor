# S-DOC-004：A04 超时


建议场景：适合第一次使用 pVisor、确认命令和 Session 身份。

**语义**：pVisor 非零退出，运行结果的失败类型为 `deadline_exceeded`。

**理由**：给运行设置墙钟超时。`100ms` 是从运行开始计时的持续时间，不是 CPU 时间；命令故意睡眠 10 秒。

**违反示例**：超时命令仍成功退出，或失败被记成可重试的普通 process_exit。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-004 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run nonzero <<'CASE_COMMAND'
> pvisor --timeout 100ms -- /bin/sleep 10
> CASE_COMMAND
>
> bundle_expect run.state failed
> bundle_expect run.failure.kind deadline_exceeded
> bundle_expect run.failure.retryable false
> CASE
PASS S-DOC-004
```
