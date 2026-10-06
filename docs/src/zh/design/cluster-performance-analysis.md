# 已退役的 Cluster 性能证据

**Controller/Worker 测量描述的是退役制品，不是当前 daemon。** B-CLUSTER 没有活动测量、绘图或发布入口。[历史 benchmark](../benchmarks/cluster-scalability.md)保留完整任务与历史成本表格及原 CSV 来源。没有新增测量，也没有 daemon 密度结论。

## 历史 VM 就绪探针 {#execution}

2026-10-05 的 shell/sleep 实验同时增加 Worker 数和 CPU 预算，每个 VM 使用独立 Worker。它观察一到四个 guest 的并行就绪，不是固定预算有效任务吞吐或单 Worker 密度。

![历史退役 Worker 就绪探针](../../assets/benchmarks/cluster-scalability-20261005/execution.svg)

| 存活 VM | 总内存 P50，MiB | 就绪 P50，s | 批量就绪速率，台/s |
|---:|---:|---:|---:|
| 1 | 92.93 | 3.807 | 0.261 |
| 2 | 178.00 | 3.789 | 0.525 |
| 4 | 342.96 | 4.383 | 0.901 |

每档一次预热、五个测量批次。guest 为 128 MiB/一 vCPU；每个 Worker 及其子进程限制为 512 MiB/0.5 核，Controller 为 256 MiB/0.25 核，零 swap。共享 Linux/KVM 宿主使用预备最小输入和 debug 制品，其他宿主负载未停止。就绪包含提交 CLI/HTTP、持久化、调度与 VM 启动。内存是全部 guest 就绪时互不重叠服务 cgroup 之和，不是启动峰值。图中为观察范围，不是置信区间。这些观察不能证明生产容量、尾延迟、共享工作集收益或当前 daemon 性能。

## 历史 Controller 成本 {#controller}

![历史退役 Controller 查询与恢复](../../assets/benchmarks/cluster-scalability-20261005/controller.svg)

| 保留记录 | 索引计数 P50，ns | 全扫描参考 P50，ms | 进程与 ID fixture RSS，MiB | 日志，MiB | 热回放，s |
|---:|---:|---:|---:|---:|---:|
| 1,000 | 202.83 | 0.068 | 12.13 | 1.93 | 0.079 |
| 10,000 | 286.78 | 5.254 | 70.59 | 19.33 | 0.388 |
| 100,000 | 129.40 | 20.789 | 658.82 | 193.25 | 1.744 |
| 1,000,000 | 169.47 | 134.204 | 6,540.58 | 1,932.55 | 16.090 |

归档 `controller-indexes-20261005-v3-history-*` 每档使用一个 release 进程、一个 CPU、NVMe 和热回放。只有一个 ready 记录，其余为已取消历史，不执行 guest 或 HTTP 压测。计数使用二十个查询样本，每样本一百次索引调用；RSS 与回放每档只有一次观察。RSS 含 ID fixture 和分配器留存。热回放不包含 Worker 对账或冷磁盘恢复。全扫描是算法参考，不是另一 Controller 制品的吞吐。不能与历史 benchmark 中独立的后续历史批次合并。

## 证据边界 {#limits}

就绪和查询观察仍是冻结实现的历史描述，不提供当前优化优先级、daemon 容量建议或调度系统/运行时排名。本机[容量](../benchmarks/density.md)和[VM 内存](../benchmarks/vm-memory/index.md)拥有独立实验对象与保留证据；单 VM 内存回收不能证明并发密度。[退役计划](cluster-benchmark-plan.md)区分旧提案与已测结论。

## 保留证据，不提供活动复现 {#reproduce}

`cluster_scalability.py`、`cluster_worker.py`、`controller_history.py`、专属测试、`plot_cluster_scalability.py` 和 `publish_controller_history.py` 已删除。退役 scheduler example 没有当前构建/运行/发布命令。原日志、样本、manifest 和冻结 harness 保留在本地 `.data/`；保留副本是归档，不是活动入口。删除旧 crate measurements 不授权改写 receipt 或将旧测量归于 daemon 源码。

历史图表输入记录于 `docs/src/assets/benchmarks/.data/cluster-scalability-20261005/`：`vm.tsv`、`vm-summary.csv`、`controller-summary.csv`、`controller-provenance.tsv`、`manifest.tsv` 和 `setup-failure.tsv`。这些是本地来源位置，不是站点下载链接。[运行手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)记录退役范围与独立活动 native 探针。
