# S-DOC-026：E03 image rootfs


建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

准备：Linux；可访问 /dev/kvm；为脚本设置 PVISOR_CASE_IMAGE。

**语义**：镜像准备后启动 VM，guest 的工作目录与宿主 workspace 路径一致。

**理由**：使用 OCI 镜像准备 VM 的 rootfs，不依赖 Docker/Podman daemon。将 `image=` 后的占位符替换为可获取的镜像引用。

**违反示例**：镜像 VM 返回错误 cwd，或运行结果不标为 virtual_machine。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-026 --timeout 180 <<'CASE'
> require_python3
> require_linux
> require_kvm
> require_image
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --vm --rootfs "image=$CASE_IMAGE" -- /bin/pwd > guest-cwd.txt
> CASE_COMMAND
>
> test "$(cat guest-cwd.txt)" = "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
> bundle_expect run.executor.isolation virtual_machine
> CASE
PASS S-DOC-026
```
