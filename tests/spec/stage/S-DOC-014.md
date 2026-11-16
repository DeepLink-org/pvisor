# S-DOC-014：C03 显式丢弃持久 stage 的改动


建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：stage 目录保留，运行记录的文件系统状态变为 `discarded`；原 workspace 没有新文件。

**理由**：指定持久 stage 路径，完成运行后通过 `pvisor drop` 显式丢弃其中的改动。

**违反示例**：drop 后记录仍标为 staged，或原工作区得到 result.txt。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-014 --timeout 180 <<'CASE'
> require_python3
> require_stage
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --stage "$CASE_ROOT/stage-drop" -- /bin/sh -c 'printf changed > result.txt'
> pvisor drop "$CASE_ROOT/stage-drop"
> CASE_COMMAND
>
> record_expect overlay.state discarded "$PVISOR_CASE_ROOT/stage-drop"
> test ! -e result.txt
> test -f "$PVISOR_CASE_ROOT/stage-drop/run-bundle.json"
> CASE
PASS S-DOC-014
```
