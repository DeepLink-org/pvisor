# S-USE-001：最小运行与直接写入

你已有可信脚本，只需要记录一次执行。`--` 后是原命令，省略 `run` 也可以。

**语义**：命令在当前工作区写入 hello.txt，Session 成功完成且没有 staged changeset。

**违反示例**：命令成功但文件写到别处，或出现隐式 stage。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-USE-001 --timeout 180 <<'CASE'
> journey_setup
> pvisor --name hello --stdio capture -- /bin/sh -c 'printf hello > hello.txt; cat hello.txt'
> assert_content hello.txt hello
> pvisor status last --json > "$CASE_ROOT/status.json"
> json_expect "$CASE_ROOT/status.json" /run/state '"completed"'
> json_expect "$CASE_ROOT/status.json" /run/overlay null
> CASE
PASS S-USE-001
```
