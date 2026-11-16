# S-DOC-002：A02 显式 `run` 与省略形式等价


建议场景：适合第一次使用 pVisor、确认命令和 Session 身份。

**语义**：两个输出文件内容相同，都是当前工作目录。两次运行会各自生成记录，Session ID 和时间可以不同。

**理由**：对比省略和显式写出 `run` 的两种调用。分别保存 Agent 的标准输出，便于比较。

**违反示例**：显式 run 与省略形式的工作目录输出不同。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-002 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor -- /bin/pwd > implicit.txt
> pvisor spawn -- /bin/pwd > explicit.txt
> CASE_COMMAND
>
> diff implicit.txt explicit.txt
> test "$(cat implicit.txt)" = "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
> bundle_expect run.agent pwd
> CASE
PASS S-DOC-002
```
