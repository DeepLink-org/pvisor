# S-DOC-061：N05 offload 拒绝覆盖，VM 继续运行


**语义**：offload 选定已有文件时拒绝操作，已有内容、Running 状态和未取消状态保持；guest 继续推进，之后 pause/resume 仍可完成。

**违反示例**：拒绝路径后截断目标、暂停或取消了 VM，或使后续命令的确认串线。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" vm S-DOC-061 --timeout 180 <<'CASE'
> require_vm_sdk
> vm_case_setup
> vm_run_sdk reject
> grep -Fq 'vm-control-case-ok reject' "$CASE_ROOT/sdk.log"
> assert_content "$CASE_ROOT/occupied.ram" keep
> CASE
PASS S-DOC-061
```
