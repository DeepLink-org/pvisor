# 历史轨迹能否准确准备为新的执行起点？

## 主要结论 {#conclusions}

**六种固定版本轨迹格式的前缀准备全部通过校验，P50 约 **5–5.5 ms**，prepare-only 不执行工具、不修改工作区。该能力适合绑定任务与历史观测；它不等于恢复任意远程连接，也不证明模型下一步动作或所有新版 CLI 格式保持一致。**

| 需求 | 选型含义 |
|---|---|
| 绑定已记录的工具历史 | 使用固定格式的 prefix 准备 |
| 恢复实际文件状态 | 另行恢复任务环境 |
| 预测下一步动作或 reward | 没有相应一致率实测 |

## Motivation {#motivation}

训练或复现任务时，轨迹结构正确还不够：边界、工具参数和历史观测也要保留。准备阶段尤其不应偷偷执行命令。

## 实验设计 {#interpretation}

每 adapter 20 个合成轨迹，2 个工具批次，选 after-step=1；3 次独立重复，每 adapter 60 次。要求 manifest 边界是 1 批/1 call、命令参数精确保留、replayed_tool_calls=0 且工作区为空。固定格式版本见表，不等同于本机 CLI 版本。无模型请求，也不执行工具；计时是 prefix 准备。

这些结果来自 Linux/x86_64；macOS 的对应负载未测。每项数字的制品、缓存条件与样本保存在关联报告中。

固定制品与测量日期按表注明。失败与校验不通过的样本不计入成功耗时，失败数量单列；既有数据没有事先的宿主干扰剔除规则，所有通过校验的慢样本保留。30 次及更少采样的 P95 仅为观察参考，不给 P99 或稳定尾延迟承诺。

## 实验数据和分析 {#results}

| Adapter | Pinned format profile | Passed/planned | Preparation P50/P95 ms |
|---|---|---|---|
| claude-code | claude-code/2.1.220/native-resume-v1 | 60/60 | 5.03 / 5.62 |
| codex | codex/0.149.0/native-responses-jsonl-v1 | 60/60 | 5.05 / 5.49 |
| opencode | opencode/1.17.7/native-events-jsonl-v1 | 60/60 | 5.16 / 5.42 |
| mini-swe-agent | mini-swe-agent/2.4.6/native-messages-v1 | 60/60 | 5.48 / 6.76 |
| openhands | openhands/0.53.0/native-replay-v1 | 60/60 | 5.18 / 6.10 |
| pi-agent | pi-agent/0.83.0/native-rpc-events-v1 | 60/60 | 5.25 / 6.08 |

### 适用边界 {#acceptance}

合成轨迹验证前缀结构、边界与参数，不代表模型下一动作或 reward 一致率。真实会话、新版 CLI、长前缀、token 成本和远端连接恢复未测。

