# S-DOC-044：I02 JSON RunSpec


建议场景：适合从 TOML/JSON 文件或控制面执行 RunSpec 的任务。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：运行名称为 `case-i02`，`run-result.json` 非空。该委托路径当前只支持 host executor，不套用普通 Session 的 rootless safe profile；不要把此例视为隔离模式示例。

**理由**：执行已准备好的 JSON RunSpec，并把结果原子写入指定文件。手工运行前准备包含 run_id、agent 和 process invocation 的 `run-spec.json`；脚本预置的是运行 `/bin/true` 的 `case-i02`。

**违反示例**：委托运行没有生成结果文件，或被误记成隔离的普通 Session。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-044 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --spec ./run-spec.json --result-file ./run-result.json --stage ./delegated-stage
> CASE_COMMAND
>
> bundle_expect run.agent case-i02
> bundle_expect run.executor.isolation host_process
> test -s run-result.json
> CASE
PASS S-DOC-044
```
