# 启动一个可用环境要等多久？

## 主要结论 {#conclusions}

**2026-10-11 重启后的独立条件复测中，Ready 中位数为 pVisor host 33.52 ms、staged 50.59 ms、VM 130.71 ms、Docker 105.03 ms、Firecracker MMIO 287.13 ms、QEMU 663.25 ms 和 microvm 260.10 ms。Ready 与正常 Exit 来自同次启动的 60 个样本；当前制品的数字应按这组表格读取，历史随机配对的排名和区间不沿用到新批次。**

以下选型摘要保留历史批次的适用范围。

| 需求 | 选型含义 |
| --- | --- |
| 频繁执行短命令 | host 和 staged 的启动等待为十几到几十毫秒 |
| 频繁创建一次性 VM | pVisor VM 的首条命令等待约 0.1 秒 |
| 完整 Agent 工具任务 | 结合工具与修复任务结果选择执行模式 |

<!-- measurement-status:start -->

本页新增实验与历史数据分开发布；各表适用的计时范围和输入见实验设计与来源，不合并不同批次。

<!-- measurement-status:end -->

## Motivation {#motivation}

频繁创建一次性环境时，首条命令的等待会累积。选择 host、staged、容器或 VM，需要知道各模式的启动等待，以及隔离需求带来的成本。短任务还要预算退出和工具执行。

## 实验设计 {#interpretation}

历史 Ready / Exit 对照使用 Linux x86_64 / KVM，AMD Ryzen 7 9700X，Fedora 宿主。对照为 native、pVisor host、pVisor staged、pVisor VM、Docker rootless / overlay2、Firecracker PCI、QEMU q35 和 QEMU microvm。进程固定到 CPU 0,1，VM 均为 2 vCPU / 128 MiB；进程和容器不设内存上限。

Firecracker、QEMU 和 microvm 使用 Fedora 的完整发行版内核，Linux 7.2.8-200.fc44.x86_64。pVisor 的虚拟化基于 libkrun，使用该项目配套 libkrunfw 的定制内核，Linux 6.12.109。

使用已准备的用户态环境和相同的 `/bin/sh` 输出探针，每次执行使用新环境与私有工作区，VM 每次重新启动，不使用快照或环境池。热页缓存，每组 3 次预热、60 次正式样本，组内随机交替执行。环境准备和输入复制不计入启动时间。内核、设备和存储路径不同，结果反映各配置的完整启动成本；这个探针不代表完整发行版开机、冷镜像、Agent CLI 初始化、并发容量或快照恢复。

历史 Ready 截止到校验后的首条 shell 标记；Exit 包含执行和收尾，要求正确输出并正常零退出。Firecracker 在校验标记和结果、确认无 panic 后受控终止，只报告 Ready。失败、panic、输出不匹配或制品改变的样本不计入耗时分布；保留有效慢样本。

历史 hyperfine 对照使用当时每个条件的独立 JSON 模板，通过 semspec bench 执行，统一 3 次预热和 60 次采样。它测量完整命令到正常退出，不测首条 Ready；每次输入准备与结果校验在计时外。各条件独立采样，不合并批次或推导配对改善比例。该批次校验 Run Bundle 中的实际执行模式：本机 host 和 staged 使用 `rootless_process`，VM 使用 `vm`；制品摘要与批次来源独立记录。

## 实验数据和分析 {#results}

### Python：同次启动的 Ready / Exit {#python-ready-exit}

同一进程启动中记录开始、收到完整 `REFERENCE_READY` 行和正常退出三个时刻，固定
3 次预热、60 次正式样本；下面两张表只是同一个命令输出的列投影，不重复采样。
已准备 rootfs、2 vCPU / 128 MiB、热缓存、CPU 0,1 亲和性。Python `perf_counter_ns`
计时不包含解释器启动和输入准备；Ready 包含进程创建、管道读取和观察者调度，
Exit 从同一开始时刻计至观察到正常退出。全部样本还须通过输出、Run Bundle 和输入校验。
新 Firecracker 使用 stock 内核、同一 initrd 和 virtio-MMIO；PCI 配置两次预检在成功 guest 标记后无法正常退出，整批拒绝发布。历史 Firecracker PCI 的 Ready 与新 MMIO 是不同配置，不能相减解释为性能改善。

#### Ready

{{ table("startup-timing.ready.render.json") }}

#### Exit

{{ table("startup-timing.exit.render.json") }}

[同次样本统计 CSV](../../../overrides/assets/benchmarks/data/startup/startup-timing.csv) 与 [来源 JSON](../assets/benchmarks/startup-timing.provenance.json)
记录各条件最新成功的独立批次，标准差采用正式样本的样本标准差。这是 Python 事件时钟数据，独立于
hyperfine 完整命令实验；历史不同制品的数字不与这组样本合并。整表复测使用
`just benchmark startup/timing`；仅复测 VM 使用 `just benchmark startup/timing "pvisor vm"`。
此前 VM 单条件批次独立保留在本地，不混入新样本。

2026-10-11 重启后重新测量全部八个条件，重启前后对照的完整 CSV/JSON 保存在本地，
保留两批各自的完整精度统计与来源摘要。被测 pVisor 二进制 SHA-256 为
`d3f09687cca60b617629c215fa1359c441ab6ac19f8c63b68f1dcdf7614e6b30`，重启前后相同；
用户态输入、被测命令、计时和统计函数也相同。框架期间新增失败处理与来源字段，均位于计时外。
VM Ready 中位数从 179.22 ms 降至 130.71 ms（描述性差值为 -27.07%），Exit 从
297.79 ms 降至 243.53 ms。Host Ready 中位数从 37.45 ms 降至 33.52 ms，但标准差
从 1.38 ms 升至 29.70 ms，后 17 个正式样本明显变慢；全部有效慢样本保留。
两批独立采样，且仍使用热缓存，这组比较不能单独建立重启的因果效果。
历史约 101 ms 的 VM Ready 使用不同二进制及固件，剩余差距需要固定制品的对照才能定位。

### 历史配对：启动与退出耗时 {#reference-startup}

<a id="reference-exit"></a>
<a id="legacy-startup"></a>

历史对照单位为 ms，每组 N=60，正式失败均为 0。进程/容器与 VM 分别采样，各行保留自己的统计值；四种 VM 在同组内随机交替执行。P95 仅供分布观察，不是稳定尾延迟保证。

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

<!-- startup-hyperfine:start -->

### 历史 Python：仅 Ready 数据 {#python-ready}

这组实验使用已准备好的 rootfs、2 vCPU / 128 MiB、热缓存和 CPU 0,1 亲和性。
固定 3 次预热、60 次正式样本；Python `perf_counter_ns` 从 Popen 前计到收到完整的
`REFERENCE_READY` 行，包含进程创建、管道读取和观察者调度开销。解释器启动、输入准备、
后续退出和清理不计入 Ready。停表后仍要求正确输出、正常零退出和通过 Run Bundle 校验。

|  | N | Ready 中位数 ms | Ready 均值 ms | Ready 标准差 ms | Ready 最小 ms | Ready 最大 ms |
| --- | --- | --- | --- | --- | --- | --- |
| pVisor VM | 60 | 132.95 | 132.51 | 5.09 | 123.00 | 147.53 |

该条件的 Ready 完整统计 CSV 与来源 JSON 保存在本地，
保留最新成功批次；标准差是正式样本的样本标准差。这是 Python 时钟数据，不是 hyperfine
完整退出耗时。历史 Ready 使用不同二进制制品，不能只根据计时边界解释两者数值差异。
当前 Ready / Exit 实验使用 `just benchmark startup/timing "pvisor vm"`；本节保留旧的 Ready-only 批次。

### 历史 Hyperfine：直接启动到正常退出 {#hyperfine-startup}

每行取该条件最新成功批次，独立采样，不合并批次。3 次预热、60 次正式样本；中位数、均值和标准差来自同一份 hyperfine 原始导出，单位为 ms。Python 准备和校验、semspec 处理及外部 CPU 亲和性设置均在计时外。

|  | N | 中位数 ms | 均值 ms | 标准差 ms |
| --- | --- | --- | --- | --- |
| Native | 60 | 0.42 | 0.42 | 0.02 |
| pVisor host | 60 | 208.79 | 217.94 | 34.13 |
| pVisor staged | 60 | 153.85 | 154.58 | 5.88 |
| pVisor VM | 60 | 278.83 | 305.77 | 63.45 |
| Docker | 60 | 163.92 | 168.87 | 21.38 |
| Firecracker | — | — | — | — |
| QEMU | 60 | 722.08 | 711.24 | 26.75 |
| QEMU microvm | 60 | 339.47 | 355.08 | 86.50 |

空白条件尚无通过校验的新批次。它测量命令到正常退出，不等于首条输出 Ready；各行仅并列展示水平，不提供随机配对改善比例或置信区间。实际命令、极值及逐行来源保存在本地完整 CSV/JSON 中。

<details markdown="1">
<summary>历史完整命令结果（包含包装器）</summary>

|  | N | 中位数 ms | 均值 ms | 标准差 ms |
| --- | --- | --- | --- | --- |
| Native | 60 | 2.13 | 2.17 | 0.23 |
| pVisor host | 60 | 165.12 | 315.84 | 259.74 |
| pVisor staged | — | — | — | — |
| pVisor VM | — | — | — | — |
| Docker | 60 | 167.27 | 381.08 | 509.32 |
| Firecracker | — | — | — | — |
| QEMU | 60 | 748.41 | 896.73 | 319.03 |
| QEMU microvm | 60 | 353.76 | 451.13 | 219.93 |

本地历史 CSV 的计时包含外层 shell、taskset 和 timeout，保留原始口径。它与直接命令是不同批次、不同范围，不能通过相减估计包装器成本或解释为产品性能改善。

</details>

<!-- startup-hyperfine:end -->

### 来源与复现 {#run}

启动统计、VM 差异与 95% 区间、源码与制品来源的完整 CSV/JSON 和原始记录保存在对应本地批次或文档数据归档。当前 Ready / Exit 复现见[启动实验手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark.new/startup/README.md)。

[比较方法](methodology.md) · [历史复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#explicit-firecracker-kernel-controls)
