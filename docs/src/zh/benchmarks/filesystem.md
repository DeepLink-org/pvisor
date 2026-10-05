# 开发工具在 pVisor、Docker 和轻量 VM 中要等多久？

## 主要结论 {#conclusions}

**rootless pVisor host 工具执行接近原生；staged 增加文件访问与保留改动成本。七项工具任务中，pVisor VM 比所测 Firecracker/QEMU 配置等待更长。Docker VFS 创建较贵，即使 bind mount 内工具较快。**

| 需求 | 选型含义 |
|---|---|
| 本机工具与保留改动 | 评估 rootless host/staged |
| 需要独立 guest 内核 | 预算完整 VM 工具时间 |
| 已有容器/Git 工作流 | 比较成本与审查语义 |

## Motivation {#motivation}

仓库遍历、搜索、编译与装依赖构成许多 Agent 工具循环。选型需同时考虑这些成本、启动和审查。

## 实验设计 {#interpretation}

共享 Linux/x86_64 宿主，AMD Ryzen 7 9700X，Fedora 内核 7.2.8-200.fc44.x86_64。启动进程树及专用 Docker daemon 固定到宿主 CPU 0,1；guest 为 2 vCPU。host/staged 使用 rootless_process。Shell VM 为 128 MiB，工具 VM 为 16 GiB。原生/Docker 不限内存：控制 CPU 与 guest 配置内存，不是相同资源限制的对照。工具与输入已准备，每次新建工作区、热缓存，3 次预热、60 次正式采样；按固定种子随机交错后端。构建、下载及输入复制不计时。

Docker Engine 29.7.2 使用专用 rootless VFS daemon 与可写 bind mount，结果不代表 overlay2 或 Docker Desktop。Firecracker 1.13.1 PCI 不使用 jailer；QEMU 10.2.2 分别使用 q35/microvm、私有 ext4。pVisor VM 使用 virtio-fs 和不同内核。内核、存储、设备及暂存语义均有差异，不能把差距单独归因于 VMM 或 FUSE。

负载：32 个目录中 2,048 文件；64 MiB 读取及 SHA256 校验；256 × 64 KiB 写入；git status；rg；64 个无外部依赖 Cargo 模块；32 个离线 npm 包。单项含校验、不含启动/退出；Completion 包含七项及退出。大型仓库、冷磁盘、联网 registry 与并发吞吐未测。

## 实验数据和分析 {#results}

### 七项工具操作 {#reference-fs}

测于 2026-10-05，每后端 60/60 有效、正式失败 0。检查输出与退出；暂存模式额外检查原文件未修改、改动完整保留。通过校验的慢样本全部保留，不按耗时剔除。P95 仅为观察参考。按[比较方法](methodology.md)中的预定规则识别出分离簇时，展示各簇中位数及占 60 次的数量，替代单个 P50。

单位 ms；通常为 P50，分离簇展示各簇中位数与数量。

| 操作 | Native | pVisor host | pVisor staged | pVisor VM | Docker rootless / VFS | Firecracker PCI | QEMU q35 | QEMU microvm |
|---|---|---|---|---|---|---|---|---|
| 遍历 2,048 文件 | 4.68 | 4.69 | 74.90 | 227.11 | 5.03 | 23.30 | 24.99 | 27.76 |
| 读取并校验 64 MiB | 33.28 | 32.53 | 68.85 | 154.80 | 32.48 | 81.30 | 39.16 (15/60); 106.63 (45/60) | 41.63 (13/60); 105.45 (47/60) |
| 写入 256 文件 | 3.69 | 3.68 | 27.10 | 159.80 | 3.69 (54/60); 6.80 (6/60) | 43.83 | 5.26 (15/60); 45.30 (45/60) | 5.45 (14/60); 45.22 (46/60) |
| git status | 15.35 | 15.91 | 123.95 | 425.74 | 17.55 | 123.36 | 90.22 | 157.20 |
| Ripgrep 搜索 | 8.56 | 8.81 | 91.29 | 460.88 | 9.99 | 14.39 | 14.53 | 15.11 |
| 离线 Cargo 编译 | 57.21 (41/60); 144.26 (19/60) | 84.40 | 142.29 | 852.03 | 190.13 | 371.36 | 419.54 | 454.21 |
| 离线 npm 安装 | 190.34 | 198.23 | 263.19 | 1662.23 | 273.68 | 565.52 | 583.35 | 619.69 |

Docker bind mount 单项计时不含 VFS 创建。staged/VM 保留改动供审查，可写 bind 则直接修改挂载目录。目录扫描、Git 与工具加载仍增加交互等待；此对照不能确定某一层是唯一原因。

### 启动到退出 {#complete-task}

单位秒；P95 仅作观察参考，不是耗时上界。

| Backend | Valid / failed | Completion P50 s | Completion P95 s |
|---|---|---|---|
| Native | 60 / 0 | 0.51 | 0.70 |
| pVisor host | 60 / 0 | 0.57 | 0.79 |
| pVisor staged | 60 / 0 | 1.29 | 1.58 |
| pVisor VM | 60 / 0 | 6.66 | 8.00 |
| Docker rootless / VFS | 60 / 0 | 5.68 | 6.76 |
| Firecracker PCI | 60 / 0 | 2.16 | 2.35 |
| QEMU q35 | 60 / 0 | 2.44 | 2.75 |
| QEMU microvm | 60 / 0 | 2.45 | 2.79 |

### 完整 Ubuntu 文件操作 {#full-ubuntu}

独立的 2026-10-04 Firecracker/Ubuntu 样本组，2 vCPU / 16 GiB、N=10、3 次预热。单位 P50 ms；OS/工具/存储不同，不合并分布。QEMU/Ubuntu 没有七项操作实测。

| Operation | Firecracker / Ubuntu P50 ms |
|---|---|
| 遍历 2,048 文件 | 18.64 |
| 读取并校验 64 MiB | 115.51 |
| 写入 256 文件 | 36.07 |
| git status | 136.50 |
| Ripgrep 搜索 | 20.40 |
| 离线 Cargo 编译 | 969.09 |
| 离线 npm 安装 | 1079.89 |

[Startup](startup.md) · [Repair tasks](agent-tasks.md) · [Apply/drop](apply.md)

### 数据下载与复现 {#run}

[整理后的表格 CSV](filesystem.csv) · [运行时统计](runtime-summary.csv) · [来源与制品](runtime-provenance.csv) · [证据来源摘要](evidence-sources.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
