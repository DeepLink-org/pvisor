# S-STAGE-004：Apply all 等价于直接运行

**语义**：对同一初始工作区执行同一成功脚本，stage 后 apply --all 与直接执行所得完整树相同。

**违反示例**：内容相同但权限或链接丢失，或者 rename、目录删除结果不同。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" stage S-STAGE-004 --timeout 180 <<'CASE'
> require_stage
> printf original > edit; mkdir d; printf gone > d/file; printf moved > old
> mkdir "$CASE_ROOT/direct"; cp -pR "$WS/." "$CASE_ROOT/direct/"
> script='printf changed > edit; rm -rf d; mv old renamed; mkdir newdir; printf new > newdir/file; chmod 700 renamed'
> (cd "$CASE_ROOT/direct"; /bin/sh -eu -c "$script")
> stage "$CASE_ROOT/stage" "$script"
> pvisor apply --all "$CASE_ROOT/stage"
> assert_same_tree "$WS" "$CASE_ROOT/direct"
> CASE
PASS S-STAGE-004
```
