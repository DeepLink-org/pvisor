---
hide:
  - toc
---

# 开始使用

沿着一条主线，从安装 CLI 到运行一个可以审查的 Agent Run。

## 1. 安装

安装命令行工具，并确认入口可用：

```bash
pip install persisting
pvisor --help
```

macOS 使用 staged host workspace 前，需要安装 macFUSE：

```bash
brew install --cask macfuse
```

[阅读安装指南 →](installation.md)

## 2. 使用 pVisor 运行 Agent

在 staged workspace 中运行 Agent，检查实际发生的事情，只应用你信任的修改：

```bash
pvisor run --stage ./runs/task-001 -- codex
pvisor review last
pvisor apply last --path src
```

Agent 工作期间，基础项目保持不变。Run Bundle 会记录文件 Effect、实际控制机制、网络证据和警告。
继续阅读[运行第一个 Agent](pvisor/get-started.md)完成完整流程，再学习[选择性 apply](pvisor/guides/review-apply.md)。

**完成本节后：**你会得到一次经过审查的项目修改，并清楚哪些内容仍留在 stage 中。

## 3. 需要时再捕获模型流量

Gateway 可以把一次 Run 的模型流量记到该 Run 的目录里。它是可选的，并且留在 pVisor 内部。
审查循环熟悉之后，再阅读[捕获](pvisor/guides/capture.md)。
