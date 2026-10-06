# 如何在 pVisor、Firecracker 和 QEMU 之间选择？

## 主要结论 {#conclusions}

**使用 `/boot` 原版内核的启动对照中，pVisor VM 为 100.91 ms，快于 Firecracker 的 285.04 ms 和 QEMU microvm 的 321.56 ms；定制参考内核的独立工具批次中，pVisor VM 的修复和文件密集任务较慢。按实际内核和完整任务选型。**

| 需求 | 选型含义 |
| --- | --- |
| 本机执行并保留改动 | 评估 host/staged 与完整审查流程 |
| 独立 guest 内核 | 同时预算启动和 VM 工具等待 |
| 并发或闲置环境 | 需要固定资源下的吞吐与物理内存实测 |

## Motivation {#motivation}

独立 guest 内核、OCI 工作流和暂存审查是不同需求。比较完整任务可以避免仅凭启动速度选择执行环境。

## 实验设计 {#interpretation}

Linux x86_64，AMD Ryzen 7 9700X，Fedora 7.2.8-200.fc44.x86_64。执行进程树与专用 Docker daemon 固定到 CPU 0,1；VM 为 2 vCPU，shell 探针配置 128 MiB，工具任务配置 16 GiB。原生/Docker 未限制内存，因此是 CPU 控制的任务对照，不能推导相同内存预算下的容量。host/staged 使用 rootless_process。

同一套离线工具与固定输入，每次新建工作区；热缓存、3 次预热、每格 60 次正式采样，固定种子随机交替执行。环境准备、构建、镜像导入和输入重置不计时；启动和退出计入完整任务。Docker Engine 29.7.2 使用专用 rootless **overlay2** daemon、经典镜像存储和可写 bind mount；Firecracker 1.13.1 PCI 不使用 jailer，QEMU 10.2.2 分别使用 q35/microvm 与私有 ext4。pVisor VM 使用 virtio-fs 和自己的固件。内核、存储和暂存语义不同，结果是这些配置下的任务成本，不是纯 VMM 或安全排名。

复用启动、文件系统和修复三个独立注册负载，不合并其样本。镜像与工具已准备，输出、退出和暂存均须校验；这不是相同 OS 或安全加固程度的排名。

Stock 启动为 2026-10-07 的独立批次：Firecracker 使用原版 Fedora 7.2.8 提取的 ELF，QEMU 使用同一 `/boot/vmlinuz`，共用最小 initrd 和用户态；microvm 保留 RTC。pVisor 使用专用 Linux 6.12.109 固件。Firecracker 校验输出后受控终止，只报告 Ready；正常关机未测。完整方法与来源见[启动对照](startup.md)。

## 实验数据和分析 {#results}

两组独立实验每格均为 60/60 有效、正式失败 0。保留所有有效慢样本，没有按耗时剔除；原始报告、二进制、输入与源码摘要保存在忽略的 `.data/`。

### 原版发行版内核启动 {#stock-startup}

2026-10-07，同批 2 vCPU / 128 MiB，Ready P50，单位 ms：

| Runtime | Ready P50 ms |
| --- | --- |
| pVisor VM | 100.91 |
| Firecracker PCI / stock | 285.04 |
| QEMU q35 / stock | 702.98 |
| QEMU microvm / stock | 321.56 |

在这些完整配置下，pVisor 的首条命令等待更短；不能由此推导工具任务、相同内核的 VMM 成本或容量优势。差异的 95% 区间见[启动](startup.md)。

### 完整任务对照 {#reference-comparison}

2026-10-06 的独立定制参考内核批次；Firecracker 为 legacy reference/unknown，均非 `/boot` stock 对照。Stock 内核的工具任务尚未测量，下表不与上方合并。

| Runtime | Ready P50 ms | Repair completion P50 s | Seven-tool completion P50 s |
| --- | --- | --- | --- |
| pVisor VM | 100.73 | 3.25 | 4.27 |
| Firecracker PCI | 72.61 | 2.20 | 2.29 |
| QEMU q35 | 213.07 | 1.46 | 1.49 |
| QEMU microvm | 86.57 | 1.40 | 1.50 |

分项、计时边界与差异置信区间见[启动](startup.md)、[文件系统](filesystem.md)和[修复任务](agent-tasks.md)。

<a id="full-ubuntu"></a>
gVisor、Kata 与完整 Ubuntu 没有当前同条件实测；不提供排名。

### 数据下载与复现 {#run}

[整理后的统计 CSV](compare-runtimes.csv) · [全部运行时统计](runtime-summary.csv) · [差异与 95% 置信区间](runtime-comparisons.csv) · [源码与制品来源](runtime-provenance.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
