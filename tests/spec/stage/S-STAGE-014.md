# S-STAGE-014：Apply 保留可执行位和链接目标

**语义**：可执行位和符号链接目标原样落地，链接不得被解引用为目标文件。

**违反示例**：可执行文件失去 x 位，或链接落地为 /etc/hosts 内容副本。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" stage S-STAGE-014 --timeout 180 <<'CASE'
> require_stage
> stage "$CASE_ROOT/stage" 'printf executable > executable; chmod 755 executable; ln -s executable link'
> pvisor apply --all "$CASE_ROOT/stage"
> assert_content executable executable
> [ -x executable ] || fail 'executable bit lost'
> [ -L link ] && [ "$(readlink link)" = executable ] || fail 'link was dereferenced or changed'
> CASE
PASS S-STAGE-014
```
