# 用例：价值随规模展开

| 规模 | 读者 | 级别 | 状态 |
| --- | --- | --- | --- |
| 一个人 · 一个 Agent | 开发者 | L1 | 今天可用 |
| 一个人 · 多个 Agent | 开发者 | L2 | 下一步 |
| 团队与平台 | CI、平台工程 | L2–L3 | 方向 |
| 研究与训练 | MLSys、Agentic RL | L3 | 方向 |

## 一个人 · 一个 Agent（今天可用）

**场景**：让 Claude Code 或 Codex 完成一次跨多个文件的重构。

**今天的做法与痛点**：要么开着审批模式，一条条确认命令，半小时的任务要盯半小时；要么开全自动，结束后通读 `git diff`，还担心它删了不该删的文件、读过 `~/.ssh`、或者访问过别的地址。

**用 pVisor 之后**：

```bash
pvisor run --safe -- codex
pvisor status --review last
pvisor apply last --path src
```

Agent 无人值守地跑完；改动停在暂存区，项目文件在 apply 前不变；审查时看到改动清单，以及被拒绝的敏感路径和网络访问；只合入 `src`，其余一键丢弃。期间你自己改过的文件，pVisor 拒绝覆盖。

相关指南：[第一次运行](../start/first-run.md)、[接入你的 Agent](../guides/agents/index.md)、[审查并应用改动](../guides/review-apply.md)。

## 一个人 · 多个 Agent（下一步）

**场景**：让两三个 Agent 用不同方案同时尝试同一个问题，挑最好的结果合入。

**今天的做法与痛点**：多个 Agent 共用一个工作区会互相踩踏；分别开 worktree 只隔离文件，不管网络和凭据，也没有可比较的记录。

**用 pVisor 之后**：每个 Agent 是一个独立的 Job，各自暂存、各自留证据。今天已经可以从同一个检查点分叉出多个 Job（见[逻辑检查点与分叉](../guides/fork-checkpoint.md)）；批量审查与并发运行的指南在建设中，见[并行 Agent（规划中）](../guides/parallel-agents.md)。

## 团队与平台（方向）

**场景**：在 CI 中让 Agent 修复失败的测试，或在内部平台上为多个团队运行 Agent。

**今天的做法与痛点**：不敢让 Agent 进入无人值守的流水线，因为出了问题既不知道它做了什么，也无法只撤回它的改动。

**用 pVisor 之后**：用策略划定边界，靠 Run Bundle 做审计，只有超出策略的结果才需要人处理。CI 集成见[在 CI 中运行 Agent（规划中）](../guides/ci.md)；集群化执行见[信任阶梯](trust-ladder.md)。

## 研究与训练（方向）

**场景**：Agentic RL 的 rollout、Agent 评测、轨迹数据采集。

**今天的做法与痛点**：大量不可信的执行需要隔离、可复现、有记录，并且常常需要从某个中间状态分叉出多条轨迹。

**用 pVisor 之后**：每次 rollout 都是有界、可逆、可查的执行；Gateway 记录模型交互，逻辑检查点支持从中间状态分叉，回放可以按工具前缀重建上下文后实时续跑（见[回放](../guides/replay.md)）。作为批量执行基座的设计见[RL rollout（规划中）](../guides/rl-rollouts.md)。
