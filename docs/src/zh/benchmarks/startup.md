# 启动一个可用环境要等多久？

## 主要结论 {#conclusions}

**已准备环境下，pVisor host 和 staged 的首条输出分别为 12.72 ms 和 25.13 ms；在 Linux/KVM、2 vCPU / 128 MiB 条件下，pVisor VM 为 100.91 ms，快于使用 Fedora 完整内核的 Firecracker（285.04 ms）、QEMU（702.98 ms）和 microvm（321.56 ms）。**

| 需求 | 选型含义 |
| --- | --- |
| 频繁执行短命令 | host 和 staged 的启动等待为十几到几十毫秒 |
| 频繁创建一次性 VM | pVisor VM 的首条命令等待约 0.1 秒 |
| 完整 Agent 工具任务 | 结合工具与修复任务结果选择执行模式 |

## Motivation {#motivation}

频繁创建一次性环境时，首条命令的等待会累积。选择 host、staged、容器或 VM，需要知道各模式的启动等待，以及隔离需求带来的成本。短任务还要预算退出和工具执行。

## 实验设计 {#interpretation}

Linux x86_64 / KVM，AMD Ryzen 7 9700X，Fedora 宿主。对照为 native、pVisor host、pVisor staged、pVisor VM、Docker rootless / overlay2、Firecracker PCI、QEMU q35 和 QEMU microvm。进程固定到 CPU 0,1，VM 均为 2 vCPU / 128 MiB；进程和容器不设内存上限。

Firecracker、QEMU 和 microvm 使用 Fedora 的完整发行版内核，Linux 7.2.8-200.fc44.x86_64。pVisor 的虚拟化基于 libkrun，使用该项目配套 libkrunfw 的定制内核，Linux 6.12.109。

使用已准备的用户态环境和相同的 `/bin/sh` 输出探针，每次执行使用新环境与私有工作区，VM 每次重新启动，不使用快照或环境池。热页缓存，每组 3 次预热、60 次正式样本，组内随机交替执行。环境准备和输入复制不计入启动时间。内核、设备和存储路径不同，结果反映各配置的完整启动成本；这个探针不代表完整发行版开机、冷镜像、Agent CLI 初始化、并发容量或快照恢复。

Ready 截止到校验后的首条 shell 标记；Exit 包含执行和收尾，要求正确输出并正常零退出。Firecracker 在校验标记和结果、确认无 panic 后受控终止，只报告 Ready。失败、panic、输出不匹配或制品改变的样本不计入耗时分布；保留有效慢样本。

## 实验数据和分析 {#results}

### 启动与退出耗时 {#reference-startup}

<a id="reference-exit"></a>
<a id="legacy-startup"></a>

单位为 ms，每组 N=60，正式失败均为 0。进程/容器与 VM 分别采样，各行保留自己的统计值；四种 VM 在同组内随机交替执行。P95 仅供分布观察，不是稳定尾延迟保证。

| Runtime | Valid / failed | Ready P50 ms | Ready P95 ms | Exit P50 ms | Exit P95 ms |
| --- | --- | --- | --- | --- | --- |
| Native | 60 / 0 | 1.28 | 1.71 | 1.35 | 1.82 |
| pVisor host | 60 / 0 | 12.72 | 14.18 | 24.03 | 24.71 |
| pVisor staged | 60 / 0 | 25.13 | 26.85 | 33.84 | 44.55 |
| pVisor VM | 60 / 0 | 100.91 | 151.27 | 141.52 | 176.59 |
| Docker | 60 / 0 | 74.48 | 87.13 | 98.29 | 110.22 |
| Firecracker | 60 / 0 | 285.04 | 291.88 | — | — |
| QEMU | 60 / 0 | 702.98 | 789.36 | 725.55 | 818.85 |
| microvm | 60 / 0 | 321.56 | 404.39 | 347.99 | 428.27 |

host 和 staged 的等待为十几到几十毫秒，适合频繁启动的短命令。VM 需要额外的内核和设备初始化；选择隔离方式时，应结合完整任务耗时，而不是只看启动。Firecracker 的空白 Exit 表示未测正常关机。

| VM 对照 | pVisor 减少的等待 ms | 差异的 95% 区间 ms | 等待减少 | 对照耗时 / pVisor 耗时 |
| --- | --- | --- | --- | --- |
| Firecracker | 184.12 | 181.60–185.84 | 64.6% | 2.82× |
| QEMU | 602.06 | 582.17–635.99 | 85.6% | 6.97× |
| microvm | 220.64 | 195.91–251.74 | 68.6% | 3.19× |

差异为两组中位数之差；对匹配轮次做 5,000 次配对 bootstrap 得到 95% 区间。三个区间均不含零，支持这些配置下 pVisor VM 启动更快。专用固件和精简初始化提供较短的启动路径；这个对照不能分别量化内核、文件系统和 VMM 的贡献。

冷镜像的客户端等待与内容下载见独立的[镜像按需启动](lazy-image-startup.md)实验；镜像服务准备成本单列，样本不与这里的预准备环境合并。

### 数据下载与复现 {#run}

[启动统计](startup.csv) · [VM 差异与 95% 区间](startup-stock-comparisons.csv) · [VM 源码与制品来源](startup-stock-provenance.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#explicit-firecracker-kernel-controls)。二进制、完整输入和原始日志保留在本地忽略的 `.data/`；CSV 保留统计来源与样本数，来源记录保留 CPU/RAM 配置、报告和制品摘要。
