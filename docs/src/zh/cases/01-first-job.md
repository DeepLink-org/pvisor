# 1. 从一个命令到一个 Job

先用熟悉的 shell 命令理解 Job。普通 host 运行直接写入当前工作区，网络默认 ambient；想审查后再接受改动，请继续第二章。每条场景独立执行，不依赖上一条生成的 Job。

### S-USE-001：最小运行与直接写入

你已有可信脚本，只需要记录一次执行。`--` 后是原命令，省略 `run` 也可以。

**语义**：命令在当前工作区写入 hello.txt，Job 成功完成且没有 staged changeset。

**违反示例**：命令成功但文件写到别处，或出现隐式 stage。

<!-- semspec: case id=S-USE-001 -->
```bash
journey_setup
pvisor --name hello --stdio capture -- /bin/sh -c 'printf hello > hello.txt; cat hello.txt'
assert_content hello.txt hello
pvisor status last --json > "$CASE_ROOT/status.json"
json_expect "$CASE_ROOT/status.json" /run/state '"completed"'
json_expect "$CASE_ROOT/status.json" /run/overlay null
```

### S-USE-002：失败也是可查询的 Job

脚本报错时，先查 Job 的退出码，不把文件存在当成成功。

**语义**：子命令退出 7，CLI 保留该退出码，记录为 failed。

**违反示例**：退出码被吞掉或 failed 被记录为 completed。

<!-- semspec: case id=S-USE-002 -->
```bash
journey_setup
expect_exit 7 pvisor run --stdio capture -- /bin/sh -c 'printf failed >&2; exit 7'
pvisor status last --json > "$CASE_ROOT/status.json"
json_expect "$CASE_ROOT/status.json" /run/state '"failed"'
```

### S-USE-003：给长任务设置期限

无人值守任务使用 `--timeout`，避免脚本无限等待。

**语义**：休眠任务被期限中止，Run Bundle 报告 deadline_exceeded。

**违反示例**：任务睡到结束或被记成普通成功。

<!-- semspec: case id=S-USE-003 -->
```bash
journey_setup
expect_refused pvisor run --timeout 100ms --stdio capture -- /bin/sleep 30
journey_bundle > "$CASE_ROOT/bundle.json"
json_expect "$CASE_ROOT/bundle.json" /run/failure/kind '"deadline_exceeded"'
```

### S-USE-004：把固定参数放进配置

任务稳定后，用显式 `--config` 保存命令和名称；本次 CLI 参数仍可覆盖它。

**语义**：从 TOML 启动的命令被本次 CLI 命令替换，名称被覆盖，捕获输出为 override。

**违反示例**：偷偷读取隐式配置或忽略 CLI 覆盖。

<!-- semspec: case id=S-USE-004 -->
```bash
journey_setup
cat > task.toml <<'TOML'
[run]
agent = "configured"
command = ["/bin/sh", "-c", "printf configured"]
TOML
pvisor run --config task.toml --name explicit --stdio capture -- /bin/sh -c 'printf override'
journey_bundle > "$CASE_ROOT/bundle.json"
json_expect "$CASE_ROOT/bundle.json" /run/agent '"explicit"'
json_expect "$CASE_ROOT/bundle.json" /run/output/stdout '"override"'
```

[返回学习路线](index.md) · [2. 先审查，再接受文件改动](02-review-and-decide.md)
