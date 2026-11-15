# S-STAGE-010：未触碰路径的外部变更不阻止 Apply

**语义**：Agent 未触碰路径的外部修改、新增和删除不影响 apply，并全部保留。

**违反示例**：整树冲突检查误拒绝，或 apply 恢复无关外部删除。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" stage S-STAGE-010 --timeout 180 <<'CASE'
> require_stage
> printf original > touched; mkdir other; printf base > other/edit; printf base > other/deleted
> stage "$CASE_ROOT/stage" 'printf agent > touched'
> printf external > other/edit; printf external > other/new; rm other/deleted
> snapshot external "$WS/other"
> pvisor apply --all "$CASE_ROOT/stage"
> assert_content touched agent
> assert_unchanged external "$WS/other"
> CASE
PASS S-STAGE-010
```
