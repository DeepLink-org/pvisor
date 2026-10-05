# pVisor 相比业界已有方案处于什么水位？

## 主要结论 {#conclusions}

**频繁新建大工作区、只保留少量改动时，pVisor stage 的完整机器流程比所测 Git worktree 和 btrfs reflink 更省时。小工作区 Git 较快。pVisor VM 启动处于轻量 VM 量级，但文件密集任务仍比所测 Firecracker/QEMU 配置等待更长。**

| 需求 | 选型含义 |
|---|---|
| 大工作区的稀疏改动与选择性合入 | 评估 rootless stage 的完整流程成本 |
| 需要独立 guest 内核 | 预算完整 VM 工具时间 |
| 已有容器/Git 工作流 | 比较成本与审查语义 |

## Motivation {#motivation}

Agent 成本除了启动，还有工具、依赖、测试和审查。容器、VM、Agent 内置 sandbox 与托管云环境提供不同边界和工作流。这些测量支持按负载选型。

## 实验设计 {#interpretation}

共享 Linux/x86_64 宿主，AMD Ryzen 7 9700X，Fedora 内核 7.2.8-200.fc44.x86_64。启动进程树及专用 Docker daemon 固定到宿主 CPU 0,1；guest 为 2 vCPU。host/staged 使用 rootless_process。Shell VM 为 128 MiB，工具 VM 为 16 GiB。原生/Docker 不限内存：控制 CPU 与 guest 配置内存，不是相同资源限制的对照。工具与输入已准备，每次新建工作区、热缓存，3 次预热、60 次正式采样；按固定种子随机交错后端。构建、下载及输入复制不计时。

Docker Engine 29.7.2 使用专用 rootless VFS daemon 与可写 bind mount，结果不代表 overlay2 或 Docker Desktop。Firecracker 1.13.1 PCI 不使用 jailer；QEMU 10.2.2 分别使用 q35/microvm、私有 ext4。pVisor VM 使用 virtio-fs 和不同内核。内核、存储、设备及暂存语义均有差异，不能把差距单独归因于 VMM 或 FUSE。

各主题定义正确性与计时。完整 Ubuntu、macOS、apply/网络与固定版本 CLI 保留独立样本组及数量。未测云端、gVisor/Kata、内存净收益及完整 RL 吞吐，不给数值排名。

## 实验数据和分析 {#results}

### 实测水位

启动/文件系统：2026-10-05，修复：2026-10-06，各后端/负载 N=60、失败 0；通常为 P50，分离簇展示中位数与数量。合入：2026-10-04，10/1,000/100,000 文件分别 N=30/10/3。网络：2026-10-04、30 个批次。CLI：独立固定版本。完整审查流程：2026-10-06，每种工作区规模/合入条件/后端 N=30，共 360 个样本、失败 0；与工具任务为不同负载，不合并计时。

| 问题 | 实测水位 | 选型含义 |
|---|---|---|
| [已准备环境启动](startup.md) | pVisor VM 99.76 ms; Firecracker 74.74 ms; QEMU microvm 86.60 ms | 轻量 VM 启动量级 |
| [修复到退出](agent-tasks.md) | staged 0.68 s; VM 3.25 s; QEMU microvm 1.27 s | 关注完整工具等待 |
| [七项工具到退出](filesystem.md) | staged 1.29 s; VM 6.66 s; Firecracker 2.16 s | VM 工具/文件成本明显 |
| [新建工作区到审查、选择性合入和清理](supervision-cost.md) | 10,000 文件、修改 20 个：stage 141 ms；Git worktree 252 ms；reflink 349 ms | 大工作区的稀疏改动有收益；小工作区 Git 更快 |
| [合入](apply.md) | 10: 15.01 ms; 1,000: 836.38 ms; 100,000: 330.40 s | Git patch 较快；语义不同 |
| [网络](network.md) | host proxy 1.24 ms; native 0.95 ms / local request | 另行预算 VM 大块传输 |
| [Agent CLI](agent-tasks.md#cli-compatibility) | 固定版本 Codex 通过；Claude/VM 初始化超时 | 核验具体客户端版本 |

所测 Docker VFS 配置的创建较贵，但 bind mount 单项工具计时仍有参考意义。总耗时不排名 overlay2 或 Docker Desktop。完整 Ubuntu 开机属于不同部署选择。原始证据留在本地；各主题链接加工表格和来源摘要。

### 测量主题

[Startup](startup.md) · [Filesystem](filesystem.md) · [Agent tasks](agent-tasks.md) · [Network](network.md) · [Apply/drop](apply.md) · [VM memory](vm-memory/index.md) · [Density](density.md) · [Cluster](cluster-scalability.md) · [Review](supervision-cost.md) · [Isolation](isolation-tests.md) · [Replay](replay-fidelity.md)

### 业界方案对照

[Docker/devcontainer](compare-containers.md) · [Firecracker/QEMU/gVisor/Kata](compare-runtimes.md) · [Agent sandboxes](compare-agent-sandboxes.md) · [E2B/Daytona/Modal](compare-cloud-sandboxes.md) · [Agent RL infrastructure](compare-rl-infra.md)

### 数据下载与复现 {#run}

[整理后的表格 CSV](index.csv) · [运行时统计](runtime-summary.csv) · [来源与制品](runtime-provenance.csv) · [证据来源摘要](evidence-sources.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
