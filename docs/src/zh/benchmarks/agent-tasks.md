# 端到端 Agent 任务：什么时候选择 pVisor？

## 主要结论 {#conclusions}

**需要频繁创建任务视图、保留少量改动并审查合入时，评估 pVisor staged：固定修复任务为 0.64 s，Docker 为 0.81 s；但七项工具任务 staged 为 1.09 s，Docker 为 0.82 s。pVisor VM 的修复为 3.25 s，长于 Firecracker 的 2.20 s 和 QEMU microvm 的 1.40 s，独立 guest 边界需要额外预算。**

| 需求 | 选型含义 |
| --- | --- |
| 大工作区、稀疏改动、每次新建任务视图 | staged 有完整机器流程优势；同时预算工具开销 |
| 小工作区、现成容器或原生 Agent 沙箱 | 保留已有 Git/工具流程，不为单项速度全面替换 |
| 独立 guest 内核 | 选择 VM 前验证 CLI 兼容性和完整工具成本 |
| 远端 API 与托管弹性 | 按仓库同步、部署和运维需求评估云端；同条件性能与账单未测 |

## Motivation {#motivation}

一次任务包括环境准备、模型与工具交互、测试验证、审查合入和清理。只看启动会漏掉工具与结果处理；只看工具速度，又会漏掉反复创建工作区的成本。固定工具计划排除推理和公网波动，独立的完整审查流程衡量同样改动如何进入原工作区。

## 实验设计 {#interpretation}

Linux x86_64，AMD Ryzen 7 9700X，Fedora 7.2.8-200.fc44.x86_64。执行进程树与专用 Docker daemon 固定到 CPU 0,1；VM 为 2 vCPU，shell 探针配置 128 MiB，工具任务配置 16 GiB。原生/Docker 未限制内存，因此是 CPU 控制的任务对照，不能推导相同内存预算下的容量。host/staged 使用 rootless_process。

同一套离线工具与固定输入，每次新建工作区；热缓存、3 次预热、固定计划每格 60 次正式采样；环境检查与真实 CLI 每格 30 次，固定种子随机交替执行。环境准备、构建、镜像导入和输入重置不计时；启动和退出计入完整任务。Docker Engine 29.7.2 使用专用 rootless **overlay2** daemon、经典镜像存储和可写 bind mount；Firecracker 1.13.1 PCI 不使用 jailer，QEMU 10.2.2 分别使用 q35/microvm 与私有 ext4。pVisor VM 使用 virtio-fs 和自己的固件。内核、存储和暂存语义不同，结果是这些配置下的任务成本，不是纯 VMM 或安全排名。

固定计划检查和搜索仓库、修复 Python、执行 Python/Rust/Node 测试、安装 32 个离线 npm 包并生成 diff。Result 截止到校验后的结果返回，Completion 包括退出；测试和预期改动必须通过。该工具计划没有测量真实模型推理，也不能证明真实 Agent CLI 的兼容性。

这些样本未验证 Node/npm 编译缓存跨后端一致。新工作区不等于空工具缓存；完整任务的差值包含各配置下的缓存行为，不能全部归因于 stage 或 VMM。

场景分析还引用[文件系统](filesystem.md)、[完整审查流程](supervision-cost.md)、[启动](startup.md)、[惰性镜像](lazy-image-startup.md)和[隔离有效性](isolation-tests.md)的独立注册实验。审查流程每格 30 次，计时含视图创建、20 次修改、审查、合入 10 个文件与清理；Git/reflink 不提供同一隔离。各实验分别统计，不能相加中位数估算一次真实 Agent 任务，也不沿用它们的资源配置做统一排名。

## 实验数据和分析 {#results}

测于 2026-10-06，固定计划每个后端 60/60 有效，正式失败 0；环境与 CLI 的样本数和预检失败另列。输出、退出和执行器记录必须通过校验；暂存模式还验证宿主原文件不变和完整改动保留。保留所有有效慢样本，没有按耗时剔除。表格通常为 P50；分离分布展示各簇中位数和数量，P95 仅作观察参考。原始报告、二进制、输入与源码摘要保存在忽略的 `.data/`，公开 CSV 保留负载、批次和来源关联。

### 固定修复与测试 {#reference-env}

| Runtime | Valid / failed | Result P50 s | Completion P50 s | Completion P95 s |
| --- | --- | --- | --- | --- |
| Native | 60 / 0 | 0.43 | 0.44 | 0.46 |
| pVisor host | 60 / 0 | 0.44 | 0.46 | 0.48 |
| pVisor staged | 60 / 0 | 0.56 | 0.64 | 0.67 |
| pVisor VM | 60 / 0 | 3.12 | 3.25 | 3.29 |
| Docker rootless / overlay2 | 60 / 0 | 0.74 | 0.81 | 0.83 |
| Firecracker PCI | 60 / 0 | 2.15 | 2.20 | 2.24 |
| QEMU q35 | 60 / 0 | 1.41 | 1.46 | 1.48 |
| QEMU microvm | 60 / 0 | 1.35 | 1.40 | 1.42 |

staged 相对 Docker 的完整任务中位数差为 −175.84 ms，95% 配对 bootstrap 区间 [−178.85, −174.55] ms。VM 相对 QEMU microvm 为 +1849.00 ms，区间 [+1834.43, +1855.78] ms。该优势限于这套已准备的修复负载；不代表所有工具或并发吞吐。

### 工具速度与完整改动流程的取舍 {#workflow-tradeoffs}

前三个工具行来自 2026-10-06 的任务/文件系统实验；审查行来自同日独立实验。单位、P50 与每格样本数分别标注，各行只比较同一实验内的配置。

| 负载 | pVisor P50 | 对照 P50 | 每格 N | 选型含义 |
|---|---:|---:|---:|---|
| 固定修复到退出 | 0.64 s | Docker 0.81 s | 60 | 这套修复计划 staged 等待较短 |
| 七项工具到退出 | 1.09 s | Docker 0.82 s | 60 | 文件密集工具开销可能抵消短启动 |
| 七项工具到退出 | VM 4.27 s | QEMU microvm 1.50 s | 60 | guest 边界不能只按 shell-ready 选型 |
| 10,000 文件中改 20、合入 10，含视图创建/清理 | 140.82 ms | Git worktree 248.10 ms；reflink 343.00 ms | 30 | 大工作区的整树处理成本超过 stage 的额外开销 |
| 100 文件中改 20、合入 10，含视图创建/清理 | 109.02 ms | Git worktree 22.02 ms；reflink 23.66 ms | 30 | 小工作区原生流程更快 |
| 已准备的 20 文件视图，只审查/合入/丢弃 | 27.71 ms | Git worktree 4.69 ms | 30 | 视图准备完成后，stage 无单独审查速度优势 |

[七项工具](filesystem.md#complete-task)中 staged-minus-Docker 的中位数差为 +268.81 ms，95% CI [+264.64, +271.15] ms；[大工作区流程](supervision-cost.md#baseline-meaning)中 stage-minus-Git 为 −107.27 ms，95% CI [−109.02, −105.60] ms。小工作区为 +86.99 ms，95% CI [+77.28, +87.57] ms。优势来自负载和流程选择，不能推导全面替代 Docker、worktree 或 VMM。完整流程只合入 10 个文件；大量合入的额外成本见 [apply](supervision-cost.md#apply-cost)。机器计时不包含人的阅读时间。

[启动](startup.md)与[惰性镜像](lazy-image-startup.md)分别回答环境已准备和客户端镜像未缓存的等待。冷客户端数据不含首次上游下载、解包和索引；热客户端是 Docker 更快。镜像按需读取不能消除后续仓库工具开销。

### 如何接入现有工具链 {#existing-workflows}

- **原生 Agent 沙箱：** 保留客户端权限与审批机制；需要跨 Agent 统一暂存、按路径合入和记录时再评估 pVisor。内置沙箱结合 Git/worktree 也能审查改动。下方 CLI 实测使用受控模式，默认双层沙箱兼容性未测。能力说明见 [Claude Code](https://code.claude.com/docs/en/sandboxing)、[Codex](https://developers.openai.com/codex/security/)、[Gemini CLI](https://geminicli.com/docs/cli/sandbox/)。
- **Docker / devcontainer：** 依赖环境与现有 Git 流程可以继续使用；可写 bind mount 直接修改宿主，需自行组织独立工作区和恢复。所测 staged/safe/VM 在路径与 Unix socket fixture 中各 3/3 保留暂存并阻止列出的视图外访问，OCI/Podman 的授权可写工作区则写穿，见[隔离矩阵](isolation-tests.md)。这不证明内核逃逸防护或远端副作用可回滚。
- **Firecracker / QEMU / gVisor / Kata：** 先确定 guest/容器边界，再预算完整任务；上述修复对照的内核、文件系统与加固不同。原版内核 shell-ready 与工具批次独立，gVisor/Kata 同条件任务未测，不作安全或速度统一排名。
- **云端沙箱：** [E2B](https://docs.e2b.dev/)、[Daytona](https://www.daytona.io/docs/en/)、[Modal](https://modal.com/docs/guide/sandboxes)可按远端环境和托管容量需求评估。真实任务需计环境构建、仓库上传、依赖缓存、执行、结果下载及本地合入；本机数据不能替代云端区域延迟、账单或可用性测试。模型费用、本地硬件和运维也属于成本；长期复用环境会改变准备成本的摊销。

### 真实 CLI 兼容性 {#cli-compatibility}

环境检查运行 Python、Node、Git、Cargo/Rustc 与 CLI 版本检查；Claude Code 2.1.128 和 Codex 0.160.0 则实际启动客户端，通过本地确定性模型响应执行修复与测试，并校验工具结果返回客户端。各可用条件 30/30 有效、正式失败 0，3 次预热；单位秒，统计量为启动到退出的 P50，尾延迟不作结论。

| Runtime | Environment P50 s | Claude Code P50 s | Codex P50 s |
| --- | --- | --- | --- |
| Native | 0.15 | 0.77 | 1.86 |
| pVisor host | 0.17 | 0.78 | 1.87 |
| pVisor staged | 0.19 | 0.99 | 2.14 |
| pVisor VM | 1.10 | — | 9.01 |
| Docker rootless / overlay2 | 0.44 | 1.11 | 6.00 |
| Firecracker PCI | 1.50 | 2.98 | 7.72 |
| QEMU q35 | 0.88 | 2.07 | 6.93 |
| QEMU microvm | 0.80 | 2.05 | 6.86 |


Claude Code 在 pVisor VM 的预检中超过 90 s 初始化期限：有效样本为 0，此条件不参加耗时比较。其他 Claude 条件与所有 Codex 条件完成了受控工具闭环。

staged 相对 Docker 的完成时间中位数差：Claude Code 为 −127.83 ms，95% 配对 bootstrap 区间 [−137.91, −116.72] ms；Codex 为 −3864.45 ms，区间 [−3948.16, −3804.45] ms。这些差异包含客户端初始化、工具调用和退出等待，不能归因为单一文件系统成本。

Claude 使用 `--bare`、只允许 Bash；Codex 使用 `--ephemeral` 和 `danger-full-access`，隔离由外层执行器提供。没有真实推理或公网请求，因此结果适用于这些受控配置，不代表默认客户端沙箱、模型质量或真实服务的总耗时。完整 Ubuntu 部署仍未完成当前制品实测。

<a id="full-ubuntu"></a>

### 数据下载与复现 {#run}

[场景证据 CSV](task-scenarios.csv) · [整理后的统计 CSV](agent-tasks.csv) · [全部运行时统计](runtime-summary.csv) · [差异与 95% 置信区间](runtime-comparisons.csv) · [源码与制品来源](runtime-provenance.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
