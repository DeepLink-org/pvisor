# S-DOC-011：B04 Stage 总大小限制


建议场景：适合需要控制或验证资源限制的任务。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：stage 成功建立并保存在指定路径。此例只验证参数可用和目录建立；当前产物未记录该上限，也未在此例中尝试写满 stage。

**理由**：为持久 stage 请求 1GiB 的总大小限制。它限制的是 stage 总量，和 B02 的单个文件大小不是同一个概念。

**违反示例**：stage 命令成功退出，但文件系统状态未标为 staged，或存储路径不符。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-011 --timeout 180 <<'CASE'
> require_python3
> require_stage
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --stage "$CASE_ROOT/limited-stage" --overlayfs-max-size 1GiB -- "$CASE_TRUE"
> CASE_COMMAND
>
> bundle_expect filesystem.state staged
> bundle_expect safety.filesystem_changes_staged true
> record_expect storage "$(realpath "$PVISOR_CASE_ROOT/limited-stage")"
> CASE
PASS S-DOC-011
```
