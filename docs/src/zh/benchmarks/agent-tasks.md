# 一次完整修复任务要多久？

## 主要结论 {#conclusions}

**固定修复任务到退出，pVisor staged 为 0.64 s，短于 Docker 的 0.81 s；pVisor VM 为 3.25 s，长于 Firecracker 的 2.20 s 和 QEMU microvm 的 1.40 s。优势随执行模式与负载变化。**

| 需求 | 选型含义 |
| --- | --- |
| 本机执行并保留改动 | 评估 host/staged 与完整审查流程 |
| 独立 guest 内核 | 同时预算启动和 VM 工具等待 |
| 并发或闲置环境 | 需要固定资源下的吞吐与物理内存实测 |

## Motivation {#motivation}

修复代码还需要搜索、安装依赖、运行测试和输出 diff。固定工具计划排除推理和公网波动，让你判断执行环境增加的等待。

## 实验设计 {#interpretation}

Linux x86_64，AMD Ryzen 7 9700X，Fedora 7.2.8-200.fc44.x86_64。执行进程树与专用 Docker daemon 固定到 CPU 0,1；VM 为 2 vCPU，shell 探针配置 128 MiB，工具任务配置 16 GiB。原生/Docker 未限制内存，因此是 CPU 控制的任务对照，不能推导相同内存预算下的容量。host/staged 使用 rootless_process。

同一套离线工具与固定输入，每次新建工作区；热缓存、3 次预热、固定计划每格 60 次正式采样；环境检查与真实 CLI 每格 30 次，固定种子随机交替执行。环境准备、构建、镜像导入和输入重置不计时；启动和退出计入完整任务。Docker Engine 29.7.2 使用专用 rootless **overlay2** daemon、经典镜像存储和可写 bind mount；Firecracker 1.13.1 PCI 不使用 jailer，QEMU 10.2.2 分别使用 q35/microvm 与私有 ext4。pVisor VM 使用 virtio-fs 和自己的固件。内核、存储和暂存语义不同，结果是这些配置下的任务成本，不是纯 VMM 或安全排名。

固定计划检查和搜索仓库、修复 Python、执行 Python/Rust/Node 测试、安装 32 个离线 npm 包并生成 diff。Result 截止到校验后的结果返回，Completion 包括退出；测试和预期改动必须通过。该工具计划没有测量真实模型推理，也不能证明真实 Agent CLI 的兼容性。

这些样本未验证 Node/npm 编译缓存跨后端一致。新工作区不等于空工具缓存；完整任务的差值包含各配置下的缓存行为，不能全部归因于 stage 或 VMM。

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

[整理后的统计 CSV](agent-tasks.csv) · [全部运行时统计](runtime-summary.csv) · [差异与 95% 置信区间](runtime-comparisons.csv) · [源码与制品来源](runtime-provenance.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
