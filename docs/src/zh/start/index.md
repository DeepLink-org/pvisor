# 从这里开始

**PolicyVisor（pVisor）** 让 Agent 全自动执行，文件改动由你决定去留。它运行你现有的 Agent CLI、脚本和自动化命令，并把每次运行实际生效的限制记录下来。

Python 包和 CLI 统一使用 `pvisor`。

## 完成第一个闭环

1. [安装 pVisor](installation.md)，确认平台要求。
2. [运行一个小示例](first-run.md)，不需要 Agent 账号或 API Key。
3. 把示例命令换成你的脚本、自动化命令或 Agent CLI。
4. [审查并应用](../guides/review-apply.md)需要保留的改动。

使用 `--safe` 或 `--stage PATH` 暂存工作区改动，退出后再审查。默认行为及 HOME、VM 根目录的区别见 [暂存与存储](../reference/cli.md#暂存与存储)。

## 按问题查找

| 问题 | 文档 |
| --- | --- |
| pVisor 能做什么？ | [产品概览](what-is-pvisor.md) |
| 命令应该运行在哪里？ | [宿主机、容器与 VM](../guides/executors/index.md) |
| 这次运行实际隔离了什么？ | [能力与证据](../concepts/capabilities-and-evidence.md) |
| 如何记录模型请求？ | [流量捕获](../guides/capture.md) |
| 需要使用哪个选项？ | [CLI 参考](../reference/cli.md) |
| 如何构建和参与开发？ | [开发入口](../community/index.md) |
