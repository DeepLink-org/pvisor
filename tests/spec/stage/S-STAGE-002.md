# S-STAGE-002：Session 读取自己的变更

**语义**：Session 内读取看到自己的写入、删除和重命名；apply 前宿主工作区不变。

**违反示例**：写完读取旧内容，删除后仍可见，或重命名后找不到新路径。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" stage S-STAGE-002 --timeout 180 <<'CASE'
> require_stage
> printf original > edit; printf gone > deleted; printf moved > old
> snapshot before "$WS"
> stage "$CASE_ROOT/stage" 'printf changed > edit; test "$(cat edit)" = changed; rm deleted; test ! -e deleted; mv old new; test ! -e old; test "$(cat new)" = moved'
> assert_unchanged before "$WS"
> CASE
PASS S-STAGE-002
```
