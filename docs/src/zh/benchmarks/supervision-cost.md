# 监督成本：先测完整审查流程

读取 20 个文件的审查信息、选择性合入 10 个并丢弃其余 10 个，机器侧总耗时 P50 约 25 ms。第一版确认这条流程正确且成本较小；人工阅读和判断时间尚未测量。

## Motivation

Agent 的运行速度之外，用户还要付出审批、看 diff 和处理冲突的时间。机器流程成本和真实人的监督成本应各有证据。

## 实验设计 {#interpretation}

30 个独立 stage，无预热。逐次确认 status --review 的 20 项都存在，按路径 apply 10 项，检查目标内容，再 drop 剩余 10 项并核对 lower 保留。wall 是三步机器耗时之和，不含 Agent 生成 stage、等待用户或阅读。

## macOS

本轮在 Linux 实测，以下工作负载没有 macOS 样本；macFUSE/FSKit 开销与并发容量均未测。既有 macOS/HVF 数据继续保留在 [VM 启动时间](startup.md)与[VM 内存报告](vm-memory/index.md)，不移作本页结果。

## Linux：2026-10-04 {#results}

| Step | N | P50 / P95 / P99 ms |
|---|---|---|
| review_ms | 30 | 3.21 / 7.95 / 10.81 |
| apply_ms | 30 | 17.62 / 43.66 / 51.07 |
| drop_ms | 30 | 4.07 / 12.83 / 16.68 |
| wall_ms | 30 | 24.94 / 64.74 / 75.67 |

### 决策数量与实际含义

逐工具审批的决定次数取决于工具请求数；一次 stage 审查允许一次决定多个文件，之后仍需要理解 diff、处理冲突并选择路径。Docker/Git 工作流同样可以集中审查。这里没有参与者实验，不能把一批文件对应一次点击推成“人类时间减少 90%”。

性能数据支持自动化审查链路的机器成本；它不支持用户满意度、正确审批率或最优审批策略的结论。
## 边界与下一轮 {#acceptance}

下一轮人工实验固定任务和 diff、交叉安排逐工具审批 / staged 批审 / Docker+Git，记录等待时间、实际判断次数、错误接受/拒绝与冲突处理；参与者信息、匿名数据和置信区间一起公布。在完成之前，人类分钟数保持未测。

## 复现与证据 {#run}

从仓库根目录运行；输出目录必须是新目录。动态 firmware 入口需要 GNU/Linux 版 CLI；静态 musl 制品使用另一套 firmware 入口，不能混用。Linux、KVM/FUSE/user namespaces、Python 3.14、Rust/GCC、Git/rg、Node 24/npm、Podman/crun 是本机工具环境；Agent 套件另需 Claude/Codex CLI。

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --suites supervision --samples 30 --warmups 3
```

先用 `--samples 1 --warmups 0` 检查环境。脚本、输入定义和正确性断言在 `benchmark/pvisor/v1/`；报告会复制二进制、firmware 和当轮脚本并记录摘要。失败不会进入性能分布；准备、策略拒绝和工作负载错误分别保留。每个页面写明有效样本数；小样本 P95/P99 只描述本批次，不估计长期尾延迟。

[环境、制品与采样方法](methodology.md#product-v1) · [批次清单](../../assets/benchmarks/product-v1-20261004/manifest.json) · [逐样本汇总 CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [原始报告与日志归档](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz)。报告保留源码的未提交状态；当轮可执行制品 SHA256 是身份依据。归档不含数 GB 的 rootfs、二进制和可重建工作区；输入摘要及每轮脚本保留。
