# S-STAGE-012：Apply 不越出工作区

**语义**：staging 后目录被替换为指向外部的链接时，apply 不写外部目录；本例要求冲突拒绝且两棵树不变。

**违反示例**：沿替换的链接把 staged 内容写入工作区之外。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" stage S-STAGE-012 --timeout 180 <<'CASE'
> require_stage
> mkdir d; printf base > d/file
> stage "$CASE_ROOT/stage" 'printf agent > d/file'
> mkdir "$CASE_ROOT/outside"; printf external > "$CASE_ROOT/outside/file"
> rm -rf d; ln -s "$CASE_ROOT/outside" d
> snapshot workspace "$WS"; snapshot outside "$CASE_ROOT/outside"
> expect_refused pvisor apply --all "$CASE_ROOT/stage"
> assert_unchanged workspace "$WS"; assert_unchanged outside "$CASE_ROOT/outside"
> CASE
PASS S-STAGE-012
```
