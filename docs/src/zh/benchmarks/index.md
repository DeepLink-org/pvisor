# pVisor 适合哪些场景？

## Motivation {#motivation}

选执行方式看的是完成同样结果的总成本。大工作区、稀疏改动用 stage；小工作区或只关心工具运行，用原生流程；VM 只在完整任务测量支持时才选。按决策匹配证据：选本地执行方式看任务对照，选独立 guest 内核看启动与工具数据，定容量看成功率和同口径的资源占用。

10,000 个文件中修改 20 个、保留 10 个，stage 为 141 ms，Git worktree 为 248 ms，btrfs reflink 为 343 ms；小工作区上原生流程更快。启动、工具执行、合入和资源占用回答不同问题，不能相互替代。

<!-- measurement-status:start -->

文件系统覆盖九配置、19 项分层负载，每个单元三个独立环境、每环境三轮；实际执行、通过、失败与未执行的计数见[文件系统](filesystem.md)。历史 N5/N30 与一次性任务独立保留，其他主题使用各自批次。

<!-- measurement-status:end -->

## 实验设计 {#interpretation}

测量产出一个通过校验结果所需的启动、工具执行、合入和资源占用，作为该结果的成本。这个成本决定哪种执行模式适合一个负载；同样结果下成本越低，该负载运行得越便宜。

这些数字来自独立注册的实验。启动与修复每后端 60 个样本、3 次预热；文件系统为九配置、19 项负载，每个单元三个独立环境、每环境三轮；审查流程 30 个样本、3 次预热；网络为 30 个独立批次，小请求取 256 次请求的中位数；容量在共同两核、2 GiB、零 swap 预算下每并发度跑五轮。所有组共用同一宿主与 CPU 0–1，缓存状态按主题设定，随机交替执行。

有效计时都通过正确性校验；失败和未执行的样本单独列出，不按速度剔除。批次不合并。P95 仅作观察参考，少于 30 个样本不展示；不展示公开 P99；分离簇按比例和各簇中位数报告。

### 隔离与资源口径 {#reference-resources}

Docker 使用 rootless overlay2，Firecracker 与 QEMU 使用私有 ext4，pVisor VM 使用 virtio-fs 且内核不同：这些是完整配置的成本，不是纯 VMM 或安全排名。工程 A/B 留在实验记录，完整数据留在本地批次。

## 实验数据和分析 {#results}

2026-10-06 的启动、文件系统和修复共 1,440 个有效样本，完整审查流程 360 个；2026-10-07 的独立 stock 内核启动对照为 240 个有效样本，失败均为 0。2026-10-11 重启后的启动全条件复测为当前批次（数字见[启动](startup.md)），历史配对批次不沿用。2026-10-12 所选冻结源码文件系统对照另有 240 个有效样本、失败 0，使用每任务独立缓存与统一父组资源约束。中位数差异和 95% 配对 bootstrap 区间见各专题。

| 问题 | 实测水位（耗时为 P50） |
|---|---|
| [Startup](startup.md) | 2026-10-11 复测：VM 130.71 ms；Firecracker MMIO 287.13 ms；QEMU microvm 260.10 ms；QEMU 663.25 ms |
| [Repair completion](agent-tasks.md) | staged 0.64 s; Docker 0.81 s; VM 3.25 s; QEMU microvm 1.40 s |
| [分层文件系统任务](filesystem.md#complete-task) | N=3，三轮发布：VM 11.271 s；VM + writeback 5.928 s；Docker 2.053 s；Firecracker 4.305 s；QEMU q35/microvm 4.065/3.851 s |
| [Review workflow](supervision-cost.md) | 10,000 files: stage 141 ms; Git 248 ms; reflink 343 ms |
| [网络](network.md) | 八线程小请求：原生 0.69 ms；host proxy 10.09 ms；VM 2.10 ms |
| [活跃容量](density.md) | 2 GiB：stage/Podman 32 路、VM 16 路通过全部五轮 |

网络为 510 个有效批次；容量扫描保留全部 320 个批次，包括 57 个失败。两个实验分别统计，不能合并成一种容量或速度排名。daemon 吞吐、密度与历史成本均未测。

内存压缩带来的净物理内存与有效任务密度、裁剪内核收益、完整 Ubuntu 和 macOS 对照尚未在当前制品上验证；没有相应优势结论。云端、gVisor/Kata 和完整 RL 吞吐没有同条件排名。合入、网络、隔离和回放的专题需按各自证据范围判断，不能由这里的短任务推导。

[端到端任务](agent-tasks.md)结合工具等待、工作区生命周期与 CLI 兼容性分析 Agent 沙箱、容器、VM 和云端的取舍。[强化学习训练](compare-rl-infra.md)把工具成本、活跃容量与历史前缀准备映射到 rollout；完整训练吞吐与 reward 未测。

### 已准备环境 {#reference-env}

启动、文件系统与任务对照共用同一套已准备工具环境，每张表都按摘要固定其制品。表格描述的是当轮固定的制品，不代表第三方当前发布版。

### 完整发行版 {#full-ubuntu}

Ubuntu 使用发行版内核、initrd、systemd 和私有磁盘；pVisor 复用已准备工具目录。该对照回答部署等待，不隔离纯 VMM 成本。

### QEMU 完整发行版 {#full-ubuntu-qemu}

q35 和 microvm 使用同一 Ubuntu 模板；样本和百分位数与其他批次分开。

### 任务与资源 {#product-v1}

合入、网络、隔离与回放使用各自的任务集和批次，都不继承上表的数字。

### 工程实验与数据保留 {#evidence-format}

工程 A/B、带计数器的 profile 和诊断探针保存在[技术分析](../lab-notes/filesystem-performance-analysis.md)，不作为跨产品主表。

Markdown 保留面向用户的表格，仅为必要的用户统计提供共享 CSV 下载；正文已有的静态表、逐轮逐环境记录以及历史和工程中间表不默认另行导出。完整 CSV/JSON、原始报告、逐次样本、日志、制品清单和冻结 harness 保存在对应本地批次或 `benchmark.new/.data/docs-assets-prune-20261018/` 文档数据归档，不发布到站点。动态 JSON 只保留构建所需的展示表，公开工程图使用 SVG。复现与来源保留规则见[运行手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)。

### 新框架的复测覆盖 {#measurement-coverage}

下表区分新测量与历史证据。仅当指标、输入、配置和正确性门禁一致时才更新相应数据；Ready、完整退出、工具内部计时、内存与正确性互不替代。未准备的实验不会产生新数字，历史结论只适用于原批次。

{{ table("measurement-coverage.render.json", {"markdown":true}) }}

文件系统覆盖对应九配置、19项负载的N3协议，四个资源单元不完整；历史 N5/N30 数据不填补新协议的缺失。

### 来源与复现 {#run}

[九配置分层统计](../../../overrides/assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-levels.csv) · [Ready](../../../overrides/assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-ready.csv) · [来源摘要](../../../overrides/assets/benchmarks/filesystem-layered-references/provenance.json)

其他主题的完整 CSV/JSON 和原始记录保存在对应本地批次或文档数据归档。

[Reproduction manual](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
