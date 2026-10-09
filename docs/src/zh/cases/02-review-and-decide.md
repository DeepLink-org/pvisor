# 2. 先审查，再接受文件改动

Agent 会修改代码时，先加 `--stage`，再用 `review` 和 `inspect` 看结果，最后 `apply` 或 `drop`。stage 只暂存工作区改动，host 仍可访问其他路径；需要访问边界时使用第四章的 `--safe`。

### S-USE-005：查看 Agent 看到的文件

准备让 Agent 修改一份报告，先检查暂存内容与原件的区别。`inspect` 提供只读视图。

**语义**：原件保持 original，inspect 读到 proposed，review 列出报告改动。

**违反示例**：写入穿透原件或 review 漏报改动。

<!-- semspec: case id=S-USE-005 -->
```bash
journey_setup
printf original > report.txt
pvisor run --stage "$CASE_ROOT/draft" -- /bin/sh -c 'printf proposed > report.txt'
assert_content report.txt original
pvisor inspect "$CASE_ROOT/draft" -- /bin/cat report.txt > "$CASE_ROOT/view.txt"
assert_content "$CASE_ROOT/view.txt" proposed
pvisor review "$CASE_ROOT/draft" --diff > "$CASE_ROOT/review.txt"
journey_contains "$CASE_ROOT/review.txt" report.txt
pvisor review "$CASE_ROOT/draft" --json > "$CASE_ROOT/review.json"
json_paths "$CASE_ROOT/review.json" report.txt
```

### S-USE-006：只接受需要的文件

Agent 同时修改正文和实验文件时，可以只接受正文，再丢弃剩余改动。

**语义**：apply --path 只提交 report.txt，剩余 scratch.txt 被 drop 丢弃，Job 记录仍可查询。

**违反示例**：选择性 apply 提交所有文件，或 drop 删除记录。

<!-- semspec: case id=S-USE-006 -->
```bash
journey_setup
pvisor run --stage "$CASE_ROOT/draft" -- /bin/sh -c 'printf accepted > report.txt; printf scratch > scratch.txt'
pvisor apply "$CASE_ROOT/draft" --path report.txt
assert_content report.txt accepted
assert_absent scratch.txt
pvisor review "$CASE_ROOT/draft" --json > "$CASE_ROOT/review.json"
json_paths "$CASE_ROOT/review.json" scratch.txt
pvisor drop "$CASE_ROOT/draft"
assert_absent scratch.txt
pvisor review "$CASE_ROOT/draft" --json > "$CASE_ROOT/review.json"
json_paths "$CASE_ROOT/review.json"
```

### S-USE-007：宿主已修改时拒绝覆盖

审查期间你也修改了同一文件。apply 必须保护新的宿主内容，并保留 Agent 提案供处理。

**语义**：有 preimage 冲突的 apply 失败，宿主内容为 human，暂存内容仍为 agent。

**违反示例**：用旧提案覆盖人类的新改动，或失败时丢掉提案。

<!-- semspec: case id=S-USE-007 -->
```bash
journey_setup
printf base > report.txt
pvisor run --stage "$CASE_ROOT/draft" -- /bin/sh -c 'printf agent > report.txt'
printf human > report.txt
expect_refused pvisor apply "$CASE_ROOT/draft" --all
assert_content report.txt human
pvisor inspect "$CASE_ROOT/draft" -- /bin/cat report.txt > "$CASE_ROOT/view.txt"
assert_content "$CASE_ROOT/view.txt" agent
```

### S-USE-008：终止任务后再决定

任务卡住时，先 `kill`，确认已经停止，再丢弃文件。不要对 live Job 做提交。

**语义**：live Job 拒绝 apply；kill 后状态为 cancelled 且 live 为 false，原件没有改动。

**违反示例**：live apply 成功，kill 后仍在运行或文件穿透。

<!-- semspec: case id=S-USE-008 -->
```bash
journey_setup
pvisor run --stage "$CASE_ROOT/live" -- /bin/sh -c 'printf ready > ready.txt; exec /bin/sleep 30' > "$CASE_ROOT/live.log" 2>&1 &
runner=$!
trap 'kill "$runner" 2>/dev/null || true; wait "$runner" 2>/dev/null || true' EXIT
journey_wait_file "$CASE_ROOT/live/upper/ready.txt" "$runner"
expect_refused pvisor apply "$CASE_ROOT/live" --all
pvisor kill "$CASE_ROOT/live"
expect_refused wait "$runner"
trap - EXIT
pvisor status "$CASE_ROOT/live" --json > "$CASE_ROOT/status.json"
json_expect "$CASE_ROOT/status.json" /run/state '"cancelled"'
json_expect "$CASE_ROOT/status.json" /live false
pvisor drop "$CASE_ROOT/live"
assert_absent ready.txt
```

[返回学习路线](index.md) · [3. 保存提案，探索另一条分支](03-checkpoint-and-fork.md)
