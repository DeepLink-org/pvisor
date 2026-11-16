# S-DOC-015：C04 显式 stage 保留已有目录内容


建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

**语义**：命令成功，原有的 `user-file` 和新生成的 Run Bundle 都保存在指定目录。

**理由**：验证 `--stage PATH` 始终表示持久目录；目录里已有的用户文件不会因运行结束而被删除。

**违反示例**：运行清理删除了指定 stage 目录中原有的 user-file。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-015 --timeout 180 <<'CASE'
> require_python3
> require_stage
> case_setup
> case_run success <<'CASE_COMMAND'
> mkdir -p "$CASE_ROOT/existing-stage"
> touch "$CASE_ROOT/existing-stage/user-file"
> pvisor --stage "$CASE_ROOT/existing-stage" -- "$CASE_TRUE"
> CASE_COMMAND
>
> test -f "$PVISOR_CASE_ROOT/existing-stage/user-file"
> test -f "$PVISOR_CASE_ROOT/existing-stage/run-bundle.json"
> CASE
PASS S-DOC-015
```
