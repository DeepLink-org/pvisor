# S-DOC-013：C02 默认保留 stage


建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：命令成功，日志中给出的存储目录及 Run Bundle 保留，原 workspace 没有新建的文件。

**理由**：无需手写存储路径。`--safe` 在没有指定 `--stage` 时使用持久 Session 存储，退出后保留改动。

**违反示例**：日志中的 Run Bundle 路径在退出后消失，或 result.txt 出现在原工作区。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-013 --timeout 180 <<'CASE'
> require_python3
> require_stage
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --safe -- /bin/sh -c 'printf changed > result.txt'
> CASE_COMMAND
>
> storage=$(dirname "$(grep -m1 '^Run Bundle: ' "$PVISOR_CASE_STDOUT" | cut -d' ' -f3-)")
> test -n "$storage"
> test -f "$storage/run-bundle.json"
> test ! -e result.txt
> CASE
PASS S-DOC-013
```
