# Agent 工具闭环：真实 CLI、受控响应

72/72 次受控任务通过。真实 Claude Code/Codex CLI 完成修复、测试和结果回传；Claude 暂存总耗时增加约 44 ms，Codex 的差异在本轮波动范围内。它验证 pVisor 对工具闭环的成本与兼容性；真实模型的解题能力差异尚未测量。

## Motivation

想知道 pVisor 是否干扰 Agent，需要先固定工具动作，排除模型和公网波动，再测真实模型任务。第一步给出可重复的工具闭环基线。

## 实验设计 {#interpretation}

Claude Code 2.1.128、Codex 0.160.0；同机本地模型响应固定为“改写 adder.py 并执行 grade.py”，工具输出 GRADE_PASS 后再返回结束。6 个算术输入任务、每任务 3 次独立重复，每 CLI 每后端计划 18 次，无预热。每任务 10 个断言；不是 6 类独立软件缺陷。使用假凭据，不进行模型推理、不付费。固定 usage 字段用于协议，不能作为真实 tokens 或账单。

## macOS

本轮在 Linux 实测，以下工作负载没有 macOS 样本；macFUSE/FSKit 开销与并发容量均未测。既有 macOS/HVF 数据继续保留在 [VM 启动时间](startup.md)与[VM 内存报告](vm-memory/index.md)，不移作本页结果。

## Linux：2026-10-04 {#results}

| CLI | Backend | Passed/planned | Wall P50/P95/P99 ms | P50 vs native |
|---|---|---|---|---|
| claude | native | 18/18 | 302.15 / 318.58 / 340.44 | +0.0% |
| claude | staged | 18/18 | 345.69 / 365.95 / 366.29 | +14.4% |
| codex | native | 18/18 | 1433.75 / 1543.04 / 1546.18 | +0.0% |
| codex | staged | 18/18 | 1401.23 / 1521.71 / 1521.86 | -2.3% |

### 分析

每次检查修复后的文件、测试结果回到了下一次模型请求、CLI 正常结束；staged 原目录仍是错误版本。按 CLI 分别比较 native 与 staged，不能把两个不同客户端的启动时间解释成模型速度差。Codex staged P50 为 -2.3%，但本轮 native/staged 顺序固定、共享桌面有后台负载，不能据此宣称 pVisor 加速，也没有对等性置信区间。输入与模型服务随归档脚本公开。

控制实验没有成功率置信区间，也不能据 100% 通过宣称 SWE-bench 性能不变。它只支持本机这两个版本、Bash/exec_command 与该修复路径。
## 边界与下一轮 {#acceptance}

真实模型 SWE-bench Lite 子集、真实 tokens、复杂多工具任务和模型输出波动未测；未授权使用付费账户。后续固定 task IDs、仓库 commit、模型、预算、工具/网络权限，随机 native/staged 顺序并保留所有失败与原始轨迹。第一版不编造这部分数字。

## 复现与证据 {#run}

从仓库根目录运行；输出目录必须是新目录。动态 firmware 入口需要 GNU/Linux 版 CLI；静态 musl 制品使用另一套 firmware 入口，不能混用。Linux、KVM/FUSE/user namespaces、Python 3.14、Rust/GCC、Git/rg、Node 24/npm、Podman/crun 是本机工具环境；Agent 套件另需 Claude/Codex CLI。

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --suites agent --samples 3 --warmups 0
```

先用 `--samples 1 --warmups 0` 检查环境。脚本、输入定义和正确性断言在 `benchmark/pvisor/v1/`；报告会复制二进制、firmware 和当轮脚本并记录摘要。失败不会进入性能分布；准备、策略拒绝和工作负载错误分别保留。每个页面写明有效样本数；小样本 P95/P99 只描述本批次，不估计长期尾延迟。

[环境、制品与采样方法](methodology.md#product-v1) · [批次清单](../../assets/benchmarks/product-v1-20261004/manifest.json) · [逐样本汇总 CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [原始报告与日志归档](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz)。报告保留源码的未提交状态；当轮可执行制品 SHA256 是身份依据。归档不含数 GB 的 rootfs、二进制和可重建工作区；输入摘要及每轮脚本保留。
