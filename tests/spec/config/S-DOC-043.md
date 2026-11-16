# S-DOC-043：I01 TOML config


建议场景：适合从 TOML/JSON 文件或控制面执行 RunSpec 的任务。

**语义**：命令来自配置文件，无需在 CLI 重复；运行正常结束。

**理由**：把命令写进 TOML 后通过 `--config` 运行。手工执行前创建 `pvisor.toml`，内容为 `[run]` 下的 `command = ["/bin/true"]`；脚本会预置此文件。

**违反示例**：配置文件中的命令被忽略，运行名称或终态不符。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-043 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --config ./pvisor.toml
> CASE_COMMAND
>
> bundle_expect run.state completed
> bundle_expect run.agent true
> CASE
PASS S-DOC-043
```
