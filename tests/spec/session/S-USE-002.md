# S-USE-002：失败也是可查询的 Session

脚本报错时，先查 Session 的退出码，不把文件存在当成成功。

**语义**：子命令退出 7，CLI 保留该退出码，记录为 failed。

**违反示例**：退出码被吞掉或 failed 被记录为 completed。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-USE-002 --timeout 180 <<'CASE'
> journey_setup
> expect_exit 7 pvisor spawn --stdio capture -- /bin/sh -c 'printf failed >&2; exit 7'
> pvisor status last --json > "$CASE_ROOT/status.json"
> json_expect "$CASE_ROOT/status.json" /run/state '"failed"'
> CASE
PASS S-USE-002
```
