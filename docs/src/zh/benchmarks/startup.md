# 启动一个可用环境要等多久？

## 主要结论 {#conclusions}

**在 Linux/KVM、已准备用户态、2 vCPU / 128 MiB 条件下，pVisor VM 首条输出为 100.91 ms，快于使用 `/boot` 原版发行版内核的 Firecracker（285.04 ms）、QEMU microvm（321.56 ms）和 QEMU q35（702.98 ms）。**

| 需求 | 选型含义 |
| --- | --- |
| 频繁创建一次性 VM | pVisor 的专用固件与启动路径减少所测首条命令等待 |
| 使用发行版原版内核 | 对照 stock Firecracker/QEMU 的实际就绪成本 |
| 完整 Agent 工具任务 | 另看工具与修复任务，启动优势不能代替完整任务性能 |

## Motivation {#motivation}

频繁创建一次性环境时，首条命令的等待会累积。发行版内核与专用固件的初始化成本不同，选型需要知道真实部署配置的等待。短任务还要预算退出和工具执行。

## 实验设计 {#interpretation}

Linux x86_64 / KVM，AMD Ryzen 7 9700X，宿主 Fedora 7.2.8-200.fc44.x86_64。四组进程固定到 CPU 0,1，VM 均为 2 vCPU / 128 MiB。使用相同已准备 rootfs 和 `/bin/sh` 输出探针；每次新建 VM 与私有工作区/磁盘，不使用快照或环境池。热页缓存、每组 3 次预热、60 次正式样本，固定种子 20261007 随机交替执行。环境准备、内核提取、initrd 制作与输入复制在计时之外；`prepare_ms` 另存证据。

Firecracker 1.13.1 PCI 使用 `/boot/vmlinuz-7.2.8-200.fc44.x86_64` 提取的原版 ELF；QEMU 10.2.2 q35/microvm 使用该文件的原版 vmlinuz 字节。匹配 config 和 RPM payload 摘要，冻结并前后校验内核、提取器、config、initrd 与来源元数据，不重编内核。三组共用最小 initrd，加载发行版原版 `virtio_mmio` 模块后进入共同 ext4 用户态。microvm 关闭 ACPI、option ROM、PIT、PIC，保留 RTC 供 stock 驱动初始化。

pVisor 使用固定 GNU release CLI、libkrunfw 5.6.2 专用 Linux 6.12.109 固件与 staged virtio-fs。内核版本、设备、PID1 和存储语义不同，比较的是各配置的完整启动路径，不能将差值全部归因于 VMM 或某项固件优化。这个 shell 探针不代表完整 Fedora/systemd/SSH、冷镜像、真实 Agent CLI 初始化、并发容量或快照恢复。

Ready 截止到校验后的首条 shell 标记。pVisor/QEMU 必须输出正确结果并正常零退出，Exit 包含收尾；Firecracker 使用 ready-only，在唯一、有序的 Ready/Result/Exit0 且无 panic 后受控 SIGTERM，因此只报告 Ready，不报告正常关机耗时。失败、panic、输出不匹配或制品改变均不能计入；保留有效慢样本，不按速度剔除。

## 实验数据和分析 {#results}

### 发行版原版内核对照 {#reference-startup}

<a id="reference-exit"></a>

测于 2026-10-07，同批 240/240 有效样本，正式失败 0。单位为 ms，每组 N=60；P95 仅供分布观察，不是稳定尾延迟保证。四组均未触发预定的分离分布规则。

| Runtime | Valid / failed | Ready P50 ms | Ready P95 ms | Exit P50 ms | Exit P95 ms |
| --- | --- | --- | --- | --- | --- |
| pVisor VM | 60 / 0 | 100.91 | 151.27 | 141.52 | 176.59 |
| Firecracker PCI / stock | 60 / 0 | 285.04 | 291.88 | — | — |
| QEMU q35 / stock | 60 / 0 | 702.98 | 789.36 | 725.55 | 818.85 |
| QEMU microvm / stock | 60 / 0 | 321.56 | 404.39 | 347.99 | 428.27 |

Firecracker 的空白 Exit 表示未测正常关机，不能与其他行的 Exit 排名。

| 对照 | pVisor 减少的等待 ms | 差异的 95% 区间 ms | 等待减少 | 对照耗时 / pVisor 耗时 |
| --- | --- | --- | --- | --- |
| Firecracker PCI / stock | 184.12 | 181.60–185.84 | 64.6% | 2.82× |
| QEMU q35 / stock | 602.06 | 582.17–635.99 | 85.6% | 6.97× |
| QEMU microvm / stock | 220.64 | 195.91–251.74 | 68.6% | 3.19× |

差异为两组中位数之差；对匹配轮次做 5,000 次配对 bootstrap 得到 95% 区间。三个区间均不含零，支持此配置下 pVisor 启动更快。频繁创建短命 VM 时，每次首条命令少等待上表所示时间。专用固件和精简初始化提供一条较短的启动路径；这个对照不能分别量化内核、initrd、文件系统和 VMM 的贡献。

### 定制参考内核的独立批次 {#legacy-startup}

下表为 2026-10-06 的独立批次，每组 N=60，正式失败 0。Firecracker 为 legacy reference/unknown，QEMU 为定制参考内核，均不是 `/boot` stock 内核。用户态和制品不同；不与上方合并，也不据此计算跨批次百分比。该配置下 pVisor VM 慢于轻量参考 Firecracker/microvm，说明优势取决于内核及完整配置。

| Runtime | Valid / failed | Ready P50 ms | Ready P95 ms | Exit P50 ms | Exit P95 ms |
| --- | --- | --- | --- | --- | --- |
| Native | 60 / 0 | 1.28 | 1.71 | 1.35 | 1.82 |
| pVisor host | 60 / 0 | 12.72 | 14.18 | 24.03 | 24.71 |
| pVisor staged | 60 / 0 | 25.13 | 26.85 | 33.84 | 44.55 |
| pVisor VM | 60 / 0 | 100.73 | 109.17 | 136.65 | 156.89 |
| Docker rootless / overlay2 | 60 / 0 | 74.48 | 87.13 | 98.29 | 110.22 |
| Firecracker PCI / legacy reference | 60 / 0 | 72.61 | 74.53 | 95.98 | 104.27 |
| QEMU q35 / custom reference | 60 / 0 | 213.07 | 217.77 | 237.52 | 245.37 |
| QEMU microvm / custom reference | 60 / 0 | 86.57 | 95.30 | 109.52 | 119.96 |

### 完整发行版与 macOS {#full-ubuntu}

<a id="macos"></a>

完整 Ubuntu 冷启动和 Apple Silicon/HVF 尚无同条件新对照；轻量 shell 探针不能代替实际部署测量。

### 数据下载与复现 {#run}

[Stock 启动统计](startup-stock.csv) · [Stock 差异与 95% 区间](startup-stock-comparisons.csv) · [Stock 源码与制品来源](startup-stock-provenance.csv) · [定制参考批次](startup.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#explicit-firecracker-kernel-controls)。二进制、完整输入和原始日志保留在本地忽略的 `.data/`；来源 CSV 保留批次、CPU/RAM 配置、报告和制品摘要。
