# S-STAGE-003：Review 精确表示净效果

**语义**：review 清单精确等于文件净增删改；创建又删除的临时文件不出现在清单中。

**违反示例**：临时文件泄漏进 review，遗漏删除，或只记录被触碰而无净效果的路径。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" stage S-STAGE-003 --timeout 180 <<'CASE'
> require_stage
> printf original > edit; printf gone > deleted
> stage "$CASE_ROOT/stage" 'printf changed > edit; rm deleted; printf added > added; printf temporary > temporary; rm temporary'
> assert_changes "$CASE_ROOT/stage" <<'EXPECTED'
> added added
> deleted deleted
> modified edit
> EXPECTED
> CASE
PASS S-STAGE-003
```
