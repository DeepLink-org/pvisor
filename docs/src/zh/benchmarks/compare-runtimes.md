# 如何在 pVisor、Firecracker、QEMU 与隔离运行时之间选择？

## 主要结论 {#conclusions}

**pVisor VM 启动处于轻量 VM 量级，但所测修复与七项工具任务比 Firecracker、QEMU 等待更长。应结合完整执行与保留改动成本选型；gVisor/Kata 没有同条件排名。**

| 需求 | 选型含义 |
|---|---|
| 只需轻量 VM 执行 | 同时比较 Firecracker 和 QEMU microvm |
| 需要统一 stage/apply | 评估 pVisor 的执行与合入总成本 |
| 需要 gVisor 或 Kata | 没有同条件本机性能排名 |

## Motivation {#motivation}

需要独立 guest kernel、OCI 工作流或系统调用隔离时，执行边界是选型的一部分。还需要区分 VMM 启动、操作系统启动、工具执行和 pVisor 的暂存/记录成本。

## 实验设计 {#interpretation}

共享 Linux/x86_64 宿主，AMD Ryzen 7 9700X，Fedora 内核 7.2.8-200.fc44.x86_64。启动进程树及专用 Docker daemon 固定到宿主 CPU 0,1；guest 为 2 vCPU。host/staged 使用 rootless_process。Shell VM 为 128 MiB，工具 VM 为 16 GiB。原生/Docker 不限内存：控制 CPU 与 guest 配置内存，不是相同资源限制的对照。工具与输入已准备，每次新建工作区、热缓存，3 次预热、60 次正式采样；按固定种子随机交错后端。构建、下载及输入复制不计时。

Docker Engine 29.7.2 使用专用 rootless VFS daemon 与可写 bind mount，结果不代表 overlay2 或 Docker Desktop。Firecracker 1.13.1 PCI 不使用 jailer；QEMU 10.2.2 分别使用 q35/microvm、私有 ext4。pVisor VM 使用 virtio-fs 和不同内核。内核、存储、设备及暂存语义均有差异，不能把差距单独归因于 VMM 或 FUSE。

输出、暂存与退出必须校验。对照已准备环境，不是相同 OS 或安全加固程度的排名。

## 实验数据和分析 {#results}

### 轻量 VM 等待 {#reference-comparison}

启动/文件系统：2026-10-05；修复：2026-10-06。各独立负载/后端 N=60、失败 0、3 次预热。单位与计时边界见表头。通常为 P50，分离簇展示各簇中位数与数量；不跨批次合并分布。

| Runtime | Valid / failed | Ready P50 ms | Repair completion P50 s | Seven-tool completion P50 s |
|---|---|---|---|---|
| pVisor VM | 60 / 0 | 99.76 | 3.25 | 6.66 |
| Firecracker PCI | 60 / 0 | 74.74 | 2.06 | 2.16 |
| QEMU q35 | 60 / 0 | 213.89 | 1.33 | 2.44 |
| QEMU microvm | 60 / 0 | 86.60 | 1.27 | 2.45 |

复用环境摊薄启动后，工具时间更影响反馈速度。内核与文件系统路径有差异，不能把成本唯一归因于 libkrun。[启动](startup.md)、[文件系统](filesystem.md)与[修复/CLI 检查](agent-tasks.md)给出详细数据。

### 执行范围

| 运行时 | 执行范围 | 测量状态 |
|---|---|---|
| pVisor / libkrun | 集成 guest VM + stage/apply | 本机对照如下 |
| Firecracker / QEMU | 独立 VMM CLI | 对照测量，不是 pVisor 集成后端 |
| gVisor | 应用内核 / runsc | 同条件性能未测 |
| Kata | VM 支持容器工作流 | 同条件性能未测 |

官方说明：[gVisor](https://gvisor.dev/docs/)、[Firecracker](https://firecracker-microvm.github.io/)、[Kata](https://katacontainers.io/)。pVisor 已验收后端见[执行器](../guides/executors/index.md)。

### 完整 Ubuntu 部署 {#full-ubuntu}

独立的 2026-10-04 数据，两核。启动：2 GiB，pVisor/Firecracker N=30、QEMU N=10；修复：16 GiB、N=10。统计 P50，OS 初始化/工具/存储不同。

| Deployment | Ready P50 ms | Repair result P50 s |
|---|---|---|
| pVisor VM / host tools | 109.69 | 4.61 |
| Firecracker / Ubuntu | 5644.11 | 8.51 |
| QEMU q35 / Ubuntu | 5428.90 | 8.12 |
| QEMU microvm / Ubuntu | 7666.69 | 10.33 |

此表描述部署等待，不是纯 VMM 排名。

### 数据下载与复现 {#run}

[整理后的表格 CSV](compare-runtimes.csv) · [运行时统计](runtime-summary.csv) · [来源与制品](runtime-provenance.csv) · [证据来源摘要](evidence-sources.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
