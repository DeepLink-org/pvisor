# 实验记录

这里保存性能与机制的工程实验记录：设计过程中的测量、分析与失败复盘。它们回答「当时为什么这样设计」，不回答「现在该怎么选型」——面向选型的结论以[基准与对比](../benchmarks/index.md)为准。

每页的数值属于各自的冻结批次与制品版本，不代表当前构建的性能水位。`.data/` 路径指向本地归档，未随仓库或站点发布，仅用于批次溯源。

完整 CSV/JSON、来源摘要和原始记录保留在对应批次的本地 `.data/` 或 publication 目录。此前公开的数据文件归档于 `benchmark.new/.data/docs-assets-prune-20261018/`，迁移清单保留原路径、字节摘要和归档位置。

| 页面 | 内容 |
| --- | --- |
| [VM 启动性能分析](vm-startup-performance-analysis.md) | 启动路径的分解、回归与验收记录 |
| [端到端任务性能分析](agent-task-performance-analysis.md) | 真实 Agent 任务在各执行器下的端到端测量 |
| [VM 内存性能分析](vm-memory-performance-analysis.md) | 去重、共享库与池化的静态曲线与读回代价 |
| [文件系统性能分析](filesystem-performance-analysis.md) | OverlayFS 各配置的负载测量与复测记录 |
| [缓存与异步写入性能曲线](filesystem-io-performance-analysis.md) | 请求粒度、并发、同步与写合并的工程曲线 |
| [Benchmark 方法与证据](benchmark-methodology-evidence.md) | 历史批次的测量口径与证据清单 |
