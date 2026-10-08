# 用例

同一个 pVisor，在不同规模下解决不同的问题。

## 一个人，一个 Agent

让 Claude Code 或 Codex 全自动完成一次重构：它改文件、跑测试，可能顺手删掉点东西。你不盯着，跑完像审 PR 一样看改动，只合入想要的路径。**今天可用。**

```bash
pvisor run --safe -- claude
pvisor status --review last
pvisor apply last --path src
```

## 一个人，多个 Agent

同时开几个 Agent 做不同方案。每个 Job 各自隔离、各自留证据，可以批量审查，只合入需要的结果。**下一步（L2）**，见[并行 Agent](../guides/parallel-agents.md)。

## 团队与平台

把 pVisor 放进 CI：让 Agent 跑完、审查、只合入想要的。**今天可用（L1 方式）**；策略与证据驱动的免审和集群化（L2/L3）是方向，见[在 CI 中运行 Agent](../guides/ci.md)。

## 研究与训练

Agentic RL 和评测要的正是「大规模、不可信、可记录」的执行基座：轨迹可记录、可从检查点分叉、可按工具前缀回放。**方向**，见 [RL rollout](../guides/rl-rollouts.md) 与[研究方向](../design/research/index.md)。
