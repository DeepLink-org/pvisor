# S-DOC-016：C05 whole-rootfs 捕获与 tmpfs 隔离


建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

准备：Linux user/mount namespace 可用；macOS Seatbelt 不提供此例要求的 whole-rootfs/tmpfs 隔离。

**语义**：workspace 的改动出现在 stage，宿主 workspace 和宿主 `/tmp` 均不出现新文件。这里不验证 workspace 以外普通 rootfs 路径的持久化。

**理由**：比较 workspace 写入和 sandbox 临时目录写入。前者用于保留任务改动，后者只供本次运行临时使用。

**违反示例**：工作区写入未被暂存，或 sandbox 的 /tmp 写入出现在宿主 /tmp。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-016 --timeout 180 <<'CASE'
> require_python3
> require_rootless
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --stage "$CASE_ROOT/root-stage" -- /bin/sh -c \
>   'printf workspace > ./workspace-change; printf tmp > "$1"' sh "$CASE_TMP_PATH"
> CASE_COMMAND
>
> bundle_contains filesystem.changes workspace-change
> test ! -e workspace-change
> test ! -e "$CASE_TMP_PATH"
> CASE
PASS S-DOC-016
```
