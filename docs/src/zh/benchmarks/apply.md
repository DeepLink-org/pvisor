# apply / drop：文件数量、冲突与恢复

## 主要结论 {#conclusions}

小批量合入适合交互式审查：10 文件约 **15 ms**、1,000 文件约 **0.84 s**。10 万文件约 **5.5 分钟**，不适合高频大批量提交。Git patch 同批明显更快；pVisor 的合入流程还包含原像冲突检查、持久化与恢复。

## Motivation {#motivation}

保留改动的收益需要提交时兑现。除了快慢，还要确认宿主并行修改不会被覆盖，以及进程中断后是否能完成同一笔提交。

## 实验设计 {#interpretation}

真实 staged Python 任务改写既有文本文件，apply 前核对 lower 未变和 upper 数量。计时只含 apply/drop CLI，不含 stage 生成与逐文件验证。10 文件 N=30、1,000 N=10、100,000 N=3；前两组每动作 1 次预热，最大组无预热。最大组 3 个 stage 并行准备完后，操作串行计时。冲突用宿主改写第一个文件，要求拒绝且其余目标未变。

这些结果来自 Linux/x86_64；macOS 的对应负载未测。每项数字的制品、缓存条件与样本保存在关联报告中。

## 实验数据和分析 {#results}

| Files | Operation | N | P50 / P95 / P99 ms |
|---|---|---|---|
| 10 | apply | 30 | 15.01 / 16.75 / 18.26 |
| 10 | drop | 30 | 3.38 / 3.82 / 4.72 |
| 10 | conflict | 30 | 4.07 / 9.44 / 11.67 |
| 10 | copy | 30 | 5.07 / 10.81 / 11.59 |
| 10 | git-apply | 30 | 0.73 / 0.85 / 0.88 |
| 1000 | apply | 10 | 836.38 / 1462.81 / 1864.62 |
| 1000 | drop | 10 | 24.44 / 26.02 / 26.24 |
| 1000 | conflict | 10 | 42.90 / 43.29 / 43.34 |
| 1000 | copy | 10 | 15.84 / 16.20 / 16.24 |
| 1000 | git-apply | 10 | 13.61 / 14.43 / 14.61 |
| 100000 | apply | 3 | 330396.22 / 337869.09 / 338533.34 |
| 100000 | drop | 3 | 996.55 / 1207.85 / 1226.63 |
| 100000 | conflict | 3 | 247496.21 / 253640.35 / 254186.49 |
| 100000 | copy | 3 | 1214.85 / 1359.59 / 1372.45 |
| 100000 | git-apply | 3 | 1464.63 / 1552.12 / 1559.90 |

### 分析

小批量可以在交互流程中合入；千文件已经接近秒级，十万文件成本远高于简单复制或 Git patch。最大组三次实测约 325–339 秒，不能用 3 个样本证明稳定尾延迟。冲突拒绝在十万文件下也需要约 247 秒，检查成本同样是瓶颈。性能有优化空间，所测配置公开原始结果。

copy 是复制到空目录；git-apply 修改相同文本文件，但没有 pVisor 的暂存清理、preimage/ledger 协议与恢复流程，因此都是功能成本对照，不能宣称语义等价。主批次 10/1,000 文件和配置对照 100,000 文件分开归档。

100,000 文件 stage 出现 `trace append rejected: event exceeds size limit` 的观察记录缺口；文件、ledger、冲突检查仍通过，但不能声称完整文件审计记录。

### SIGKILL 恢复

在 prepared、target_applied 或 committed 状态注入 SIGKILL，重跑后核对目标内容与已提交 ledger。下表仅含实际命中的注入；1,000 文件组有两次 committed 窗口未命中，不算成功注入，原始报告保留。恢复延迟依赖中断时已经完成的步骤。

| Files | Requested kill state | N | Durable state at death | Recovery P50/P95/P99 ms |
|---|---|---|---|---|
| 1000 | prepared | 3 | prepared | 1587.60 / 1640.03 / 1644.70 |
| 1000 | target_applied | 3 | target_applied | 120.98 / 123.47 / 123.69 |
| 10000 | prepared | 3 | prepared | 8856.00 / 9023.85 / 9038.77 |
| 10000 | target_applied | 3 | target_applied | 354.28 / 371.49 / 373.02 |
| 10000 | committed | 3 | committed | 259.47 / 347.70 / 355.55 |

### 和 Git patch 相比，这个成本意味着什么 {#baseline-meaning}

同一批输入的 `git apply` 是熟悉的成本基线：10 文件约 0.73 ms、1,000 文件约 13.61 ms，而 pVisor apply 约 15 ms、836 ms，分别约 21 倍、61 倍。绝对时间更有用：少量改动可以按十几毫秒预算，千文件合入要按秒预算，十万文件要按分钟预算。

Git patch、复制和 pVisor apply 的流程不同；这里比较完成相同文本更新的实际耗时，不把它们说成相同事务。是否选择 pVisor，应同时考虑 preimage 检查、选择性合入和崩溃恢复是否是工作流的要求。大批量改动目前有明确的性能代价。

### 适用边界 {#acceptance}

SIGKILL 恢复不是断电、文件系统损坏或磁盘丢写测试；所测配置也未覆盖全部文件类型、符号链接和元数据组合。追加这些场景时保留失败和恢复前后证据。更大的项目应先控制单次提交规模；所测配置未用十文件结果外推百万文件。

### 数据来源与复现 {#run}

[配置与采样方法](methodology.md#product-v1) · [Manifest](../../assets/benchmarks/product-v1-20261004/manifest.tsv) · [Samples CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [Raw evidence](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz)

复现命令与环境要求见[方法技术记录](../design/benchmark-methodology-evidence.md)。
