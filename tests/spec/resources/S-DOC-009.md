# S-DOC-009：B02 文件大小限制实际生效


建议场景：适合需要控制或验证资源限制的任务。

**语义**：写入命令失败，落盘文件如果存在，其大小不超过 1024 字节；运行结果记录进程退出失败。

**理由**：验证单文件大小限制：将上限设为 1KiB，再尝试用 `dd` 写入 4KiB。

**违反示例**：dd 成功写出 4KiB，或失败后文件仍超过 1024 字节。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-009 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run nonzero <<'CASE_COMMAND'
> pvisor --max-file-size 1KiB -- /bin/sh -c 'dd if=/dev/zero of=large bs=4096 count=1'
> CASE_COMMAND
>
> bundle_expect resources.requested.file_size_bytes 1024
> bundle_expect run.state failed
> bundle_expect run.failure.kind process_exit
> test ! -f large || [ "$(wc -c < large)" -le 1024 ]
> CASE
PASS S-DOC-009
```
