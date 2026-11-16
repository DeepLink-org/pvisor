# S-DOC-001：A01 省略 `run` 的最简调用


建议场景：适合第一次使用 pVisor、确认命令和 Session 身份。

**语义**：输出当前 workspace 的绝对路径并成功退出。默认使用 host executor，不启用 stage；未配置网络策略时记录为 ambient。

**理由**：在当前目录执行一个命令，不需要显式写出 `run`。`--` 后全部是交给 Agent 的命令和参数。

**违反示例**：命令退出 0，但输出的是父目录，或默认启动了 stage。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-001 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor -- /bin/pwd
> CASE_COMMAND
>
> stdout_has "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
> bundle_expect run.state completed
> bundle_expect run.exit_code 0
> bundle_expect run.agent pwd
> bundle_expect network.policy.mode ambient
> CASE
PASS S-DOC-001
```
