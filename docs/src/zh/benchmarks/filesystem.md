# 文件系统与开发工具性能

## 主要结论 {#conclusions}

**FUSE 无 stage 对照比同批 staged 更快，但元数据操作仍明显慢于原生。** 通过 FUSE 直接读写宿主文件，遍历约 **22.04 ms**、读取 64 MiB 约 **39.17 ms**、写入 256 个文件约 **12.52 ms**；同批 staged 分别为 **78.06、68.56、206.40 ms**。这是专门的 passthrough 基准适配器，不是 host 直访，也不是现有 CLI 内置模式。

**历史 Docker 对照中，pVisor staged 的离线 npm 安装与 Docker 接近，大块读取的额外等待较小；小文件和元数据操作则明显更慢。** 读取并校验 64 MiB，staged 约 **48 ms**、Docker **33 ms**；离线 npm 安装约 **223–256 ms**、Docker **231 ms**。遍历、创建文件、Git 和搜索的差距更大，是这些负载的主要短板。

**pVisor VM 的性能取决于负载：大块读取和小型 Cargo 编译快于所测完整 Ubuntu VM，但多数文件密集型操作更慢。** VM 读取 64 MiB 约 **89 ms**，Firecracker/Ubuntu **116 ms**；Cargo 约 **0.55–0.56 s**，Ubuntu **0.97 s**。遍历、搜索、写小文件与 npm 则明显落后，整体文件访问也慢于 Docker bind mount。

需要暂存、审查和选择性合入时，staged 的工具预算更低；需要独立 guest kernel 时，可以选择 VM，但要考虑小文件操作的累计等待。

## Motivation {#motivation}

开发工具会反复查询目录和属性、打开文件、安装依赖并生成构建产物。用户选型需要知道 pVisor 与熟悉的容器和 VM 在这些任务上的实际差距，既要看速度，也要看改动是否暂存、执行边界是否满足需求。

## 实验设计 {#interpretation}

历史横向对照使用完整工具环境：Docker 使用 rootless Engine 和 writable bind mount；Firecracker 使用完整 Ubuntu、发行版 generic 内核、initrd、systemd 与私有 ext4；pVisor staged 和 VM 使用 staged 文件视图。Linux 同机、两核预算，VM 为 2 vCPU / 16 GiB、热宿主缓存；Docker 对照每格 30 次，Ubuntu 对照每格 10 次，均 3 次预热。

七项操作使用固定输入，计时包含工具运行和结果校验，排除环境开机、镜像准备与下载。目录遍历包含 2,048 个文件、32 个子目录；读取校验 64 MiB SHA256，写入 256 个文件；Cargo 编译 64 个无外部依赖模块，npm 安装 32 个本地包。

**下表的范围是两种完整工具环境配置各自的 P50，表示配置差异，不是样本波动或置信区间。** Docker 和 Ubuntu 来自独立对照，不合并样本，也不将不同工具版本、内核和文件路径的差值全部归因于文件系统。制品摘要和完整 P95/P99 见原始报告；目录与 lazy 的细分测量见[技术分析](../design/filesystem-performance-analysis.md)。

另列 2026-10-05 的 FUSE 无 stage 同批对照：原生、host 直访、host + FUSE passthrough、host staged 四组，各 3 次预热、30 次测量，每轮随机交错、串行执行。三组 pVisor 使用同一固定 release 制品，均验证 `rootless_process`；七项负载不变。它与下面的历史 Docker/Ubuntu 对照使用不同制品，结果分表呈现，不混合样本。

## 实验数据和分析 {#results}

### pVisor 与 Docker、完整 Ubuntu VM 的横向对比 {#filesystem-service}

单位：**P50 ms，越低越快**。

| 操作 | 原生 | pVisor staged | pVisor VM | Docker bind mount | Firecracker / Ubuntu |
|---|---:|---:|---:|---:|---:|
| 遍历 2,048 个文件 | 4.85–5.04 | 177.80–180.13 | 291.82–310.54 | 5.06 | 18.64 |
| 读取并校验 64 MiB | 33.07–33.29 | 48.12–48.77 | 88.83–89.27 | 33.24 | 115.51 |
| 写入 256 个文件 | 3.75–3.96 | 186.95–189.28 | 134.37–144.66 | 3.95 | 36.07 |
| git status | 14.66–15.75 | 172.18–177.81 | 456.21–613.69 | 16.07 | 136.50 |
| rg 搜索 | 7.58–7.98 | 140.52–144.61 | 521.65–545.82 | 8.03 | 20.40 |
| Cargo 离线编译 | 52.79–58.71 | 104.21–112.80 | 549.57–563.24 | 56.40 | 969.09 |
| npm 离线安装 | 183.46–218.82 | 222.97–256.29 | 1727.04–2260.17 | 231.45 | 1079.89 |

### FUSE 无 stage 与 staged 同批对照 {#host-direct}

单位：**P50 ms，越低越快**。以下四列来自同一批次，每格 30 个通过正确性校验的样本。

| 操作 | 原生 | pVisor host 直访（无 FUSE） | pVisor host + FUSE（无 stage，对照适配器） | pVisor host staged |
|---|---:|---:|---:|---:|
| 遍历 2,048 个文件 | 4.88 | 4.88 | 22.04 | 78.06 |
| 读取并校验 64 MiB | 32.83 | 32.18 | 39.17 | 68.56 |
| 写入 256 个文件 | 4.12 | 4.07 | 12.52 | 206.40 |
| git status | 14.99 | 15.12 | 40.49 | 173.48 |
| rg 搜索 | 8.39 | 7.70 | 28.07 | 91.38 |
| Cargo 离线编译 | 56.99 | 57.36 | 73.56 | 117.28 |
| npm 离线安装 | 176.99 | 177.18 | 182.28 | 268.46 |

FUSE 无 stage 组先由 `filesystem_fuse_passthrough.rs` 挂载一次性工作区，再在挂载点内执行 `pvisor run --no-agent-defaults --overlaynet off`，不传入 `--stage`。读写经用户态 FUSE 请求处理后落到 backing 目录；适配器不使用 OverlayCore、copy-up、访问策略或 preimage journal，改动直接写回，不提供 apply/drop 工作流。它用于测量 FUSE 数据路径的对照成本，不能当作已有的生产功能。

适配器与 staged 使用相同冻结版本的 `fuser 0.15.1`、ABI 7.31、默认初始化协议能力、单线程同步请求循环、1 秒 entry/attr TTL 和 RW/NoAtime/DefaultPermissions 挂载选项；不启用 writeback cache、keep-cache 或内核 backing-FD passthrough。两者的文件系统实现和语义不同，因此不是只切换一个 stage 开关的严格实验。

30 个 FUSE 样本都保留了真实 mountinfo、LOOKUP/READ/WRITE 请求计数，每次实际读取超过 **64 MiB**、写入超过 **16 MiB**；Run Bundle 均为 `rootless_process`、`filesystem_changes_staged=false`。执行时还校验写回 backing 的 256 个文件名称和大小，排除绕过 FUSE 或未完成写入的样本。

遍历、Git 和搜索说明 FUSE 无 stage 仍有元数据成本；staged 的写入约为该对照的 **16.5 倍**，说明仅靠 FUSE 往返不能解释全部差距。额外成本还包含不同适配器实现、copy-up、策略和日志语义，需要细分测量才能归因。表中只比较工具与校验耗时，排除挂载、Job 启动和卸载；原始报告的 completion 时间包含这些生命周期步骤，不能与历史批次的启动数据直接拼接。

### 与 Docker 相比 {#reference-fs}

Docker 的文件操作接近原生。pVisor staged 的 npm 安装处于相近水平，64 MiB 读取多约 **15 ms**，Cargo 约为 Docker 的 **2 倍**。但遍历约 **180 ms**，Docker 约 **5 ms**；写 256 个文件约 **188 ms**，Docker 约 **4 ms**。频繁扫描仓库、生成大量文件或反复运行 Git/rg 时，这些成本会累积。

pVisor VM 的读取约为 Docker 的 **2.7 倍**，npm 安装约 **1.7–2.3 s**，Docker 约 **0.23 s**。因此，相比历史 staged 和 VM，所测 Docker bind mount 的文件与工具执行速度更有优势。Docker 直接修改宿主挂载文件，pVisor staged 将改动保留到 apply；这项工作流差别也应纳入选择。本次 FUSE 无 stage 组没有同批 Docker 测量，不据此更新 Docker 倍数。

### 与完整 Ubuntu VM 相比 {#full-ubuntu}

pVisor VM 的大块读取和小型 Cargo 编译更快，说明工具性能不能用一个统一倍数概括。与此同时，遍历约 **0.29–0.31 s**，Ubuntu 约 **19 ms**；搜索约 **0.52–0.55 s**，Ubuntu 约 **20 ms**。在 Ubuntu 同组对照中，pVisor VM 的写小文件约 **134 ms**、Ubuntu **36 ms**，npm 约 **2.26 s**、Ubuntu **1.08 s**。

对安装依赖、Git 和仓库扫描较多的任务，pVisor VM 仍有明显性能代价。需要频繁新建环境时，还应结合[启动等待](startup.md)与[完整任务耗时](agent-tasks.md)；已经常驻的环境更应关注这里的工具内部时间。

### 适用范围 {#acceptance}

这些是小型、离线、热缓存开发负载。大型仓库、真实 npm registry、冷磁盘和多任务吞吐未覆盖；Docker writable layer、overlay2、Docker Desktop 及对应 macOS 文件负载也没有同条件结果。[隔离验证](isolation-tests.md)说明不同配置的实际边界。

### 数据来源 {#run}

[FUSE 无 stage 同批汇总](../../assets/benchmarks/filesystem-fuse-20261005/summary.tsv) · [逐项样本](../../assets/benchmarks/filesystem-fuse-20261005/samples.tsv) · [完整报告](../../assets/benchmarks/filesystem-fuse-20261005/report.tsv) · [构建来源](../../assets/benchmarks/filesystem-fuse-20261005/build-provenance.tsv) · [FUSE 挂载、请求日志与运行证明](../../assets/benchmarks/filesystem-fuse-20261005/evidence.tar.gz) · [校验清单](../../assets/benchmarks/filesystem-fuse-20261005/manifest.tsv)

[Docker 对照汇总](../../assets/benchmarks/reference-env-20261004/summary.tsv) · [Docker 对照样本](../../assets/benchmarks/reference-env-20261004/samples.csv) · [Docker 对照原始报告](../../assets/benchmarks/reference-env-20261004/report.tsv) · [归档命令与执行记录](../../assets/benchmarks/reference-env-20261004/evidence.tar.gz) · [Ubuntu 对照汇总](../../assets/benchmarks/full-ubuntu-20261004/summary.tsv) · [Ubuntu 对照样本](../../assets/benchmarks/full-ubuntu-20261004/samples.csv) · [方法与制品](methodology.md) · [细分测量与优化分析](../design/filesystem-performance-analysis.md)
