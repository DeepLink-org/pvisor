# 开发工具在 pVisor、Docker 和轻量 VM 中要等多久？

## 主要结论 {#conclusions}

**七项工具到退出，pVisor host 为 0.47 s、staged 为 1.09 s、VM 为 4.27 s；Docker 为 0.82 s，Firecracker 为 2.29 s，QEMU microvm 为 1.50 s。staged 的审查能力有文件访问成本，VM 工具成本仍明显。**

| 需求 | 选型含义 |
| --- | --- |
| 本机执行并保留改动 | 评估 host/staged 与完整审查流程 |
| 独立 guest 内核 | 同时预算启动和 VM 工具等待 |
| 并发或闲置环境 | 需要固定资源下的吞吐与物理内存实测 |

## Motivation {#motivation}

Agent 的工具循环需要遍历、读写、搜索、编译和安装依赖。选型要同时看单项操作与完整等待，避免用快速启动估计文件密集任务。

## 实验设计 {#interpretation}

Linux x86_64，AMD Ryzen 7 9700X，Fedora 7.2.8-200.fc44.x86_64。执行进程树与专用 Docker daemon 固定到 CPU 0,1；VM 为 2 vCPU，shell 探针配置 128 MiB，工具任务配置 16 GiB。原生/Docker 未限制内存，因此是 CPU 控制的任务对照，不能推导相同内存预算下的容量。host/staged 使用 rootless_process。

同一套离线工具与固定输入，每次新建工作区；热缓存、3 次预热、每格 60 次正式采样，固定种子随机交替执行。环境准备、构建、镜像导入和输入重置不计时；启动和退出计入完整任务。Docker Engine 29.7.2 使用专用 rootless **overlay2** daemon、经典镜像存储和可写 bind mount；Firecracker 1.13.1 PCI 不使用 jailer，QEMU 10.2.2 分别使用 q35/microvm 与私有 ext4。pVisor VM 使用 virtio-fs 和自己的固件。内核、存储和暂存语义不同，结果是这些配置下的任务成本，不是纯 VMM 或安全排名。

负载为 32 个目录中的 2,048 文件遍历、64 MiB 读取和 SHA256 校验、256 × 64 KiB 写入、git status、rg、64 个无外部依赖 Cargo 模块和 32 个离线 npm 包。单项含校验，不含启动与退出；完整任务包括七项和退出。大型仓库、冷磁盘、在线 registry 和并发吞吐未测。

## 实验数据和分析 {#results}

测于 2026-10-06，每个后端/负载 60/60 有效，正式失败 0。输出、退出和执行器记录必须通过校验；暂存模式还验证宿主原文件不变和完整改动保留。保留所有有效慢样本，没有按耗时剔除。表格通常为 P50；分离分布展示各簇中位数和数量，P95 仅作观察参考。原始报告、二进制、输入与源码摘要保存在忽略的 `.data/`，公开 CSV 保留负载、批次和来源关联。

### 七项工具操作 {#reference-fs}

单位 ms；P50 或分离簇的中位数与数量。

| Operation | Native | pVisor host | pVisor staged | pVisor VM | Docker rootless / overlay2 | Firecracker PCI | QEMU q35 | QEMU microvm |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 遍历 2,048 文件 | 4.64 | 4.63 | 73.53 | 152.97 | 4.62 | 23.43 | 15.92 | 19.91 |
| 读取并校验 64 MiB | 32.62 | 31.73 | 66.98 | 116.45 | 32.64 | 79.79 | 37.43 | 40.19 |
| 写入 256 文件 | 3.62 | 3.66 | 26.23 | 135.16 | 3.82 | 43.11 | 5.02 | 4.99 |
| git status | 14.50 | 14.30 | 120.01 | 352.11 (43/60); 665.46 (17/60) | 14.42 | 129.97 | 80.27 | 155.03 |
| Ripgrep 搜索 | 7.35 | 7.22 | 84.81 | 434.07 | 6.86 | 15.82 | 12.57 | 13.03 |
| 离线 Cargo 编译 | 51.00 | 50.81 | 71.76 | 482.69 | 48.14 | 373.88 | 260.34 | 296.94 |
| 离线 npm 安装 | 170.56 | 170.22 | 226.67 | 1320.16 | 215.77 | 546.50 | 399.42 | 421.37 |

### 启动到退出 {#complete-task}

| Runtime | Valid / failed | Completion P50 s | Completion P95 s |
| --- | --- | --- | --- |
| Native | 60 / 0 | 0.45 | 0.46 |
| pVisor host | 60 / 0 | 0.47 | 0.48 |
| pVisor staged | 60 / 0 | 1.09 | 1.13 |
| pVisor VM | 60 / 0 | 4.27 | 4.60 |
| Docker rootless / overlay2 | 60 / 0 | 0.82 | 0.85 |
| Firecracker PCI | 60 / 0 | 2.29 | 2.33 |
| QEMU q35 | 60 / 0 | 1.49 | 1.51 |
| QEMU microvm | 60 / 0 | 1.50 | 1.54 |

staged 相对 Docker 的完整任务中位数差为 +268.81 ms，95% 配对 bootstrap 区间 [+264.64, +271.15] ms。VM 相对 QEMU microvm 为 +2778.04 ms，区间 [+2754.17, +2807.74] ms。暂存能力与存储配置均有差异，不能从总差距推断某一组件的成本。

<a id="full-ubuntu"></a>
完整 Ubuntu 文件负载在当前制品下尚未复测。

### 数据下载与复现 {#run}

[整理后的统计 CSV](filesystem.csv) · [全部运行时统计](runtime-summary.csv) · [差异与 95% 置信区间](runtime-comparisons.csv) · [源码与制品来源](runtime-provenance.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
