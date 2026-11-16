# S-STAGE-007：重复 Apply 无额外效果

**语义**：成功 apply 的同一路径再次 apply 不改变工作区；是否报告 already applied 不影响该性质。

**违反示例**：重复删除或重复复制引入新修改。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" stage S-STAGE-007 --timeout 180 <<'CASE'
> require_stage
> printf original > edit
> stage "$CASE_ROOT/stage" 'printf changed > edit'
> pvisor apply --path edit "$CASE_ROOT/stage"
> snapshot applied "$WS"
> # 重复请求可以成功或明确拒绝，但必须无效果。
> pvisor apply --path edit "$CASE_ROOT/stage" || :
> assert_unchanged applied "$WS"
> CASE
PASS S-STAGE-007
```
