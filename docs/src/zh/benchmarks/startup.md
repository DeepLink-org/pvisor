# 启动一个可用环境要等多久？

## 主要结论 {#conclusions}

**已准备环境下，pVisor VM 首条输出 99.76 ms，Firecracker 74.74 ms、QEMU microvm 86.60 ms；rootless host/staged 等待更短。短任务预算还应包含退出与实际工具执行。**

| 需求 | 选型含义 |
|---|---|
| 本机工具与保留改动 | 评估 rootless host/staged |
| 需要独立 guest 内核 | 预算完整 VM 工具时间 |
| 已有容器/Git 工作流 | 比较成本与审查语义 |

## Motivation {#motivation}

一次性环境反复支付启动成本。首条有效输出与进程退出是不同等待；持久池可以摊薄二者。

## 实验设计 {#interpretation}

共享 Linux/x86_64 宿主，AMD Ryzen 7 9700X，Fedora 内核 7.2.8-200.fc44.x86_64。启动进程树及专用 Docker daemon 固定到宿主 CPU 0,1；guest 为 2 vCPU。host/staged 使用 rootless_process。Shell VM 为 128 MiB，工具 VM 为 16 GiB。原生/Docker 不限内存：控制 CPU 与 guest 配置内存，不是相同资源限制的对照。工具与输入已准备，每次新建工作区、热缓存，3 次预热、60 次正式采样；按固定种子随机交错后端。构建、下载及输入复制不计时。

Docker Engine 29.7.2 使用专用 rootless VFS daemon 与可写 bind mount，结果不代表 overlay2 或 Docker Desktop。Firecracker 1.13.1 PCI 不使用 jailer；QEMU 10.2.2 分别使用 q35/microvm、私有 ext4。pVisor VM 使用 virtio-fs 和不同内核。内核、存储、设备及暂存语义均有差异，不能把差距单独归因于 VMM 或 FUSE。

Ready 截止到校验后的 shell 标记；Exit 截止到进程结束。测新启动，不使用 RAM 快照或环境池。完整 Agent CLI 初始化、冷磁盘和跨平台排名不属于此探针。

## 实验数据和分析 {#results}

### 已准备环境 {#reference-startup}

测于 2026-10-05，每后端 60/60 有效、正式失败 0。检查输出与退出；暂存模式额外检查原文件未修改、改动完整保留。通过校验的慢样本全部保留，不按耗时剔除。P95 仅为观察参考。按[比较方法](methodology.md)中的预定规则识别出分离簇时，展示各簇中位数及占 60 次的数量，替代单个 P50。

<a id="reference-exit"></a>

| 执行方式 | 有效 / 失败 | Ready P50 ms | Ready P95 ms | Exit P50 ms | Exit P95 ms |
|---|---|---|---|---|---|
| Native | 60 / 0 | 1.24 | 1.57 | 1.31 | 1.65 |
| pVisor host | 60 / 0 | 12.22 | 13.67 | 23.82 | 24.56 |
| pVisor staged | 60 / 0 | 24.99 | 27.93 | 33.77 | 54.32 |
| pVisor VM | 60 / 0 | 99.76 | 110.86 | 155.02 | 176.06 |
| Docker rootless / VFS | 60 / 0 | 3380.93 | 3622.36 | 4487.13 | 4724.50 |
| Firecracker PCI | 60 / 0 | 74.74 | 81.13 | 99.62 | 107.00 |
| QEMU q35 | 60 / 0 | 213.89 | 223.19 | 239.82 | 254.20 |
| QEMU microvm | 60 / 0 | 86.60 | 101.41 | 110.03 | 123.08 |

VFS 创建容器包含可写层复制。秒级启动成本不代表常规 Docker 启动性能；复用环境可摊薄创建成本。

### 完整 Ubuntu 部署 {#full-ubuntu}

独立的 2026-10-04 样本组，两核 / 2 GiB、热缓存、3 次预热。单位 P50 ms；pVisor/Firecracker N=30，QEMU 独立 N=10。Ubuntu 使用发行版内核、initrd、systemd、cloud-init；pVisor 复用宿主工具目录。

| Deployment | N | Ready P50 ms | Exit P50 ms |
|---|---|---|---|
| pVisor VM / host tools | 30 | 109.69 | 173.57 |
| Firecracker / Ubuntu | 30 | 5644.11 | 9234.94 |
| Firecracker / Ubuntu first boot | 30 | 9246.65 | 12862.17 |
| QEMU q35 / Ubuntu | 10 | 5428.90 | 9054.86 |
| QEMU microvm / Ubuntu | 10 | 7666.69 | 11199.06 |

此表描述不同 OS 配置的部署等待，不是纯 VMM 排名。

### macOS / HVF {#macos}

独立 Apple M4 数据，2 vCPU / 128 MiB、N=100：首条输出 P50 84.35 ms，P95 观察参考 112.14 ms。macOS 的 Docker/Firecracker/QEMU 同条件对照未测。

### 数据下载与复现 {#run}

[整理后的表格 CSV](startup.csv) · [运行时统计](runtime-summary.csv) · [来源与制品](runtime-provenance.csv) · [证据来源摘要](evidence-sources.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
