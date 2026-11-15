# S-DOC-028：E05 VM workspace 与 whole-rootfs stage 组合


建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

准备：Linux；可访问 /dev/kvm；为脚本设置 PVISOR_CASE_IMAGE。

**语义**：guest cwd 保持一致，stage 保留在 `vm-stage`。本例只执行 `pwd`，验证路径和 stage 建立，不验证写入捕获。

**理由**：在 VM 镜像运行基础上增加持久 stage。工作区路径默认保持与宿主 cwd 一致。

**违反示例**：VM stage 路径或 guest cwd 与请求不一致。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-028 --timeout 180 <<'CASE'
> require_python3
> require_linux
> require_kvm
> require_image
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --vm \
>   --rootfs "image=$CASE_IMAGE" \
>   --stage "$CASE_ROOT/vm-stage" \
>   -- /bin/pwd > guest-cwd.txt
> CASE_COMMAND
>
> test "$(cat guest-cwd.txt)" = "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
> bundle_expect filesystem.state staged
> record_expect storage "$PVISOR_CASE_ROOT/vm-stage"
> CASE
PASS S-DOC-028
```
