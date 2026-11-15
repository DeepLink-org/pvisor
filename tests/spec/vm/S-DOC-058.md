# S-DOC-058：N02 已有 RAM 文件不能被启动覆盖


**语义**：启动时指定已有文件必须失败，文件内容保持，guest 命令不执行。

**违反示例**：截断已有文件以建立 RAM backing，或者报错后仍执行 guest。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" vm S-DOC-058 --timeout 180 <<'CASE'
> require_vm_case
> vm_case_setup
> printf 'keep-existing-ram' > "$CASE_ROOT/startup.ram"
> expect_refused pvisor spawn --no-agent-defaults --gateway-mode off --rootfs "$PVISOR_CASE_ROOTFS" \
>   --vm-ram-backing "$CASE_ROOT/startup.ram" --stdio capture -- /bin/sh -c 'printf unexpected-guest'
> assert_content "$CASE_ROOT/startup.ram" 'keep-existing-ram'
> bundle_expect run.state failed
> python3 - "$PVISOR_CASE_RECORDS" <<'PY'
> import json, pathlib, sys
> bundles = list(pathlib.Path(sys.argv[1]).rglob('run-bundle.json'))
> assert bundles
> bundle = json.loads(max(bundles, key=lambda p: p.stat().st_mtime).read_text())
> assert bundle['run']['state'] == 'failed'
> assert bundle['run']['output'].get('stdout') is None
> PY
> CASE
PASS S-DOC-058
```
