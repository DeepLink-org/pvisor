# VM 与容器启动性能

## 主要结论 {#conclusions}

**pVisor VM 启动与 Docker、QEMU microvm 处于相近的百毫秒量级，Firecracker 的最小配置略快。** 已准备环境的首条输出 P50 分别为 **86、90、88、74 ms**。pVisor host/staged 为约 **6/15 ms**，适合更轻的本地任务。

与启动完整 Ubuntu 相比，无镜像 pVisor VM 约 **110 ms**，Firecracker/QEMU 约 **5–8 s**，短任务开机等待明显更少。这是不同环境部署方式的成本；完整发行版提供的内核、服务与启动过程也不同。工具与最终任务速度见[任务性能](agent-tasks.md)。

## Motivation {#motivation}

频繁创建一次性 Agent 环境时，启动等待会直接影响交互。常驻环境则能摊薄开机成本。首条输出、CLI 完整退出和工具可用分别代表不同预算，需要区分。

## 实验设计 {#interpretation}

Ready 从宿主启动命令前到有效输出，Exit 到命令进程退出；下载、工具安装、模板和每次复制准备不计入。所有任务创建新环境，无 RAM 快照或常驻池，宿主缓存已预热。

Linux 最小环境使用两核预算、2 vCPU / 128 MiB、相同工具制品；每格 3 次预热、30 次测量、随机顺序。Firecracker/QEMU 使用裁剪内核与静态 init，Docker daemon 已运行。完整 Ubuntu 使用 generic 内核、initrd 和 systemd，VM 2 vCPU / 2 GiB；Firecracker/pVisor N=30，QEMU 独立 N=10。共享宿主的缓存、后台负载与配置差异限制精细排名。[方法](methodology.md)记录完整制品与条件。

## 实验数据和分析 {#results}

### 已准备环境：轻量启动路径 {#reference-startup}

| Backend | N | Ready P50 / P95 / P99 ms |
|---|---|---|
| Native | 30 | 1.20 / 1.42 / 1.49 |
| pVisor host | 30 | 6.10 / 6.65 / 7.01 |
| pVisor staged | 30 | 14.68 / 15.94 / 16.09 |
| pVisor VM | 30 | 86.29 / 92.99 / 94.89 |
| Docker rootless | 30 | 90.12 / 101.13 / 112.01 |
| Firecracker PCI | 30 | 73.74 / 79.06 / 81.21 |
| QEMU q35 | 30 | 218.12 / 235.27 / 237.07 |
| QEMU microvm | 30 | 88.10 / 103.42 / 109.75 |

pVisor VM 与 Docker、QEMU microvm 的中位数接近；不能据几毫秒差异声称稳定领先。QEMU q35 的设备配置更重，不能用它代表 QEMU 的最低启动成本。pVisor 自身内核与 virtio-fs、参考 VM 的 ext4 不同，这里比较的是完整命令路径。

![已准备环境首条输出](../../assets/benchmarks/reference-env-20261004/reference-startup.svg)

### 完整发行版：部署等待 {#full-ubuntu}

| Backend | N | Ready P50 / P95 / P99 ms | Exit P50 / P95 ms |
|---|---:|---|---|
| Native / Fedora | 30 | 1.23 / 1.49 / 1.73 | 1.29 / 1.57 |
| pVisor staged | 30 | 15.04 / 18.32 / 19.35 | 43.75 / 44.52 |
| pVisor VM / host | 30 | 109.69 / 121.62 / 141.53 | 173.57 / 193.94 |
| Firecracker / Ubuntu | 30 | 5644.11 / 6009.57 / 6583.04 | 9234.94 / 9626.10 |
| Firecracker / Ubuntu first boot | 30 | 9246.65 / 10293.13 / 10322.29 | 12862.17 / 13875.61 |
| QEMU q35 / Ubuntu | 10 | 5428.90 / 7698.78 / 8968.91 | 9054.86 / 12078.24 |
| QEMU microvm / Ubuntu | 10 | 7666.69 / 8547.61 / 8706.71 | 11199.06 / 12100.94 |

无镜像 pVisor 可以直接使用工具目录，减少完整 OS 开机等待。需要 Ubuntu 系统服务和发行版环境时，上述秒数是获得该环境的成本。首个 cloud-init 启动不包含首次下载。完整 Ubuntu 使用同一磁盘模板；QEMU 两行与其余数据为独立批次，不合并分布。这些数据不能推导“libkrun 比 Firecracker 快 50 倍”。

![完整发行版与无镜像部署](../../assets/benchmarks/full-ubuntu-qemu-20261004/ubuntu-startup.svg)

### 首条输出与清理退出 {#reference-exit}

| Backend | Ready P50/P95 ms | Exit P50/P95 ms |
|---|---|---|
| Native | 1.27 / 1.99 | 1.35 / 2.05 |
| pVisor host | 5.98 / 9.91 | 13.23 / 15.78 |
| pVisor staged | 14.61 / 21.83 | 43.52 / 47.36 |
| pVisor VM | 88.46 / 142.99 | 153.11 / 207.68 |
| Docker rootless | 94.29 / 237.58 | 123.38 / 286.71 |
| Firecracker PCI | 74.18 / 92.76 | 101.82 / 121.81 |
| QEMU q35 | 219.74 / 306.82 | 248.39 / 338.34 |
| QEMU microvm | 87.42 / 170.44 | 116.09 / 199.42 |

该表独立测量 30 次，以专用进程等待记录精确退出。VM 首条输出约 **88 ms**，完整退出约 **153 ms**；清理与记录也有预算。Ready 不代表真实 CLI 已经完成初始化或能够修复项目。

### macOS / HVF {#macos}

Apple M4 上 2 vCPU / 128 MiB 的 pVisor VM 首条输出 P50 **84.35 ms**、P95 **112.14 ms**，100 个正式样本。Docker/Firecracker/QEMU 没有对应的同机 macOS 对照，Linux 与 macOS 数字不做排名。

### 数据来源与复现 {#run}

[最小环境样本](../../assets/benchmarks/reference-env-20261004/samples.csv) · [完整 Ubuntu](../../assets/benchmarks/full-ubuntu-20261004/summary.tsv) · [QEMU 完整 Ubuntu](../../assets/benchmarks/full-ubuntu-qemu-20261004/summary.tsv) · [退出报告](../../assets/benchmarks/reference-env-20261004/followups/reference-startup-exit-20261004/report.tsv) · [macOS 样本](../../assets/benchmarks/startup-p0-20261003.tsv)

细分账本、firmware A/B、历史制品与复现命令见[启动技术分析](../design/vm-startup-performance-analysis.md)。
