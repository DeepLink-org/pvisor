# S-DOC-045：I03 无扩展名 spec


建议场景：适合从 TOML/JSON 文件或控制面执行 RunSpec 的任务。

**语义**：`--config` 正常读取 TOML 并完成运行，不要求文件名以 `.toml` 结尾。

**理由**：验证配置的识别不依赖扩展名。手工执行时把 I01 的 TOML 内容保存成 `config-without-extension`；脚本会预置该文件。

**违反示例**：相同 TOML 因文件没有扩展名而不能执行。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-045 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --config ./config-without-extension
> CASE_COMMAND
>
> bundle_expect run.state completed
> CASE
PASS S-DOC-045
```
