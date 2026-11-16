# S-DOC-025：E02 显式 VM executor 与目录 rootfs


建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

准备：Linux；可访问 /dev/kvm；准备好 Linux rootfs，并为脚本设置 PVISOR_CASE_ROOTFS。

**语义**：虚拟机成功执行命令，guest cwd 与宿主 workspace 路径一致。

**理由**：已有 Linux rootfs 时，直接把目录交给 VM 使用。目录内需要有可执行的 `/bin/pwd` 及其运行依赖。

**违反示例**：目录 rootfs 的 VM 返回宿主工作区之外的 cwd。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-025 --timeout 180 <<'CASE'
> require_python3
> require_linux
> require_kvm
> require_rootfs
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --executor vm --rootfs "$CASE_ROOTFS" -- /bin/pwd > guest-cwd.txt
> CASE_COMMAND
>
> test "$(cat guest-cwd.txt)" = "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
> bundle_expect run.executor.isolation virtual_machine
> CASE
PASS S-DOC-025
```
