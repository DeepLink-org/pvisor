# S-DOC-048：J03 Container + stage + read-only root + no network


建议场景：适合上线前验证多项能力组合的端到端任务。

准备：OCI runtime 可运行，并为脚本设置 PVISOR_CASE_CONTAINER_IMAGE。

**语义**：容器成功退出并留下 stage。命令为 `/bin/true`，不会生成文件变更或有意义的 stdout；本例不验证 stage 写入和网络阻断行为。

**理由**：在容器中组合持久 stage、只读 rootfs、隔离网络和 stdout 采集。只读 rootfs 与可写 stage 是不同层面的设置。

**违反示例**：组合 container 没有正常结束并保留 stage。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-048 --timeout 180 <<'CASE'
> require_python3
> require_container
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --container-runtime "$CASE_CONTAINER_RUNTIME" --name container-full \
>   --container-image "$CASE_CONTAINER_IMAGE" \
>   --container-read-only-rootfs \
>   --container-network none \
>   --stage "$CASE_ROOT/container-full" \
>   --stdio capture \
>   -- "$CASE_TRUE"
> CASE_COMMAND
>
> bundle_expect run.agent container-full
> bundle_expect run.executor.kind container
> bundle_expect run.state completed
> bundle_expect filesystem.state staged
> CASE
PASS S-DOC-048
```
