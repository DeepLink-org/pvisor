# S-DOC-030：F01 最小 container Session


建议场景：适合由 runc/crun 直接启动 OCI 容器的任务。

准备：OCI runtime 可运行，并为脚本设置 PVISOR_CASE_CONTAINER_IMAGE。

**语义**：pVisor 准备 OCI bundle、注入自身并执行 `/bin/true`，运行结果记录为 container 且退出码为 0。

**理由**：以 OCI 镜像启动最小容器运行。`--container-image` 同时选择容器 executor 和镜像来源。

**违反示例**：最小容器命令退出非零，或产物把 container 记为其他 executor。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-030 --timeout 180 <<'CASE'
> require_python3
> require_container
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --container-runtime "$CASE_CONTAINER_RUNTIME" --container-image "$CASE_CONTAINER_IMAGE" -- "$CASE_TRUE"
> CASE_COMMAND
>
> bundle_expect run.executor.kind container
> bundle_expect run.state completed
> bundle_expect run.exit_code 0
> CASE
PASS S-DOC-030
```
