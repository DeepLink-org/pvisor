# S-DOC-024：E01 `--vm` 简写与 host rootfs


建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

准备：Linux；可访问 /dev/kvm。

**语义**：guest 输出与宿主 workspace 相同的绝对路径。运行结果标记为虚拟机，网络使用 pVisor 的 smoltcp 驱动。

**理由**：用 `--vm` 选择 VM executor；Linux 默认以宿主根目录作为 guest rootfs。该方式扩大了 guest 可读取的宿主文件范围，只应在可信测试环境使用。

**违反示例**：VM 成功退出但 guest cwd 不同，或网络没有记录 vm-smoltcp 边界。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-024 --timeout 180 <<'CASE'
> require_python3
> require_linux
> require_kvm
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --vm -- /bin/pwd > guest-cwd.txt
> CASE_COMMAND
>
> test "$(cat guest-cwd.txt)" = "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
> bundle_expect run.executor.kind virtual_machine
> bundle_expect run.executor.isolation virtual_machine
> bundle_expect network.interception.driver vm-smoltcp
> bundle_expect network.interception.strength non-bypassable
> bundle_expect safety.network_non_bypassable true
> CASE
PASS S-DOC-024
```
