# S-STAGE-013：调用返回值与链接效果一致


**语义**：创建链接的调用成功必须产生对应链接，失败则不得产生链接；返回值和可观察效果一致。

**违反示例**：ln 返回 EPERM，却已创建可 apply 的链接。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" stage S-STAGE-013 --xfail-platform macos --xfail-reason 'macFUSE 创建链接返回 EPERM 但已有实际效果；tools/semspec/DESIGN.md §12' --timeout 180 <<'CASE'
> require_stage
> stage "$CASE_ROOT/stage" 'code=0; ln -s /etc/hosts link || code=$?; if test "$code" = 0; then test -L link; test "$(readlink link)" = /etc/hosts; else test ! -e link; test ! -L link; fi'
> assert_absent link
> CASE
(PASS|XFAIL) S-STAGE-013 (regex)
```
