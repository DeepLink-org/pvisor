# S-DOC-033：F04 显式 OCI runtime 与高级 container 参数


建议场景：适合由 runc/crun 直接启动 OCI 容器的任务。

准备：OCI runtime 可运行，并为脚本设置 PVISOR_CASE_CONTAINER_IMAGE。

**语义**：容器成功退出。`read_only=false` 使绑定目录可写，即使 rootfs 只读；`--container-workdir` 在 Run 没有 cwd 时才作为回退。此例仅检查组合启动，未分别检查用户身份和读写行为。

**理由**：显式覆盖高级容器选项：使用 runc 和指定 pVisor 构建，声明平台、uid/gid、工作目录、只读 rootfs，并把宿主目录绑定到 `/workspace`。

**违反示例**：高级 OCI 参数被接受但容器失败，或被错误记为其他 executor。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-033 --timeout 180 <<'CASE'
> require_python3
> require_container
> require_runc
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --executor container \
>   --container-runtime runc \
>   --rootfs "image=$CASE_CONTAINER_IMAGE" \
>   --container-pvisor-binary "$SUBJECT_BIN" \
>   --container-platform linux/amd64 \
>   --container-network none \
>   --container-workdir /workspace \
>   --container-user 1000:1000 \
>   --container-read-only-rootfs \
>   --container-mount "source=\"$WS\",target=\"/workspace\",read_only=false" \
>   -- "$CASE_TRUE"
> CASE_COMMAND
>
> bundle_expect run.executor.kind container
> bundle_expect run.state completed
> CASE
PASS S-DOC-033
```
