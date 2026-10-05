# 监督成本：先测完整审查流程

## 主要结论 {#conclusions}

审查 20 个文件、合入其中 10 个并丢弃其余 10 个，机器流程 P50 约 **25 ms**。这一成本适合交互式使用；人的阅读与判断时间未测，不能据此声称降低某个比例的人工监督成本。Git/diff 工作流同样可以集中审查。

## Motivation {#motivation}

Agent 的运行速度之外，用户还要付出审批、看 diff 和处理冲突的时间。机器流程成本和真实人的监督成本应各有证据。

## 实验设计 {#interpretation}

30 个独立 stage，无预热。逐次确认 status --review 的 20 项都存在，按路径 apply 10 项，检查目标内容，再 drop 剩余 10 项并核对 lower 保留。wall 是三步机器耗时之和，不含 Agent 生成 stage、等待用户或阅读。

这些结果来自 Linux/x86_64；macOS 的对应负载未测。每项数字的制品、缓存条件与样本保存在关联报告中。

## 实验数据和分析 {#results}

| Step | N | P50 / P95 / P99 ms |
|---|---|---|
| review_ms | 30 | 3.21 / 7.95 / 10.81 |
| apply_ms | 30 | 17.62 / 43.66 / 51.07 |
| drop_ms | 30 | 4.07 / 12.83 / 16.68 |
| wall_ms | 30 | 24.94 / 64.74 / 75.67 |

### 决策数量与实际含义

逐工具审批的决定次数取决于工具请求数；一次 stage 审查允许一次决定多个文件，之后仍需要理解 diff、处理冲突并选择路径。Docker/Git 工作流同样可以集中审查。这里没有参与者实验，不能把一批文件对应一次点击推成“人类时间减少 90%”。

性能数据支持自动化审查链路的机器成本；它不支持用户满意度、正确审批率或最优审批策略的结论。
### 基线与使用预算 {#baseline-meaning}

熟悉的操作参照是使用 Git/diff 查看改动，再选择要保留的文件。所测配置没有测量对等的 Git 审查流程。已测约 25 ms 是机器完成清单、过滤和提交的时间，适合放在一次交互流程内；它不代表用户能在 25 ms 内完成审查，也不能换算成节省的人时。人类审查能力与耗时尚需独立实验。

### 适用边界 {#acceptance}

未测人工阅读和判断时间，也未测真实团队审查成功率。单项机器耗时不能推出人力节约比例。

### 数据来源与复现 {#run}

[配置与采样方法](methodology.md#product-v1) · [Manifest](../../assets/benchmarks/product-v1-20261004/manifest.tsv) · [Samples CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [Raw evidence](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz)

复现命令与环境要求见[方法技术记录](../design/benchmark-methodology-evidence.md)。
