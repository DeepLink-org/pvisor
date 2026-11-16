# S-STAGE-009：冲突导致所选集合全部无效果

**语义**：所选集合任一路径冲突时，该次 apply 不改变所选集合中任何路径。

**违反示例**：先应用无冲突路径，后发现冲突退出，留下部分落地。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" stage S-STAGE-009 --timeout 180 <<'CASE'
> require_stage
> printf original > a; printf original > z
> stage "$CASE_ROOT/stage" 'printf agent > a; printf agent > z'
> printf external > z
> snapshot external "$WS"
> expect_refused pvisor apply --all "$CASE_ROOT/stage"
> assert_unchanged external "$WS"
> CASE
PASS S-STAGE-009
```
