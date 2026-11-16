# S-STAGE-006：选择性 Apply 限于所选子树

**语义**：apply --path P 只改变 P 及其后代；覆盖全部变更的两次选择性 apply 等价于一次 --all。

**违反示例**：选择目录时修改兄弟文件，或第一次 apply 消耗未选变更。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" stage S-STAGE-006 --timeout 180 <<'CASE'
> require_stage
> mkdir chosen other; printf a > chosen/a; printf b > other/b
> mkdir "$CASE_ROOT/all"; cp -pR "$WS/." "$CASE_ROOT/all/"
> script='printf changed > chosen/a; printf new > chosen/new; printf changed > other/b'
> stage "$CASE_ROOT/stage" "$script"
> (cd "$CASE_ROOT/all"; stage "$CASE_ROOT/all-stage" "$script")
> snapshot other "$WS/other"
> pvisor apply --path chosen "$CASE_ROOT/stage"
> assert_content chosen/a changed; assert_content chosen/new new
> assert_unchanged other "$WS/other"
> pvisor apply --path other "$CASE_ROOT/stage"
> pvisor apply --all "$CASE_ROOT/all-stage"
> assert_same_tree "$WS" "$CASE_ROOT/all"
> CASE
PASS S-STAGE-006
```
