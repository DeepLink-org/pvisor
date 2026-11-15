# 3. 保存提案，探索另一条分支

workspace checkpoint 保存文件提案与冲突 preimage。它适合重构方案、人工审查和并行尝试。fork 启动新命令，不会继续旧进程的内存；完整 VM 恢复在第五章另行说明。

### S-USE-009：重复请求只保存一次

自动化调用方可能重试请求。固定 `--request-id`，再用 list/show 查同一个保存点。

**语义**：相同请求返回同一 checkpoint，第二次 reused=true，list 只有一条。

**违反示例**：重试产生重复 checkpoint 或不同 ID。

运行与断言：[Scrut S-USE-009](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/checkpoint/S-USE-009.md)。

### S-USE-010：接受文件后仍能审查保存点

对当前提案做决定之前保存 checkpoint，接受后仍可以审查原提案。

**语义**：apply 后当前改动为空；checkpoint review 仍列出 report.txt，已全部 apply 的 Session 拒绝 drop，checkpoint 保持。

**违反示例**：apply/drop 改写不可变保存点。

运行与断言：[Scrut S-USE-010](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/checkpoint/S-USE-010.md)。

### S-USE-011：从同一提案启动两个分支

比较两种实现方案时，从相同 checkpoint 启动两次 fork。源提案和每个子 Session 都各自保留。

**语义**：两个分支都读到 seed，分别写成 branch-a/branch-b，源仍为 seed，宿主仍无 report.txt；删除被引用 checkpoint 明确失败。

**违反示例**：分支互相污染、修改源提案或删除仍被引用的保存点。

运行与断言：[Scrut S-USE-011](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/checkpoint/S-USE-011.md)。

[返回学习路线](index.md) · [4. 为不可信任务声明边界](04-boundaries.md)
