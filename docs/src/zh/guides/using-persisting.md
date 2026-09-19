# 使用 Persisting

选择能回答当前问题的最小工作流。

## 我需要 Agent 安全地修改项目

从[pVisor 入门](../pvisor/get-started.md)开始：在 staged workspace 中运行 Agent，
审查 Run Bundle，再只应用信任的路径。下一次 Run 确实需要时，再增加网络或 provider 控制。

## 我需要留下模型流量记录

当一次 Run 需要保留实际发出的模型请求和响应时，使用[pVisor capture](../pvisor/guides/capture.md)。
私有 Run Bundle 仍是本地执行记录。

## 一套可靠的使用习惯

1. 从一次 Run 开始。
2. 记录完整命令、路径和 provider。
3. 在应用或分享前先审查结果。
4. 让结论始终带着对应的 Run Bundle。
5. 手工路径可重复后，再进入自动化。

实现背景见[设计原则](../system-design/design-principles.md)。
