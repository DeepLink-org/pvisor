# S-DOC-059：N03 pause/resume 幂等，使用启动 backing 回收


**语义**：重复 pause 停止 guest 推进；重复 resume 继续执行；沿用启动文件的两次 offload 保持 guest 内存完整，暂停/恢复状态与控制完成事件一致。

**违反示例**：暂停返回成功但 heartbeat 继续变化、恢复不推进，或第二次回收后 guest 内存校验失败。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" vm S-DOC-059 --timeout 180 <<'CASE'
> require_vm_sdk
> vm_case_setup
> vm_run_sdk pause
> grep -Fq 'vm-control-case-ok pause' "$CASE_ROOT/sdk.log"
> test -s "$CASE_ROOT/startup.ram"
> CASE
PASS S-DOC-059
```
