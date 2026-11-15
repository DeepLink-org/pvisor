# S-DOC-027：E04 VM 资源配置


建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

准备：Linux；可访问 /dev/kvm；准备好 Linux rootfs，并为脚本设置 PVISOR_CASE_ROOTFS。

**语义**：VM 成功运行，内存请求记录为 2147483648 字节。CPU 数量未由本例断言核验。

**理由**：使用已有 rootfs 启动 VM，同时指定 2GiB 内存和 2 个虚拟 CPU。Linux 静态构建已内嵌内核。

**违反示例**：2GiB 的内存请求在产物中被记成其他值。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-027 --timeout 180 <<'CASE'
> require_python3
> require_linux
> require_kvm
> require_rootfs
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --vm \
>   --rootfs "$CASE_ROOTFS" \
>   --memory 2GiB \
>   --cpu 2 \
>   -- "$CASE_TRUE"
> CASE_COMMAND
>
> bundle_expect run.executor.isolation virtual_machine
> bundle_expect resources.requested.memory_bytes 2147483648
> CASE
PASS S-DOC-027
```
