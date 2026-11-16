# S-STAGE-011：删除目录不吞掉外部新增

**语义**：Agent 删除目录后，外部向目录新增文件，apply 必须拒绝并保留整个外部树。

**违反示例**：仅核对目录旧文件，递归删除新文件。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" stage S-STAGE-011 --timeout 180 <<'CASE'
> require_stage
> mkdir d; printf base > d/base
> stage "$CASE_ROOT/stage" 'rm -rf d'
> printf external > d/new
> snapshot external "$WS"
> expect_refused pvisor apply --all "$CASE_ROOT/stage"
> assert_unchanged external "$WS"
> CASE
PASS S-STAGE-011
```
