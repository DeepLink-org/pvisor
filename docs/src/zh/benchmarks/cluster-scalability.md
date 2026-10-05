# 增加 Worker 能得到更多有效任务结果吗？

## 主要结论 {#conclusions}

**增加 Worker 和 CPU 预算时，1–4 台轻量 VM 能并行就绪，内存近似按数量增长。** 4 台就绪 P50 约 **4.38 s**，服务 cgroup 总内存约 **343 MiB**。这没有验证固定总预算的 Agent 吞吐，也没有与其他集群工具建立速度排名。

控制面计数查询约 **0.13–0.29 µs**，但大量保留历史仍有成本：百万记录的进程与 fixture RSS 约 **6.39 GiB**，热日志回放约 **16 s**。查询快不能代表历史状态无限扩展。

| 需求 | 选型含义 |
|---|---|
| 评估执行环境的基础占用 | 参考轻量 VM 就绪数据 |
| 规划有效任务吞吐 | 仍需实际任务完成率对照 |
| 长期保留大量历史 | 评估内存与重启回放成本 |

## Motivation {#motivation}

并行任务需要同时考虑执行环境、调度等待、活跃资源和历史记录。用户关心的是增加资源能否获得更多有效结果，以及长期运行后重启与状态保留的成本。

## 实验设计 {#interpretation}

Linux 共享宿主，1/2/4 Worker、每 Worker 一个 1 vCPU / 128 MiB VM，最小 shell 输出标记后等待 6 s；各档 1 次预热、5 次正式批次。Worker cgroup 各限 512 MiB / 0.5 核，Controller 限 256 MiB / 0.25 核；数量增长时总 CPU 预算增长。制品为固定 debug 二进制，不是 release 性能上限。

就绪从提交 CLI 到 guest 标记，包含 API、记录落盘、调度与 VM 启动；总内存为全部 guest 就绪后的互不重叠 cgroup 总和，含可能计入 file 的 guest RAM。批量速率为 N / 全部就绪等待，不是完成 Agent 的吞吐。Controller 独立 release 微测每档只有一个 ready task，其余为取消历史；计数 N=20，RSS/回放每档一次，不含 Worker 全量对账。

固定制品与测量日期按表注明。失败与校验不通过的样本不计入成功耗时，失败数量单列；既有数据没有事先的宿主干扰剔除规则，所有通过校验的慢样本保留。30 次及更少采样的 P95 仅为观察参考，不给 P99 或稳定尾延迟承诺。

## 实验数据和分析 {#results}

### 轻量 VM 就绪与内存 {#execution}

| 同时存活 VM | 总内存 P50 | 每 VM 就绪 P50 | 观测 P95 | 批量启动速率 |
|---|---:|---:|---:|---:|
| 1 | 92.93 MiB | 3.807 s | 3.893 s | 0.261 台/s |
| 2 | 178.00 MiB | 3.789 s | 3.993 s | 0.525 台/s |
| 4 | 342.96 MiB | 4.383 s | 4.686 s | 0.901 台/s |

![VM scaling](../../assets/benchmarks/cluster-scalability-20261005/execution.svg)

增加 Worker 时的总资源预算也增加，不能将这组就绪时间视为固定预算的吞吐扩展曲线。Kubernetes、Ray、E2B 的同任务有效产出尚未测量，不提供速度排名。

### 保留历史：查询、内存与恢复 {#controller}

| 保留任务记录 | 索引计数 P50 | 全扫描算法参考 P50 | 进程与 ID fixture RSS | 意图/回执日志 | 热回放 |
|---|---:|---:|---:|---:|---:|
| 1,000 | 202.83 ns | 0.068 ms | 12.13 MiB | 1.93 MiB | 0.079 s |
| 10,000 | 286.78 ns | 5.254 ms | 70.59 MiB | 19.33 MiB | 0.388 s |
| 100,000 | 129.40 ns | 20.789 ms | 658.82 MiB | 193.25 MiB | 1.744 s |
| 1,000,000 | 169.47 ns | 134.204 ms | 6,540.58 MiB | 1,932.55 MiB | 16.090 s |

![Controller history costs](../../assets/benchmarks/cluster-scalability-20261005/controller.svg)

全扫描为同数据的算法参考，不是旧版 Controller 吞吐。RSS 含 ID fixture 和分配器留存；回放恢复控制记录，不包括跨主机 Worker 对账。较大规模的部署还需评估有界历史保留与完整恢复时间。

### 数据范围与来源 {#limits}

 ·  ·  ·  · [协议与复现](../design/cluster-performance-analysis.md)
