# 从这里开始

**PolicyVisor（pVisor）**为 Agent CLI、脚本和自动化命令提供策略约束下的可审查执行。它记录实际控制；启用暂存后，你可以先审查文件改动，再应用到项目。

Python 包和 CLI 统一使用 `pvisor`，仓库链接继续使用现有的 `Persisting` 路径。

## 完成第一个闭环

1. [安装 pVisor](installation.md)，确认平台要求。
2. [运行一个小示例](first-run.md)，不需要 Agent 账号或 API Key。
3. 把示例命令换成你的脚本、自动化命令或 Agent CLI。
4. [审查并应用](../guides/review-apply.md)需要保留的改动。

需要审查文件改动时，请显式使用 `--stage`。不使用它，命令可能直接修改项目。文件暂存也不会撤销网络请求或外部服务中的变更。

## 按问题查找

| 问题 | 文档 |
| --- | --- |
| pVisor 能做什么？ | [产品概览](what-is-pvisor.md) |
| 命令应该运行在哪里？ | [宿主机、容器与 VM](../guides/execution.md) |
| 这次运行实际隔离了什么？ | [能力与证据](../concepts/capabilities-and-evidence.md) |
| 如何记录模型请求？ | [流量捕获](../guides/capture.md) |
| 需要使用哪个选项？ | [CLI 参考](../reference/cli.md) |
| 如何构建和参与开发？ | [开发入口](../development/index.md) |
