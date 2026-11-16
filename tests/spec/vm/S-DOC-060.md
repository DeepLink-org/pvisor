# S-DOC-060：N04 offload 发布新文件并保持 live inode


**语义**：offload 选定同一文件系统中的新路径，文件与启动 backing 属于同一 inode；结果路径和 RAM 范围正确，恢复后 guest 内存完整，重复 offload 保持文件选择。

**违反示例**：复制到了另一个 inode、回报错误的路径、回收后 guest 内存损坏，或下次 offload 悄悄换回原文件。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" vm S-DOC-060 --timeout 180 <<'CASE'
> require_vm_sdk
> vm_case_setup
> vm_run_sdk offload
> grep -Fq 'vm-control-case-ok offload' "$CASE_ROOT/sdk.log"
> python3 - "$CASE_ROOT/startup.ram" "$CASE_ROOT/offloaded.ram" <<'PY'
> import os, stat, sys
> assert os.path.samefile(sys.argv[1], sys.argv[2])
> assert stat.S_IMODE(os.stat(sys.argv[2]).st_mode) == 0o600
> PY
> CASE
PASS S-DOC-060
```
