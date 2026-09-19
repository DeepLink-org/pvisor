# 使用 PolicyVisor

选择能回答当前问题的最小工作流。

## 我需要先审查改动，再写入项目

从[第一次运行](../start/first-run.md)开始：使用 `--stage` 运行 Agent CLI、脚本或
自动化命令，审查 Run Bundle，再应用选定的路径。

## 我需要限制执行过程中的访问

根据任务选择[执行器](execution.md)，配置[网络策略](network.md)。查看
[能力与证据](../concepts/capabilities-and-evidence.md)，区分声明的权限与实际的强制执行能力。

## 我需要留下模型流量记录

当一次 Run 需要保留实际发出的模型请求和响应时，使用[pVisor capture](capture.md)。
私有 Run Bundle 仍是本地执行记录。

## 一套可靠的使用习惯

1. 从一次 Run 开始。
2. 记录完整命令、路径和 provider。
3. 在应用或分享前先审查结果。
4. 让结论始终带着对应的 Run Bundle。
5. 手工路径可重复后，再进入自动化。

实现背景见[设计原则](../design/principles.md)。
