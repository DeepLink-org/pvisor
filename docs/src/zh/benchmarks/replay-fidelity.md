# 回放保真度：原生前缀与零执行准备

首版检查六种原生轨迹格式的前缀、工具参数和 prepare-only 零执行语义。验证发现并修复了 Codex/OpenCode 在 prepare-only 执行历史命令的问题；回归测试同时检查执行数和工作区无副作用。

## Motivation

训练或复现任务时，轨迹结构正确还不够：边界、工具参数和历史观测也要保留。准备阶段尤其不应偷偷执行命令。

## 实验设计 {#interpretation}

每 adapter 20 个合成轨迹，2 个工具批次，选 after-step=1；3 次独立重复，每 adapter 60 次。要求 manifest 边界是 1 批/1 call、命令参数精确保留、replayed_tool_calls=0 且工作区为空。固定格式版本见表，不等同于本机 CLI 版本。无模型请求，也不执行工具；计时是 prefix 准备。

## macOS

本轮在 Linux 实测，以下工作负载没有 macOS 样本；macFUSE/FSKit 开销与并发容量均未测。既有 macOS/HVF 数据继续保留在 [VM 启动时间](startup.md)与[VM 内存报告](vm-memory/index.md)，不移作本页结果。

## Linux：2026-10-04 {#results}

| Adapter | Pinned format profile | Passed/planned | Preparation P50/P95/P99 ms |
|---|---|---|---|
| claude-code | claude-code/2.1.220/native-resume-v1 | 60/60 | 5.03 / 5.62 / 6.13 |
| codex | codex/0.149.0/native-responses-jsonl-v1 | 60/60 | 5.05 / 5.49 / 6.49 |
| opencode | opencode/1.17.7/native-events-jsonl-v1 | 60/60 | 5.16 / 5.42 / 6.02 |
| mini-swe-agent | mini-swe-agent/2.4.6/native-messages-v1 | 60/60 | 5.48 / 6.76 / 7.28 |
| openhands | openhands/0.53.0/native-replay-v1 | 60/60 | 5.18 / 6.10 / 7.97 |
| pi-agent | pi-agent/0.83.0/native-rpc-events-v1 | 60/60 | 5.25 / 6.08 / 7.41 |

### 发现与修复

旧 generic 路径在判断 PrepareOnly 前执行历史工具，报告却为零；工作区副作用断言捕获了它。现在先判断模式，PrepareOnly 不进入工具执行循环。新增 Codex/OpenCode 原生 JSONL 回归还检查历史观察未被新观察替换。`just test pvisor-replay` 的 **128/128** 项通过，修复后的回放制品摘要单独归档，与其余性能用的 pvisor CLI 摘要分开。

结构和参数检查全部通过才算该前缀成功，不用模型回复相似度代替输入正确性。
## 边界与下一轮 {#acceptance}

本版不是下一步动作一致率或 reward 一致率实验；合成轨迹不能代表所有真实会话、新版本或长对话。真实模型的分叉动作、环境重建后 reward、token 成本和长前缀延迟未测。已有设计与历史实验入口保留在[回放设计](../design/replay.md)，不得把这些合成通过数当成模型保真度。

## 复现与证据 {#run}

从仓库根目录运行；输出目录必须是新目录。动态 firmware 入口需要 GNU/Linux 版 CLI；静态 musl 制品使用另一套 firmware 入口，不能混用。Linux、KVM/FUSE/user namespaces、Python 3.14、Rust/GCC、Git/rg、Node 24/npm、Podman/crun 是本机工具环境；Agent 套件另需 Claude/Codex CLI。

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --suites replay --samples 3 --warmups 0
```

先用 `--samples 1 --warmups 0` 检查环境。脚本、输入定义和正确性断言在 `benchmark/pvisor/v1/`；报告会复制二进制、firmware 和当轮脚本并记录摘要。失败不会进入性能分布；准备、策略拒绝和工作负载错误分别保留。每个页面写明有效样本数；小样本 P95/P99 只描述本批次，不估计长期尾延迟。

[环境、制品与采样方法](methodology.md#product-v1) · [批次清单](../../assets/benchmarks/product-v1-20261004/manifest.json) · [逐样本汇总 CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [原始报告与日志归档](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz)。报告保留源码的未提交状态；当轮可执行制品 SHA256 是身份依据。归档不含数 GB 的 rootfs、二进制和可重建工作区；输入摘要及每轮脚本保留。
