# 回放设计

Replay 把“Agent 曾经调用过哪些工具”转换成“这些工具在当前工作区实际产生了什么结果”，再把新结果交给 Agent 继续执行。它适合复现一次失败、比较修复后的行为，以及构造训练前缀。

使用步骤见[轨迹回放指南](../guides/replay.md)。这里说明边界选择、工具重执行和各 Agent 原生会话之间的关系，方便你判断一次回放结果是否可比。

## 当前处理流水线

`engine.rs::execute` 校验请求与目录，调用 `adapter::build_plan` 读取原生轨迹并选择完整工具批次，校验步数预算与固定 Agent 版本，再为本次回放分配独立 state/output 目录。`after_step` 必须在已有完整批次范围内，不能把同一轮的工具调用切开。

| 阶段 | 作用与产物 |
| --- | --- |
| Prepare | 解析历史、确定边界并生成重建文件；`--prepare-only` 不执行历史工具，也不启动实时 Agent |
| Replay | 在当前 workspace 重新执行支持的工具前缀，用 fresh observation 替换旧结果；`--replay-only` 在边界停止 |
| Continue | 把重建的原生上下文交给指定版本的 Agent，校验首个续跑请求使用了该前缀；边界提示在 fresh observation 之后注入 |
| Finalize | 保存结果、质量、失败、重建轨迹与续跑产物；保留可定位的失败信息 |

`adapter/` 负责轨迹解析、工具语义和启动选择；`bridge/` 负责 Claude、Codex、OpenCode 的模型协议桥。完整适配器与版本表见[回放指南](../guides/replay.md)，版本不是“兼容任意新版”的承诺。

回放历史工具会再次产生文件、命令和可能的远程副作用。文件检查点不保存这些外部状态；旧 observation 无法刷新时，默认拒绝，只有显式允许才记录降级。新上下文中的工具输出可以不同，模型继续时也可以选择不同动作，不能把成功续跑理解为确定性再现原轨迹。

## 代码与验证

```bash
just test pvisor-replay
```

- `adapter/claude_code.rs`：完整批次分组、prepare-only 无工具执行、旧 observation 拒绝或显式降级、历史命令超时与进程组清理；
- `adapter/generic.rs`：Codex/OpenCode 的原生身份、完整工具轮次、续跑前缀验证与传输 nonce 过滤；
- `tests/replay_contract.rs`：mini-swe-agent、SWE-agent、OpenHands、Pi 的 replay-only 边界、提示注入与步数约束；
- `bridge/`：协议桥的请求/响应映射及边界验证。

六种适配器的原生前缀准备实验与逐项结果见[回放保真度](../benchmarks/replay-fidelity.md)。该实验验证前缀和零执行准备；模型续跑质量需要按真实任务另行测量。
