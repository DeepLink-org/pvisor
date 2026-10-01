---
status: todo
search:
  exclude: true
---

# 回放保真度

!!! warning "规划中"
    本页尚无符合[基准方法](methodology.md)的数据。现有的历史样本见 [Qwen3.6 实验记录](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/replay/qwen3.6-results.md)：3 道题、5 个 Agent，运行日期、提交号和重复次数未完整登记，不作为兼容性或确定性保证。

## 要回答的问题

从第 N 步回放后续跑，Agent 的下一动作与原始轨迹有多一致？续跑的任务成功率与从头运行相比如何？

## 需求

- 每个适配器（Claude Code、Codex、OpenCode、OpenHands、mini-swe-agent、Pi agent）至少 20 道题、多次重复；
- 指标：下一动作工具完全一致率、可见文本相似度、续跑 reward 与原始 reward 的差；
- 登记运行日期、pVisor 提交、模型与 Agent 版本、采样参数与硬件；
- 失败样本单独列出并分类（上下文重建失败、模型不确定性、环境差异）。

## 验收标准

- 一条命令复现，报告使用 `pvisor-benchmark/v1` schema；
- [回放设计（规划中）](../design/replay.md)引用本页数据。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：[回放](../guides/replay.md)、[SandboxReplay](../guides/sandbox-replay.md)
