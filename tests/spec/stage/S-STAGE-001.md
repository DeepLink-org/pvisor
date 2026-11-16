# S-STAGE-001：暂存 Session 不改变工作区

**语义**：Session 成功、非零退出或被信号终止后，apply 前的路径、类型、完整内容、权限位和链接目标均与开始前一致。

**违反示例**：删除穿透 lower，或异常清理把 upper 自动合并到工作区。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" stage S-STAGE-001 --timeout 180 <<'CASE'
> require_stage
> printf original > edit; mkdir d; printf keep > d/keep
> snapshot before "$WS"
> expect_exit 0 stage "$CASE_ROOT/success" 'printf changed > edit; rm -rf d; printf new > added'
> assert_unchanged before "$WS"
> expect_exit 3 stage "$CASE_ROOT/nonzero" 'printf changed > edit; rm -rf d; exit 3'
> assert_unchanged before "$WS"
> expect_refused stage "$CASE_ROOT/signal" 'printf changed > edit; rm -rf d; kill -TERM $$'
> assert_unchanged before "$WS"
> CASE
PASS S-STAGE-001
```
