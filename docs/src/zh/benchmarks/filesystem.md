# 环境多久 Ready，连续三轮工具要等多久？

## 主要结论 {#conclusions}

**两核、宿主热缓存下，Python 负载 Ready 中位数为：pVisor host 201.19 ms、staged 209.56 ms、VM 749.15 ms，Docker 450.94 ms、Firecracker 854.79 ms。VM 七项操作计时合计从首轮 3323.01 ms 降至第三轮 1758.28 ms；启动和工具执行分别预算。**

| 场景 | 选型含义 |
| --- | --- |
| 新建已准备环境 | 比较首条负载 Ready，不将退出耗时计入启动 |
| 同一环境连续调用工具 | 参考第三轮，同时保留首轮成本 |
| 高频只读 open 或连续小写 | 参考专门场景；写合并是显式可选配置 |

<!-- measurement-status:start -->

九个配置各 5 个独立环境，共 45 个；每个环境内执行 3 轮、共 1,215 项操作，全部校验通过。快速测量不启用外部干扰门禁，历史数据独立保留。

<!-- measurement-status:end -->

## Motivation {#motivation}

频繁创建环境时，首先关心命令何时可以开始。长驻 Agent 则会反复遍历、搜索、编译和安装依赖，需要分别预算首次访问与后续调用。重复 open 和小块流写用于判断特定调用形状是否适合现有优化。

## 实验设计 {#interpretation}

八个后端共用已准备的离线工具和固定输入；另列显式 `PVISOR_VM_FS_WRITEBACK=1` 的 VM。默认 VM 使用写合并关闭配置，guest cache 均开启，DAX 关闭。普通 CLI rootfs/workspace 为 Mutable；目录镜像、自适应元数据预取和不可变 rootfs 数据页保留不因重复执行而获得启用条件。

Linux x86_64 / Ryzen 7 9700X / Fedora 7.2.8-200.fc44.x86_64；Docker 29.8.2 rootless overlay2，Firecracker PCI 1.13.1，QEMU 10.2.2。Firecracker/QEMU 使用独立定制的参考内核，并非 stock 内核。整父组 CPU 0,1、quota 两核、16 GiB、swap 0，各 VM 2 vCPU/16 GiB；cpuset 未委托，使用继承 affinity。宿主工作区/stage 为 Btrfs，参考 VM 为 ext4；私有 /tmp 是 2 GiB tmpfs，工具保留执行器的默认临时存储。

每配置 5 个新环境，无额外预热，seed 20261016 同轮随机交替。每个环境的七工具与两个专门场景依次执行三轮，VM/容器、工作区、任务缓存保持存活。首轮 Cargo/npm 从空输出开始，后两轮复用编译产物和安装结果；写入任务后两轮覆盖相同的 256 文件。Python 统一固定为 3.14.7 且不读写 bytecode。三轮不是十五个独立样本；宿主缓存较热，不等于每轮 guest 元数据期限都未到期。

启动计时从宿主创建执行器到共同 Python 负载输出唯一 `REFERENCE_READY`，包含解释器和共同模块导入，不包含工具任务、退出或收尾。每项任务使用内部单调时钟，不包含 Python 子进程启动和 VM 退出；写入字节读回校验在操作计时外。正常退出、输出、缓存继承、Run Bundle、暂存隔离和完整字节仍须通过；失败单列，所有有效慢样本保留。无安静期、外部干扰筛选和新的全量输入清单扫描；N=5 不提供稳定尾延迟结论，也不证明整个 Agent 的加速。

## 实验数据和分析 {#results}

### Python 负载启动 {#quick-results}

单位 ms，中位数与最小—最大值，每配置 N=5。这里是命令 Ready，不是完整发行版启动。

| Runtime | N | Ready ms | Min–max ms |
| --- | --- | --- | --- |
| Native | 5 | 180.43 | 178.95–181.15 |
| pVisor host | 5 | 201.19 | 195.70–203.26 |
| pVisor staged | 5 | 209.56 | 206.75–215.36 |
| pVisor VM | 5 | 749.15 | 707.91–806.28 |
| pVisor VM + writeback | 5 | 746.21 | 704.93–832.73 |
| Docker rootless | 5 | 450.94 | 323.55–557.16 |
| Firecracker PCI | 5 | 854.79 | 832.52–865.48 |
| QEMU q35 | 5 | 486.80 | 478.82–509.91 |
| QEMU microvm | 5 | 364.31 | 359.88–433.75 |

在这组快速样本中，默认 VM 的 Ready 比 Firecracker 更早，中位数差 -105.64 ms，配对 bootstrap 95% 区间 [-126.17, -48.51] ms；Docker 和两种 QEMU 的 Ready 仍更短。

### 同一环境的三轮工具 {#reference-fs}

单位 ms，每轮 N=5。先在每个环境内相加七项操作的实际计时，再取中位数；不是相加中位数，也不是含包装器、字节读回及额外场景的整段墙钟时间。

| Runtime | R1 ms | R2 ms | R3 ms |
| --- | --- | --- | --- |
| Native | 331.41 | 226.80 | 225.48 |
| pVisor host | 351.67 | 239.40 | 237.94 |
| pVisor staged | 798.59 | 526.49 | 518.99 |
| pVisor VM | 3323.01 | 1765.81 | 1758.28 |
| pVisor VM + writeback | 2839.84 | 1826.43 | 1693.68 |
| Docker rootless | 332.76 | 212.50 | 213.37 |
| Firecracker PCI | 1237.13 | 301.14 | 332.43 |
| QEMU q35 | 977.96 | 267.86 | 266.27 |
| QEMU microvm | 1095.77 | 260.47 | 247.01 |

后两轮同时包含文件缓存与工具增量行为：Cargo 不重编未修改源码，npm 保留 node_modules。该表用于预算重复任务，不把下降全部归因于某一项文件系统优化。

<details markdown="1">
<summary>七项工具的三轮明细（ms，每轮 N=5）</summary>

#### R1

| Operation | Native | pVisor host | pVisor staged | pVisor VM | pVisor VM + writeback | Docker rootless | Firecracker PCI | QEMU q35 | QEMU microvm |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 遍历 2,048 文件 | 4.63 | 4.60 | 79.87 | 216.15 | 198.66 | 4.70 | 24.28 | 16.57 | 17.79 |
| 读取并校验 64 MiB | 32.87 | 32.81 | 78.53 | 121.60 | 118.59 | 33.06 | 81.17 | 37.81 | 42.45 |
| 写入/覆盖 256 文件 | 3.80 | 3.90 | 33.27 | 187.78 | 148.01 | 4.12 | 40.46 | 5.22 | 5.31 |
| git status | 15.69 | 15.67 | 159.19 | 520.03 | 368.02 | 15.61 | 126.32 | 83.96 | 168.22 |
| Ripgrep 搜索 | 8.73 | 8.83 | 112.86 | 425.47 | 401.23 | 8.93 | 14.05 | 13.24 | 14.15 |
| 离线 Cargo 编译 | 53.83 | 60.36 | 92.84 | 622.86 | 548.90 | 49.29 | 397.35 | 331.11 | 351.91 |
| 离线 npm 安装 | 212.17 | 225.46 | 244.94 | 1083.78 | 1053.20 | 217.76 | 550.02 | 453.31 | 506.73 |

#### R2

| Operation | Native | pVisor host | pVisor staged | pVisor VM | pVisor VM + writeback | Docker rootless | Firecracker PCI | QEMU q35 | QEMU microvm |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 遍历 2,048 文件 | 4.91 | 4.95 | 87.55 | 138.17 | 138.74 | 4.92 | 4.21 | 4.19 | 4.11 |
| 读取并校验 64 MiB | 27.12 | 27.14 | 46.55 | 41.60 | 38.01 | 27.22 | 27.76 | 27.86 | 27.88 |
| 写入/覆盖 256 文件 | 3.84 | 4.10 | 51.03 | 110.07 | 143.37 | 3.87 | 22.49 | 21.82 | 23.55 |
| git status | 2.58 | 2.60 | 13.13 | 39.12 | 38.34 | 2.56 | 2.28 | 2.94 | 2.50 |
| Ripgrep 搜索 | 8.58 | 9.66 | 112.58 | 455.77 | 384.39 | 8.27 | 6.38 | 7.23 | 5.91 |
| 离线 Cargo 编译 | 7.88 | 8.26 | 14.98 | 49.16 | 47.07 | 5.42 | 4.93 | 5.31 | 5.15 |
| 离线 npm 安装 | 173.00 | 185.30 | 200.13 | 938.55 | 935.41 | 161.12 | 232.13 | 197.63 | 191.20 |

#### R3

| Operation | Native | pVisor host | pVisor staged | pVisor VM | pVisor VM + writeback | Docker rootless | Firecracker PCI | QEMU q35 | QEMU microvm |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 遍历 2,048 文件 | 4.56 | 4.77 | 75.38 | 194.63 | 152.33 | 4.70 | 4.29 | 4.18 | 4.16 |
| 读取并校验 64 MiB | 27.13 | 27.11 | 48.97 | 38.02 | 37.38 | 27.16 | 27.67 | 28.00 | 27.80 |
| 写入/覆盖 256 文件 | 4.52 | 4.45 | 50.46 | 103.77 | 138.08 | 4.49 | 16.86 | 21.96 | 21.36 |
| git status | 2.48 | 3.02 | 13.30 | 36.29 | 38.08 | 2.79 | 2.55 | 2.52 | 2.26 |
| Ripgrep 搜索 | 7.46 | 8.88 | 112.74 | 437.97 | 359.88 | 8.60 | 7.98 | 6.75 | 6.03 |
| 离线 Cargo 编译 | 7.37 | 7.62 | 12.68 | 44.66 | 43.76 | 5.03 | 5.20 | 4.77 | 4.65 |
| 离线 npm 安装 | 171.77 | 182.80 | 208.14 | 908.41 | 861.16 | 161.06 | 268.70 | 197.83 | 178.75 |

</details>

### 热 open 与小块流写 {#cache-status}

单位 ms，每轮 N=5，中位数；每格依次为 R1 / R2 / R3。

热 open 在计时外先打开一次相同文件，计时内执行 10,000 次 open/fstat/close，不读取内容。流写每轮新建一个文件，执行 8,192 次无用户态缓冲的 1 KiB write，再 fsync/close；“write”列在 fsync 前停表，“write + fsync + close”保留显式同步成本。所有配置执行相同调用，并在计时后逐字节核对全部 8 MiB。

| Runtime | open/fstat R1 / R2 / R3 ms | 8 MiB write R1 / R2 / R3 ms | write + fsync + close R1 / R2 / R3 ms |
| --- | --- | --- | --- |
| Native | 10.11 / 10.11 / 10.12 | 5.89 / 5.92 / 5.88 | 9.67 / 9.61 / 9.55 |
| pVisor host | 10.18 / 10.19 / 10.13 | 5.84 / 5.99 / 5.91 | 9.37 / 9.62 / 9.55 |
| pVisor staged | 409.28 / 409.54 / 398.16 | 219.54 / 203.09 / 227.36 | 224.42 / 207.18 / 233.35 |
| pVisor VM | 10.26 / 11.88 / 10.27 | 468.78 / 468.24 / 465.69 | 471.84 / 478.35 / 475.88 |
| pVisor VM + writeback | 10.35 / 10.41 / 10.24 | 3.78 / 5.90 / 8.16 | 15.23 / 12.81 / 21.59 |
| Docker rootless | 10.37 / 10.51 / 10.49 | 5.92 / 6.01 / 5.97 | 9.52 / 9.04 / 9.53 |
| Firecracker PCI | 8.37 / 8.33 / 8.34 | 3.37 / 4.36 / 3.64 | 9.42 / 9.31 / 10.06 |
| QEMU q35 | 8.22 / 8.35 / 8.23 | 2.94 / 3.24 / 2.88 | 9.43 / 9.95 / 9.22 |
| QEMU microvm | 8.36 / 8.24 / 8.22 | 2.93 / 3.65 / 2.97 | 10.79 / 11.20 / 9.68 |

一个出现较低耗时的具体场景是第三轮 10,000 次热 open/fstat：VM + writeback 为 10.24 ms，Docker 为 10.49 ms，中位数差 -0.25 ms，95% 区间 [-0.48, -0.10] ms。优势幅度很小，只适用于这组不读内容的热调用；默认 VM 与 Docker 在该项未检出差异，不能把可选配置的结果解释为写合并加速了元数据。

VM 的惰性只读 open 可在这种不读内容的热路径中留在 guest；它与读取文件的 rg/Cargo 场景不同。写合并配置缩短连续小写的接受时间，但同步仍有成本，不能只用 write 列推导落盘等待。特定场景的较低样本值不代表所有工作负载领先；同批配对 bootstrap 区间见下载，只有五个独立环境，且没有多重比较校正。

### 历史来源 {#complete-task}

此前的一次性任务与 N30 完整任务数据分别保存在[旧快速统计](filesystem-quick.csv)、[历史 N30 统计](filesystem.csv)和[历史来源](filesystem-provenance.csv)。其启动/结果/退出口径不与当前 Ready 和分轮任务数据混合。完整 Ubuntu、macOS、冷磁盘、大型仓库及并发吞吐未由这里的样本覆盖。

### 数据下载与复现 {#run}

[分轮统计 CSV](filesystem-resident.csv) · [同批差异与区间](filesystem-resident-comparisons.csv) · [来源与条件](filesystem-resident-provenance.csv)

2026-10-16，B-FS-TOOLS，GNU release/gateway CLI 与同源新固件；原始样本及冻结入口保存在本地 `benchmark/.data/fr3-20261016b/` 和 `benchmark.new/filesystem/.data/retest-20261016/resident-fixed/`。[复测手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#ready-and-resident-filesystem-rounds) · [工程证据范围](../design/filesystem-performance-analysis.md#filesystem-status) · [比较方法](methodology.md)。
