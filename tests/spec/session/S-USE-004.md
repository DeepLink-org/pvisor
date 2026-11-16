# S-USE-004：把固定参数放进配置

任务稳定后，用显式 `--config` 保存命令和名称；本次 CLI 参数仍可覆盖它。

**语义**：从 TOML 启动的命令被本次 CLI 命令替换，名称被覆盖，捕获输出为 override。

**违反示例**：偷偷读取隐式配置或忽略 CLI 覆盖。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-USE-004 --timeout 180 <<'CASE'
> journey_setup
> cat > task.toml <<'TOML'
> [run]
> agent = "configured"
> command = ["/bin/sh", "-c", "printf configured"]
> TOML
> pvisor spawn --config task.toml --name explicit --stdio capture -- /bin/sh -c 'printf override'
> journey_bundle > "$CASE_ROOT/bundle.json"
> json_expect "$CASE_ROOT/bundle.json" /run/agent '"explicit"'
> json_expect "$CASE_ROOT/bundle.json" /run/output/stdout '"override"'
> CASE
PASS S-USE-004
```
