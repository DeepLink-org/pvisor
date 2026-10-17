# pVisor 适合哪些场景，各自代价是多少？

## 主要结论 {#conclusions}

**频繁创建大工作区、只合入少量改动时，pVisor stage 的完整机器流程更快：10,000 文件中修改 20 个、保留 10 个，stage 为 141 ms，Git worktree 为 248 ms，btrfs reflink 为 343 ms。小工作区原生流程更快；VM 工具等待参照各配置的独立测量。**

| 场景 | 选型含义 |
|---|---|
| 大工作区、稀疏改动、用后丢弃 | stage 的完整流程有实测优势 |
| 小工作区或只关心工具运行 | 比较原生流程与容器成本 |
| VM 或高并发容量 | 需要完整工具和资源实测 |

<!-- measurement-status:start -->

文件系统采用九配置、19项分层负载，每个单元三个独立新环境、每环境三轮，无干扰门禁。实际执行505个环境，501个通过、4个资源单元超时，另有8个后续样本按协议未执行。历史N5/N30与一次性任务独立保留，其他主题继续使用各自批次。

<!-- measurement-status:end -->

## Motivation {#motivation}

选型需要知道完成同样结果的总成本，而不只是启动或单个命令。审查合入、执行边界与资源占用决定哪类 Agent 工作适合 pVisor。

## 实验设计 {#interpretation}

复用[启动](startup.md)、[文件系统](filesystem.md)、[修复任务](agent-tasks.md)和[完整审查流程](supervision-cost.md)的独立注册实验。启动与修复每后端 60 次、3 次预热；文件系统当前为九配置、19项负载，每个单元N3新环境、内部三轮、无额外预热，历史N5/N30单独保留；审查流程每规模/条件/后端 30 次、3 次预热。同机CPU 0,1，缓存按各专题设定，随机交替执行；有效计时均通过正确性校验，失败和未执行另列，没有按速度剔除。各主题的负载、资源和计时边界不同，不合并其分布。Docker 使用 rootless overlay2；完整审查流程对照是原生 Git/reflink，不是容器或 VM 安全排名。

[网络](network.md)每条件 30 个独立批次、3 次预热；小请求以每批 256 请求的中位数作为一个样本，origin 在负载 CPU 预算之外，内存没有统一限额。[容量](density.md)在共同两核、2 GiB、零 swap 预算下，按并发度和空闲/有效工具负载分别运行五轮，保留所有失败、未知和 OOM。

[惰性镜像](lazy-image-startup.md)分别测量同一 host staged 执行器的文件加载，以及 Docker / lazy VM 的 shell、NumPy 应用启动。同一 stage 的四文件校验中，pVisor 文件级路径的冷、热客户端中位等待都短于 OverlayBD；历史 VM 对照支持冷客户端选择 lazy、热客户端选择 Docker。两类实验使用预准备的本机服务，分别记录文件内容、元数据和总响应，保留各自的缓存、执行边界与预算，不合并成统一排名。

## 实验数据和分析 {#results}

2026-10-06 的启动、文件系统和修复共 1,440 个有效样本，完整审查流程 360 个；2026-10-07 的独立 stock 内核启动对照为 240 个有效样本，失败均为 0。2026-10-12 所选冻结源码文件系统对照另有 240 个有效样本、失败 0，使用每任务独立缓存与统一父组资源约束。各批次不合并，中位数差异和 95% 配对 bootstrap 区间见各专题。

分层文件系统对照独立执行505个环境，501个通过、4个资源单元超时，8个后续样本按协议未执行；完整任务与资源边界分别报告。

| 问题 | 实测水位（耗时为 P50） |
|---|---|
| [Startup](startup.md) | stock 对照：VM 100.91 ms；Firecracker 285.04 ms；QEMU microvm 321.56 ms；q35 702.98 ms |
| [Repair completion](agent-tasks.md) | staged 0.64 s; Docker 0.81 s; VM 3.25 s; QEMU microvm 1.40 s |
| [分层文件系统任务](filesystem.md#complete-task) | N=3，三轮发布：VM 11.271 s；VM + writeback 5.928 s；Docker 2.053 s；Firecracker 4.305 s；QEMU q35/microvm 4.065/3.851 s |
| [Review workflow](supervision-cost.md) | 10,000 files: stage 141 ms; Git 248 ms; reflink 343 ms |
| [网络](network.md) | 八线程小请求：原生 0.69 ms；host proxy 10.09 ms；VM 2.10 ms |
| [活跃容量](density.md) | 2 GiB：stage/Podman 32 路、VM 16 路通过全部五轮 |


网络为 510 个有效批次；容量扫描保留全部 320 个批次，包括 57 个失败。两个实验分别统计，不能合并成一种容量或速度排名。退役 Controller/Worker 结果是[历史证据](cluster-scalability.md)，不进入当前比较或 daemon 容量规划；daemon 吞吐、密度与历史成本均未测。

内存压缩带来的净物理内存与有效任务密度、裁剪内核收益、完整 Ubuntu 和 macOS 对照尚未完成当前制品验证；没有相应优势结论。云端、gVisor/Kata 和完整 RL 吞吐没有同条件排名。合入、网络、隔离和回放的专题需按各自证据范围判断，不能由这里的短任务推导。

[Network](network.md) · [Apply](supervision-cost.md#apply-cost) · [Density](density.md) · [VM memory](vm-memory/index.md) · [Isolation](isolation-tests.md) · [Replay](replay-fidelity.md)

[端到端任务](agent-tasks.md)结合工具等待、工作区生命周期与 CLI 兼容性分析 Agent 沙箱、容器、VM 和云端的取舍。[强化学习训练](compare-rl-infra.md)把工具成本、活跃容量与历史前缀准备映射到 rollout；完整训练吞吐与 reward 未测。

### 新框架的复测覆盖 {#measurement-coverage}

下表区分新测量与历史证据。仅当指标、输入、配置和正确性门禁一致时才更新相应数据；Ready、完整退出、工具内部计时、内存与正确性互不替代。未准备的实验不会产生新数字，历史结论只适用于原批次。

{{ table("measurement-coverage.render.json", {"markdown":true}) }}

文件系统覆盖对应九配置、19项负载的N3协议，四个资源单元不完整；历史N5/N30不填补新协议的缺失。

### 来源与复现 {#run}

[九配置分层统计](../../../overrides/assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-levels.csv) · [Ready](../../../overrides/assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-ready.csv) · [来源摘要](../../../overrides/assets/benchmarks/filesystem-layered-references/provenance.json)

其他主题的完整 CSV/JSON 和原始记录保存在对应本地批次或文档数据归档。

[Method](methodology.md) · [Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
