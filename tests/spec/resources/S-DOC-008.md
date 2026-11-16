# S-DOC-008：B01 组合使用所有资源限制


建议场景：适合需要控制或验证资源限制的任务。

**语义**：命令成功退出，五个请求值出现在运行记录中，同时报告生效值和限制机制。`/bin/true` 不消耗这些额度，此例不测试超限行为。

**理由**：组合设置内存、进程数、CPU 时间、打开文件数和单文件大小。`MiB` 是二进制单位；`--max-cpu-time` 与墙钟超时不同。

**违反示例**：限制参数被接受，但请求值记录错误，或缺少文件大小的生效值和 rlimit 机制。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-008 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor \
>   --memory 256MiB \
>   --max-processes 32 \
>   --max-cpu-time 5s \
>   --max-open-files 128 \
>   --max-file-size 1MiB \
>   -- "$CASE_TRUE"
> CASE_COMMAND
>
> bundle_expect resources.requested.memory_bytes 268435456
> bundle_expect resources.requested.processes 32
> bundle_expect resources.requested.cpu_time_ms 5000
> bundle_expect resources.requested.open_files 128
> bundle_expect resources.requested.file_size_bytes 1048576
> bundle_expect resources.effective.file_size_bytes 1048576
> bundle_contains resources.mechanisms rlimit
> CASE
PASS S-DOC-008
```
