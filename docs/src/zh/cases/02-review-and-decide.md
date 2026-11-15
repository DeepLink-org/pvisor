# 2. 先审查，再接受文件改动

Agent 会修改代码时，先加 `--stage`，再用 `review` 和 `inspect` 看结果，最后 `apply` 或 `drop`。stage 只暂存工作区改动，host 仍可访问其他路径；需要访问边界时使用第四章的 `--safe`。

### S-USE-005：查看 Agent 看到的文件

准备让 Agent 修改一份报告，先检查暂存内容与原件的区别。`inspect` 提供只读视图。

**语义**：原件保持 original，inspect 读到 proposed，review 列出报告改动。

**违反示例**：写入穿透原件或 review 漏报改动。

运行与断言：[Scrut S-USE-005](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-USE-005.md)。

### S-USE-006：只接受需要的文件

Agent 同时修改正文和实验文件时，可以只接受正文，再丢弃剩余改动。

**语义**：apply --path 只提交 report.txt，剩余 scratch.txt 被 drop 丢弃，Session 记录仍可查询。

**违反示例**：选择性 apply 提交所有文件，或 drop 删除记录。

运行与断言：[Scrut S-USE-006](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-USE-006.md)。

### S-USE-007：宿主已修改时拒绝覆盖

审查期间你也修改了同一文件。apply 必须保护新的宿主内容，并保留 Agent 提案供处理。

**语义**：有 preimage 冲突的 apply 失败，宿主内容为 human，暂存内容仍为 agent。

**违反示例**：用旧提案覆盖人类的新改动，或失败时丢掉提案。

运行与断言：[Scrut S-USE-007](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-USE-007.md)。

### S-USE-008：终止任务后再决定

任务卡住时，先 `kill`，确认已经停止，再丢弃文件。不要对 live Session 做提交。

**语义**：live Session 拒绝 apply；kill 后状态为 cancelled 且 live 为 false，原件没有改动。

**违反示例**：live apply 成功，kill 后仍在运行或文件穿透。

运行与断言：[Scrut S-USE-008](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-USE-008.md)。

[返回学习路线](index.md) · [3. 保存提案，探索另一条分支](03-checkpoint-and-fork.md)
