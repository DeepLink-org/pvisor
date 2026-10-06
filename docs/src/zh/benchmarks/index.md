# pVisor 相比已有方案，在哪些场景有优势？

## 主要结论 {#conclusions}

**频繁创建大工作区、只合入少量改动时，pVisor stage 的完整机器流程更快：10,000 文件中修改 20 个、保留 10 个，stage 为 141 ms，Git worktree 为 248 ms，btrfs reflink 为 343 ms。小工作区原生流程更快；pVisor VM 的工具任务仍慢于所测 Firecracker/QEMU。**

| 场景 | 选型含义 |
|---|---|
| 大工作区、稀疏改动、用后丢弃 | stage 的完整流程有实测优势 |
| 小工作区或只关心工具运行 | 比较原生流程与容器成本 |
| VM 或高并发容量 | 需要完整工具和资源实测 |

## Motivation {#motivation}

选型需要知道完成同样结果的总成本，而不只是启动或单个命令。审查合入、执行边界与资源占用决定哪类 Agent 工作适合 pVisor。

## 实验设计 {#interpretation}

复用[启动](startup.md)、[文件系统](filesystem.md)、[修复任务](agent-tasks.md)和[完整审查流程](supervision-cost.md)的独立注册实验。前三类每后端 60 次、3 次预热；审查流程每规模/条件/后端 30 次、3 次预热。同机 CPU 0,1、热缓存、随机交替执行，全部通过正确性校验，没有按速度剔除。各主题的负载、资源和计时边界不同，不合并其分布。Docker 使用 rootless overlay2；完整审查流程对照是原生 Git/reflink，不是容器或 VM 安全排名。

[网络](network.md)每条件 30 个独立批次、3 次预热；小请求以每批 256 请求的中位数作为一个样本，origin 在负载 CPU 预算之外，内存没有统一限额。[容量](density.md)在共同两核、2 GiB、零 swap 预算下，按并发度和空闲/有效工具负载分别运行五轮，保留所有失败、未知和 OOM。

## 实验数据和分析 {#results}

2026-10-06 的启动、文件系统和修复共 1,440 个有效样本，完整审查流程 360 个；2026-10-07 的独立 stock 内核启动对照为 240 个有效样本，失败均为 0。各批次不合并，中位数差异和 95% 配对 bootstrap 区间见各专题。

| 问题 | 实测水位（耗时为 P50） |
|---|---|
| [Startup](startup.md) | stock 对照：VM 100.91 ms；Firecracker 285.04 ms；QEMU microvm 321.56 ms；q35 702.98 ms |
| [Repair completion](agent-tasks.md) | staged 0.64 s; Docker 0.81 s; VM 3.25 s; QEMU microvm 1.40 s |
| [Seven-tool completion](filesystem.md) | staged 1.09 s; Docker 0.82 s; VM 4.27 s; Firecracker 2.29 s |
| [Review workflow](supervision-cost.md) | 10,000 files: stage 141 ms; Git 248 ms; reflink 343 ms |
| [网络](network.md) | 八线程小请求：原生 0.69 ms；host proxy 10.09 ms；VM 2.10 ms |
| [活跃容量](density.md) | 2 GiB：stage/Podman 32 路、VM 16 路通过全部五轮 |


网络为 510 个有效批次；容量扫描保留全部 320 个批次，包括 57 个失败。两个实验分别统计，不能合并成一种容量或速度排名。退役 Controller/Worker 结果是[历史证据](cluster-scalability.md)，不进入当前比较或 daemon 容量规划；daemon 吞吐、密度与历史成本均未测。

内存压缩带来的净物理内存与有效任务密度、裁剪内核收益、完整 Ubuntu 和 macOS 对照尚未完成当前制品验证；没有相应优势结论。云端、gVisor/Kata 和完整 RL 吞吐没有同条件排名。合入、网络、隔离和回放的专题需按各自证据范围判断，不能由这里的短任务推导。

[Network](network.md) · [Apply](apply.md) · [Density](density.md) · [VM memory](vm-memory/index.md) · [Isolation](isolation-tests.md) · [Replay](replay-fidelity.md)

[Docker/devcontainer](compare-containers.md) · [Firecracker/QEMU/gVisor/Kata](compare-runtimes.md) · [Agent sandboxes](compare-agent-sandboxes.md) · [E2B/Daytona/Modal](compare-cloud-sandboxes.md) · [Agent RL infrastructure](compare-rl-infra.md)

### 数据下载与复现 {#run}

[Runtime statistics](runtime-summary.csv) · [Confidence intervals](runtime-comparisons.csv) · [Runtime provenance](runtime-provenance.csv) · [Workflow statistics](workflow-summary.csv) · [Workflow intervals](workflow-comparisons.csv) · [Workflow provenance](workflow-provenance.csv) · [Method](methodology.md) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
