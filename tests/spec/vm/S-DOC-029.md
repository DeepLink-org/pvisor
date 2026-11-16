# S-DOC-029：E06 拒绝 executor 冲突


建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

**语义**：参数归一化阶段失败，错误信息明确指出 `--vm` 与非 VM executor 冲突。

**理由**：验证互相冲突的 executor 参数不能一起使用：`--vm` 选择虚拟机，`--executor host` 却选择宿主。

**违反示例**：--vm 与 --executor host 的冲突被忽略并启动了工作负载。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-029 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run nonzero <<'CASE_COMMAND'
> pvisor --vm --executor host --rootfs host -- "$CASE_TRUE"
> CASE_COMMAND
>
> stdout_has "--vm cannot be combined with a non-vm --executor"
> CASE
PASS S-DOC-029
```
