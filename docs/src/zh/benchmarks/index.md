# pVisor 与已有方案相比，适合哪些任务？

## 主要结论 {#conclusions}

**pVisor 的优势是低成本获得暂存与审查工作流，以及快速启动本地 VM；文件密集型 VM 任务的执行速度仍是主要短板。** 选择时应同时看任务总耗时、隔离边界与改动合入方式。

| 用户关心的问题 | pVisor 的性能位置 | 选型含义 |
|---|---|---|
| [启动一个已准备环境](startup.md#reference-startup) | VM 首条输出约 86 ms，Docker 90 ms、Firecracker 74 ms、QEMU microvm 88 ms | 与轻量 VM、Docker 处于同一百毫秒量级 |
| [启动完整发行版](startup.md#full-ubuntu) | 无镜像 VM 约 110 ms；完整 Ubuntu 的 Firecracker/QEMU 约 5–8 s | 减少短任务开机等待；不是相同 OS 配置下的 VMM 排名 |
| [修复并运行测试](agent-tasks.md#reference-env) | staged 0.70 s，Docker 0.90 s；VM 3.97 s，QEMU microvm 1.85 s | staged 适合交互式工具任务；VM 工具执行更慢 |
| [文件访问](filesystem.md) | 七项工具任务 staged 1.11 s、VM 4.08 s；Docker 0.97 s、Firecracker 2.37 s、QEMU microvm 1.77 s | staged 的交互等待更短；VM 提供独立 guest kernel，需预留更多工具执行时间 |
| [审查后合入](apply.md) | 10 文件约 15 ms，1,000 文件约 0.84 s；10 万文件约 5.5 min | 适合小批交互合入；大批量合入慢于同批 Git patch |
| [网络](network.md) | 本地小请求 host proxy 1.24 ms、原生 0.95 ms；VM 大块传输约 155 MiB/s、原生 869 MiB/s | 小请求代理开销较小，VM 批量传输有明显差距 |
| [CLI 兼容性](agent-tasks.md) | Codex 的受控工具闭环通过；Claude/VM 初始化超时 | 使用 VM 前核对具体客户端和配置 |

这些数字来自各主题的固定配置。文件系统主表对比原生、host staged、VM、Docker、Firecracker 和两种 QEMU 配置的本地开发工具负载；不同配置的样本和百分位数独立保留，具体制品见关联报告。

## Motivation {#motivation}

运行 Agent 的成本不仅是启动。工具会读取仓库、安装依赖、执行测试，最后还需要审查和合入结果。容器、VM、Agent 内置沙箱和云端环境提供不同边界，本章帮助读者判断 pVisor 的速度与工作流是否适合自己的任务。

## 实验设计 {#interpretation}

性能实验区分首条有效输出、工具执行、完整任务和清理退出。输入与校验固定，准备镜像和工具的时间独立记录；失败和未测项目不会作为零耗时样本。Linux 本地对照使用同机原生、Docker/rootless Podman、Firecracker、QEMU，以及 pVisor 各执行模式。macOS/HVF 的启动与内存数据单独报告。[基准方法](methodology.md)说明配置、样本数和原始数据身份。

完整发行版与最小 VM 分别回答部署等待和已准备环境成本。真实 CLI 使用受控模型响应，排除模型推理与公网波动；这些结果不等于真实模型成功率。隔离边界和文件修改语义也属于比较条件。

## 实验数据和分析 {#results}

### 按场景选择

可信本地任务且需要审查改动，可以优先考虑 staged：完整修复任务接近原生的秒以下预算，比 VM 更轻。已有成熟 Docker + worktree/Git 审查流程时，Docker 的文件访问更有优势，是否引入 pVisor 取决于统一暂存、冲突保护和执行记录是否有价值。

需要独立 guest kernel 时，pVisor VM 的启动属于轻量 VM 水平，无须启动完整发行版。不过 npm、Git、搜索和文件创建的累计成本明显，长时间复用环境时应重点看工具时间。云端服务的扩展能力、gVisor/Kata 的同机性能及真实 RL 训练吞吐没有对应实测，不做数值排名。

### 测量主题

[VM 启动](startup.md) · [文件系统](filesystem.md) · [完整 Agent 任务](agent-tasks.md) · [网络](network.md) · [apply/drop](apply.md) · [VM 内存与快照](vm-memory/index.md) · [并发密度](density.md) · [Cluster 扩展性](cluster-scalability.md) · [监督成本](supervision-cost.md) · [隔离验证](isolation-tests.md) · [回放保真度](replay-fidelity.md)

### 工具对比

[Docker/devcontainer](compare-containers.md) · [隔离运行时](compare-runtimes.md) · [Agent 自带沙箱](compare-agent-sandboxes.md) · [云端沙箱](compare-cloud-sandboxes.md) · [RL 基础设施](compare-rl-infra.md)

### 数据与技术分析

各主题链接原始样本和制品摘要。优化实验、历史 A/B 与复现细节保存在[文件系统技术分析](../design/filesystem-performance-analysis.md)、[启动技术分析](../design/vm-startup-performance-analysis.md)、[内存技术分析](../design/vm-memory-performance-analysis.md)和[协议记录](../design/benchmark-methodology-evidence.md)。
