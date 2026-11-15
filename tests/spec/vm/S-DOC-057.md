# S-DOC-057：N01 启动指定 RAM 文件，退出后保留


**语义**：指定新的 RAM 文件后，guest 成功执行；文件权限为 0600，包含非空 RAM backing，正常退出后仍存在。

**违反示例**：文件未被创建、权限向其他用户开放，或正常退出删除了调用者指定的文件。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" vm S-DOC-057 --timeout 180 <<'CASE'
> require_vm_case
> vm_case_setup
> pvisor spawn --no-agent-defaults --gateway-mode off --rootfs "$PVISOR_CASE_ROOTFS" \
>   --vm-ram-backing "$CASE_ROOT/startup.ram" --stdio capture -- /bin/sh -c 'printf ram-startup-ok'
> bundle_expect run.state completed
> bundle_expect run.exit_code 0
> bundle_contains run.output.stdout ram-startup-ok
> python3 - "$CASE_ROOT/startup.ram" <<'PY'
> import pathlib, stat, sys
> p = pathlib.Path(sys.argv[1])
> assert p.is_file() and p.stat().st_size > 0
> assert stat.S_IMODE(p.stat().st_mode) == 0o600
> PY
> CASE
PASS S-DOC-057
```
