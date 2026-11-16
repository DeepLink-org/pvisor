# S-DOC-012：C01 持久 stage


建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：原 workspace 没有 `result.txt`；变更清单中出现该文件，指定 stage 内保留 `run-bundle.json`，便于之后查看。

**理由**：把本次运行的文件改动放进一个保留的 stage。命令在 workspace 里创建 `result.txt`。

**违反示例**：result.txt 穿透到原工作区，或变更清单遗漏它。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-012 --timeout 180 <<'CASE'
> require_python3
> require_stage
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --stage "$CASE_ROOT/stage-keep" -- /bin/sh -c 'printf changed > result.txt'
> CASE_COMMAND
>
> bundle_expect filesystem.state staged
> bundle_contains filesystem.changes result.txt
> bundle_expect safety.filesystem_write_non_bypassable true
> test ! -e result.txt
> test -f "$PVISOR_CASE_ROOT/stage-keep/run-bundle.json"
> CASE
PASS S-DOC-012
```
