# 第一次运行之后

先把演示替换成一个范围明确的真实任务，例如只修改 `src` 中的一处实现。保留当前仓库基线，确认 Agent 已安装在所选执行环境中，再配置它需要的模型服务与凭据；pVisor 不替 Agent 选择 provider。

## 接入一个真实 Agent

以已安装的 Codex 和显式交付 OpenAI Key 为例，在项目目录中运行；先在宿主环境设置 `OPENAI_API_KEY`，每次使用新的项目外 stage：

```bash
pvisor run --safe --pass-env OPENAI_API_KEY --stage ../agent-task-001 -- codex
pvisor status --review ../agent-task-001
pvisor inspect ../agent-task-001 -- git --no-pager diff -- src
```

`--safe` 按直接可执行文件名匹配网络预设。增加 `--overlaynet-allow` 会替换预设列表，依赖下载或其他模型服务需要连同模型 API 一起列出。HOME 状态写入在退出后丢弃，不要依赖这次运行保存登录状态。其他 Agent 的凭据与接入条件见[Agent 接入](../guides/agents/index.md)。

## 决定保留、重试还是丢弃

Job 停止后检查退出码、实际控制、警告和改动；非零退出也可能留下候选。`inspect` 要求有 OverlayFS 工作区，检查命令使用宿主工具，不在原 VM/容器内运行。

满意时按路径 apply。想从文件状态重新尝试时，工作区 fork 要求已停止且有可重建启动策略的 host Job；VM/container 或带 Gateway 路由的 Job 不支持此路径。父子 stage 必须在同一文件系统，并在全部 apply/drop 清理暂存数据前分叉。冲突处理见[审查与应用](../guides/review-apply.md)。

## 扩大任务范围前

Linux host 的选择性代理仍是协作式；需要强制选择性出口时，准备已安装 Agent 的 VM。凭据交付方式见[凭据与环境变量](../guides/policies/credentials.md)。

增加并发时，为每个任务准备独立 worktree 和 stage，用显式路径定位结果，逐份审查，不并发 apply 到同一目标树。[并行工作区流程](../guides/parallel-agents.md#batch)先用两个离线任务验证记录和合入，再替换成真实 Agent。
