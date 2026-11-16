# S-DOC-031：F02 container rootfs 与隔离网络


建议场景：适合由 runc/crun 直接启动 OCI 容器的任务。

准备：OCI runtime 可运行，并为脚本设置 PVISOR_CASE_CONTAINER_IMAGE。

**语义**：容器中的 `/bin/true` 成功退出。此命令不发起网络请求，因此断言只检查启动成功，不验证网络是否能被绕过。

**理由**：在 F01 基础上只增加 `--container-network none`，让容器使用独立的网络 namespace，不配置外部连接。

**违反示例**：隔离网络配置使简单容器命令无法正常启动或完成。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-031 --timeout 180 <<'CASE'
> require_python3
> require_container
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --container-runtime "$CASE_CONTAINER_RUNTIME" --container-image "$CASE_CONTAINER_IMAGE" \
>   --container-network none \
>   -- "$CASE_TRUE"
> CASE_COMMAND
>
> bundle_expect run.executor.kind container
> bundle_expect run.state completed
> CASE
PASS S-DOC-031
```
