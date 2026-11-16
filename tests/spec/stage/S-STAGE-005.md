# S-STAGE-005：Drop 保留工作区并拒绝后续 apply

**语义**：drop 后工作区不变；该 stage 后续 apply 必须拒绝且无效果。

**违反示例**：drop 把变更落地，或丢弃后还能 apply。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" stage S-STAGE-005 --timeout 180 <<'CASE'
> require_stage
> printf original > edit
> snapshot before "$WS"
> stage "$CASE_ROOT/stage" 'printf changed > edit; printf new > added'
> pvisor drop "$CASE_ROOT/stage"
> assert_unchanged before "$WS"
> expect_refused pvisor apply --all "$CASE_ROOT/stage"
> assert_unchanged before "$WS"
> CASE
PASS S-STAGE-005
```
