# 启动一个可用环境要等多久？

## 主要结论 {#conclusions}

**已准备环境中，pVisor host/staged 首条输出分别为 12.72/25.13 ms，短于所测 Docker；pVisor VM 为 100.73 ms，慢于 Firecracker 的 72.61 ms 和 QEMU microvm 的 86.57 ms。**

| 需求 | 选型含义 |
| --- | --- |
| 本机执行并保留改动 | 评估 host/staged 与完整审查流程 |
| 独立 guest 内核 | 同时预算启动和 VM 工具等待 |
| 并发或闲置环境 | 需要固定资源下的吞吐与物理内存实测 |

## Motivation {#motivation}

频繁创建一次性环境时，首条命令的等待会累积。短任务还要预算退出和工具执行，启动数字不能代替完整任务成本。

## 实验设计 {#interpretation}

Linux x86_64，AMD Ryzen 7 9700X，Fedora 7.2.8-200.fc44.x86_64。执行进程树与专用 Docker daemon 固定到 CPU 0,1；VM 为 2 vCPU，shell 探针配置 128 MiB，工具任务配置 16 GiB。原生/Docker 未限制内存，因此是 CPU 控制的任务对照，不能推导相同内存预算下的容量。host/staged 使用 rootless_process。

同一套离线工具与固定输入，每次新建工作区；热缓存、3 次预热、每格 60 次正式采样，固定种子随机交替执行。环境准备、构建、镜像导入和输入重置不计时；启动和退出计入完整任务。Docker Engine 29.7.2 使用专用 rootless **overlay2** daemon、经典镜像存储和可写 bind mount；Firecracker 1.13.1 PCI 不使用 jailer，QEMU 10.2.2 分别使用 q35/microvm 与私有 ext4。pVisor VM 使用 virtio-fs 和自己的固件。内核、存储和暂存语义不同，结果是这些配置下的任务成本，不是纯 VMM 或安全排名。

Ready 截止到校验后的 shell 标记；Exit 截止到进程退出。每次新启动，不使用快照或环境池。冷镜像、完整 OS 初始化和 Agent CLI 初始化不属于该探针。

## 实验数据和分析 {#results}

测于 2026-10-06，每个后端/负载 60/60 有效，正式失败 0。输出、退出和执行器记录必须通过校验；暂存模式还验证宿主原文件不变和完整改动保留。保留所有有效慢样本，没有按耗时剔除。表格通常为 P50；分离分布展示各簇中位数和数量，P95 仅作观察参考。原始报告、二进制、输入与源码摘要保存在忽略的 `.data/`，公开 CSV 保留负载、批次和来源关联。

### 已准备环境 {#reference-startup}

<a id="reference-exit"></a>

| Runtime | Valid / failed | Ready P50 ms | Ready P95 ms | Exit P50 ms | Exit P95 ms |
| --- | --- | --- | --- | --- | --- |
| Native | 60 / 0 | 1.28 | 1.71 | 1.35 | 1.82 |
| pVisor host | 60 / 0 | 12.72 | 14.18 | 24.03 | 24.71 |
| pVisor staged | 60 / 0 | 25.13 | 26.85 | 33.84 | 44.55 |
| pVisor VM | 60 / 0 | 100.73 | 109.17 | 136.65 | 156.89 |
| Docker rootless / overlay2 | 60 / 0 | 74.48 | 87.13 | 98.29 | 110.22 |
| Firecracker PCI | 60 / 0 | 72.61 | 74.53 | 95.98 | 104.27 |
| QEMU q35 | 60 / 0 | 213.07 | 217.77 | 237.52 | 245.37 |
| QEMU microvm | 60 / 0 | 86.57 | 95.30 | 109.52 | 119.96 |

### 完整发行版与 macOS {#full-ubuntu}

<a id="macos"></a>

完整 Ubuntu 冷启动和 Apple Silicon/HVF 在当前制品下尚未复测，没有同条件性能排名。需要这些场景时，轻量 shell 探针不能代替实际部署测量。

### 数据下载与复现 {#run}

[整理后的统计 CSV](startup.csv) · [全部运行时统计](runtime-summary.csv) · [差异与 95% 置信区间](runtime-comparisons.csv) · [源码与制品来源](runtime-provenance.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
