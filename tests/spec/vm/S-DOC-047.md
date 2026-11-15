# S-DOC-047：J02 VM + image rootfs + stage + OverlayNet + Gateway


建议场景：适合上线前验证多项能力组合的端到端任务。

准备：Linux；可访问 /dev/kvm；为脚本设置 PVISOR_CASE_IMAGE；PVISOR_CASE_AGENT 指向 guest 中也可执行的 Agent。

**语义**：VM 以请求的内存运行，保留 stage 和轨迹目录。是否产生模型对话取决于 Agent 是否真的调用 Gateway；当前断言不检查对话内容。

**理由**：在 VM 中运行真实 Agent，同时保留 stage、使用 smoltcp 网络、Gateway 和 JSONL 轨迹记录。需提供含 Agent 及其依赖的镜像，并把示例上游替换为实际服务。

**违反示例**：组合 VM 没有保留 stage/trajectory，或丢失 4GiB 内存请求。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-047 --timeout 180 <<'CASE'
> require_python3
> require_linux
> require_kvm
> require_image
> require_agent
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --name vm-full \
>   --vm \
>   --rootfs "image=$CASE_IMAGE" \
>   --stage "$CASE_ROOT/vm-full" \
>   --overlaynet auto \
>   --gateway-mode capture \
>   --gateway-level dialogue \
>   --gateway-route 'name="default",upstream="https://example.com/v1"' \
>   --record-destination "$CASE_ROOT/vm-full/trajectory" \
>   --memory 4GiB \
>   --cpu 4 \
>   -- "$CASE_AGENT"
> CASE_COMMAND
>
> bundle_expect run.agent vm-full
> bundle_expect run.executor.isolation virtual_machine
> bundle_expect network.interception.driver vm-smoltcp
> bundle_expect filesystem.state staged
> bundle_expect resources.requested.memory_bytes 4294967296
> test -d "$PVISOR_CASE_ROOT/vm-full/trajectory"
> CASE
PASS S-DOC-047
```
