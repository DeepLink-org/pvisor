# VM 内存、回收与快照性能

> CLI 更新：完整快照数字来自历史制品。独立 `pvisor snapshot` 已删除；当前入口与能力范围见[CLI 参考](../../reference/cli.md)。

## 主要结论 {#conclusions}

**pVisor 能减少冷 guest 页的驻留，但恢复访问需要付出时间和 CPU；目前没有证据证明比 Docker 或其他 VM 更省净物理内存。** macOS 的重复数据负载中，冷页 RAM 代理下降约 **60–89%**，不等于全机物理内存节约。2 GiB 配置的首次 64 MiB 读取从约 **20 ms** 升到 **164 ms**，测量末期 footprint 反而从约 **18 MiB** 升到 **52 MiB**。

Linux raw offload 在 2 vCPU / 256 MiB 配置下约 **23 ms**，恢复后首次完整读取约 **109 ms**。压缩占用更小，但首次读取约 **699 ms**。完整快照的 raw 保存/恢复约 **0.71/0.93 s**，compressed 约 **1.24/1.92 s**。适合可接受恢复等待的闲置环境；高频活跃任务需要权衡。

## Motivation {#motivation}

多个等待模型响应的 VM 可能保留大量冷页。用户需要知道能减少哪部分驻留、占用多少 backing，以及下一次工具访问和恢复要等多久。只看回收比例无法判断容量收益。

## 实验设计 {#experiment-design}

macOS/HVF 在 Apple M4、24 GiB RAM 上比较启用/关闭共享冷页池，每组两个 VM，写入重复可压缩的 64 MiB 数据；参数矩阵 40 次、2 GiB 延长观察 4 次。RAM 代理包括 guest 驻留和池/在途数据，不是净物理内存；同时记录 footprint、CPU 和恢复访问。Linux 共享池不使用这些 macOS 数字。

Linux/KVM 的生命周期与完整快照独立测量：固定 64 MiB guest 数据、配置 RAM/vCPU，记录 pause/resume/offload、heartbeat 和首次读取；完整快照每格 10 次、每次两次 fork，以配对中位数作为一个样本。正确性核对源退出、删除源 rootfs、内存摘要、计数、打开文件、目录句柄和 fork 写入隔离。Docker/Firecracker/Kata 的同工作集内存对照未测。

## 实验数据和分析 {#experiment-data}

### macOS / HVF: 冷页驻留与恢复 {#measurements}

| RAM / VM | 全部就绪后观察窗口 | RAM proxy: off → on | 约下降 |
|---|---|---|---|
| 256 MiB | 18–33 s | 231 → 27 MiB | 89% |
| 512 MiB | 18–33 s | 243 → 61 MiB | 75% |
| 2 GiB | 60–90 s | 309 → 124 MiB | 60% |

2 GiB 的首次读取变慢约 **7.3–8.8 倍**，额外采样 CPU 约 **15.9–17.2 s**。压缩池本身也占空间；guest 驻留下降而 footprint 上升，因此不能据此承诺更高 Agent 密度。随机不可压缩、反复访问和真实大仓库的收益需要各自测量。

### Linux / KVM: offload {#linux-lifecycle}

| MiB / vCPU / backing | pause | resume | offload | offload resume | heartbeat |
|---|---:|---:|---:|---:|---:|
| 256 / 1 / raw | 0.24 / 0.30 | 0.26 / 0.34 | 20.43 / 21.76 | 0.28 / 0.37 | 49.57 / 51.92 |
| 256 / 2 / raw | 0.24 / 0.31 | 0.29 / 0.35 | 22.98 / 24.90 | 0.29 / 0.40 | 46.79 / 49.83 |
| 256 / 4 / raw | 0.25 / 0.36 | 0.27 / 0.35 | 22.93 / 25.60 | 0.29 / 0.35 | 43.61 / 46.87 |
| 512 / 2 / raw | 0.24 / 0.31 | 0.28 / 0.35 | 26.44 / 29.35 | 0.28 / 0.39 | 44.63 / 46.74 |
| 2048 / 2 / raw | 0.21 / 0.27 | 0.23 / 0.31 | 23.86 / 28.80 | 0.26 / 0.32 | 40.39 / 46.71 |
| 256 / 2 / compressed | 0.21 / 0.33 | 0.25 / 0.40 | 66.93 / 505.43 | 0.24 / 0.31 | 175.09 / 203.24 |

| MiB / vCPU / backing | 64 MiB read before offload (ms) | First complete read after offload (ms) | Backing allocation (MiB, P50) |
|---|---:|---:|---:|
| 256 / 1 / raw | 32.12 / 32.31 | 109.02 / 111.56 | 200.80 |
| 256 / 2 / raw | 32.12 / 32.37 | 109.49 / 110.84 | 202.23 |
| 256 / 4 / raw | 32.10 / 32.93 | 114.85 / 117.64 | 203.99 |
| 512 / 2 / raw | 32.15 / 32.48 | 115.15 / 117.42 | 206.30 |
| 2048 / 2 / raw | 25.03 / 31.91 | 86.12 / 109.30 | 235.91 |
| 256 / 2 / compressed | 25.06 / 27.64 | 698.71 / 754.50 | 23.96 |

表内时间为 P50/P95 ms。offload 会暂停、写回并请求回收 RAM；调用完成后仍需 resume。控制调用返回与 guest heartbeat 推进不同。2 GiB 配置仍只写同一段 64 MiB，不能当成写回 2 GiB 的吞吐。压缩 backing 较小，代价是更长的首次访问与尾延迟。

### Linux / KVM: 完整快照与 fork {#linux-snapshot}

| RAM storage | save (ms, P50 / P95) | restore heartbeat (ms, P50 / P95) | Published allocation (MiB, P50 / P95) |
|---|---:|---:|---:|
| raw | 712.11 / 819.75 | 932.88 / 1034.90 | 296.99 / 296.99 |
| compressed | 1236.62 / 1343.63 | 1920.23 / 1947.43 | 16.48 / 16.58 |

save 包含冻结、CPU/设备/RAM、文件复制、校验、持久发布和源退出；恢复计到新 guest heartbeat，包含解码和私有副本。Published allocation 是已发布文件的磁盘分配，排除活动 fork、临时制品与进程内存。这里的高压缩率来自重复数据，不能推广到任意 Agent 工作集。

### 数据来源与复现 {#evidence}

[macOS JSON](assets/decision.json) · [macOS CSV](assets/decision.csv) · [Compatibility](assets/compatibility.json) · [Linux lifecycle](../../../assets/benchmarks/vm-lifecycle-20261003/lifecycle.tsv) · [Linux snapshots](../../../assets/benchmarks/vm-lifecycle-20261003/snapshot.tsv) · [技术分析与复现](../../design/vm-memory-performance-analysis.md)
