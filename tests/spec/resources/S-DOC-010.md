# S-DOC-010：B03 内存参数短别名


建议场景：适合需要控制或验证资源限制的任务。

**语义**：命令成功，记录中的请求值为 268435456 字节，与 `--memory 256MiB` 一致。

**理由**：使用 `--memory` 的别名 `--mem`，为一个简单命令设置 256MiB 内存额度。

**违反示例**：--mem 256MiB 被解析成不同于 --memory 的请求值。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-010 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --mem 256MiB -- "$CASE_TRUE"
> CASE_COMMAND
>
> bundle_expect resources.requested.memory_bytes 268435456
> CASE
PASS S-DOC-010
```
