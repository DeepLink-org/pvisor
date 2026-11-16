# 5. 回放轨迹与选择恢复工具

先判断要恢复的是文件提案、Agent 历史，还是完整机器执行状态。workspace checkpoint 用于文件提案；`pvisor-replay` 处理 Agent 原生轨迹；Session execution checkpoint 保存受支持 VM 的完整状态。暂存文件通过 review/apply/drop 审查处理。Gateway 用于模型路由和捕获，cache/memory-pool 用于执行基础设施，不是 Session checkpoint 的替代入口。

### S-USE-015：离线准备轨迹，不重跑历史工具

恢复 Agent 对话前先 prepare-only，确认解析前缀不会执行历史工具命令。需要同目录安装 pvisor-replay。

**语义**：回放前缀解析为 prepared，历史工具没有创建 marker，重放工具数量为零。

**违反示例**：prepare-only 执行历史命令。

运行与断言：[Scrut S-USE-015](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/replay/S-USE-015.md)。

### S-USE-016：完整执行 checkpoint 必须检查能力

本例的 host Session 不支持 CPU/RAM 保存和恢复；兼容的原生 VM Session 支持。检查 capability，要求不支持的操作明确拒绝，不要偷偷改成 workspace fork。

**语义**：execution checkpoint、suspend、resume 和 execution fork 均报告 CAPABILITY_UNSUPPORTED，Session 文件树不变。

**违反示例**：忽略 execution 选项或拒绝后修改 Session。

运行与断言：[Scrut S-USE-016](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-USE-016.md)。

[返回学习路线](index.md)
