# S-DOC-038：G05 VM OverlayNet auto


建议场景：适合配置出站网络、代理访问或禁止网络的任务。

准备：Linux；可访问 /dev/kvm；准备好 Linux rootfs，并为脚本设置 PVISOR_CASE_ROOTFS。

**语义**：记录为 vm-smoltcp 和 non-bypassable，而不是 host 的协作式代理。

**理由**：验证 VM 默认使用 OverlayNet auto，使流量经过虚拟机的 smoltcp 网络驱动。

**违反示例**：VM 网络被记录为 cooperative proxy 而非 vm-smoltcp 强制边界。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-038 --timeout 180 <<'CASE'
> require_python3
> require_linux
> require_kvm
> require_rootfs
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --vm --rootfs "$CASE_ROOTFS" -- "$CASE_TRUE"
> CASE_COMMAND
>
> bundle_expect network.interception.driver vm-smoltcp
> bundle_expect network.interception.strength non-bypassable
> CASE
PASS S-DOC-038
```
