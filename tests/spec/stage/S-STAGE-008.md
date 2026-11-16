# S-STAGE-008：外部变更触发冲突

**语义**：staging 后目标被外部修改、新建或删除时，apply 被拒绝，外部状态完整保留。

**违反示例**：覆盖外部修改，覆盖外部新建文件，或恢复外部删除文件。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" stage S-STAGE-008 --timeout 180 <<'CASE'
> require_stage
> printf original > modified; printf original > removed
> stage "$CASE_ROOT/stage" 'printf agent > modified; printf agent > created; printf agent > removed'
> printf external > modified; printf external > created; rm removed
> snapshot external "$WS"
> for path in modified created removed; do
>   expect_refused pvisor apply --path "$path" "$CASE_ROOT/stage"
>   assert_unchanged external "$WS"
> done
> CASE
PASS S-STAGE-008
```
