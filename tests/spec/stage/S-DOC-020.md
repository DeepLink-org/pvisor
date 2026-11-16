# S-DOC-020：D03 host stage 隐藏原 workspace


建议场景：适合检查 OverlayFS 视图和 host 安全边界。

准备：Linux user/mount namespace 可用；macOS 不支持此例的 procfs/mount namespace 路径隐藏语义。

**语义**：cwd 指向 stage 的 merged 目录，输出中不出现原 workspace 路径。此例只检查路径显示，不证明所有原路径或继承 FD 访问都已被禁止。

**理由**：观察启用 stage 后子进程的 cwd 和 procfs 路径。三条命令的输出保存到 `views.txt`。

**违反示例**：启用 host stage 后 procfs 的 cwd 仍暴露原工作区路径。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-020 --timeout 180 <<'CASE'
> require_python3
> require_rootless
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --stage "$CASE_ROOT/host-stage" -- /bin/sh -c \
>   'pwd; readlink /proc/self/root; readlink /proc/self/cwd' > views.txt
> CASE_COMMAND
>
> merged="$PVISOR_CASE_ROOT/host-stage/merged"
> test "$(sed -n 1p views.txt)" = "$merged"
> test "$(sed -n 3p views.txt)" = "$merged"
> ! grep -Fq -- "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)" views.txt
> CASE
PASS S-DOC-020
```
