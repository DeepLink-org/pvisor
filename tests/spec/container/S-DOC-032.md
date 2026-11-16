# S-DOC-032：F03 使用宿主 rootfs 的 OCI bundle


建议场景：适合由 runc/crun 直接启动 OCI 容器的任务。

准备：Linux、可运行的 OCI runtime，以及允许 rootless container 的 user namespace。

**语义**：以指定目录作为容器根文件系统执行命令，不需要配置镜像仓库。使用专用测试 rootfs，不要把宿主 `/` 当作此例的测试目录。

**理由**：不提供容器镜像，直接以宿主 `/` 作为只读 lower。pVisor 会先建立独立 synthetic rootfs，再把宿主标准目录以只读方式映射进去。

**违反示例**：宿主 rootfs 的 OCI bundle 没有作为 container 正常完成。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-032 --timeout 180 <<'CASE'
> require_python3
> require_container_runtime
> require_linux
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --container-runtime "$CASE_CONTAINER_RUNTIME" --executor container \
>   --rootfs host \
>   --container-network none \
>   -- "$CASE_TRUE"
> CASE_COMMAND
>
> bundle_expect run.executor.kind container
> bundle_expect run.state completed
> CASE
PASS S-DOC-032
```
