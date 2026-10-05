# 文件系统与开发工具性能

## 主要结论 {#conclusions}

**pVisor staged 的离线 npm 安装与 Docker 接近，大块读取的额外等待较小；小文件和元数据操作则明显更慢。** 读取并校验 64 MiB，staged 约 **48 ms**、Docker **33 ms**；离线 npm 安装约 **223–256 ms**、Docker **231 ms**。遍历、创建文件、Git 和搜索的差距更大，是当前文件系统的主要短板。

**pVisor VM 的性能取决于负载：大块读取和小型 Cargo 编译快于所测完整 Ubuntu VM，但多数文件密集型操作更慢。** VM 读取 64 MiB 约 **89 ms**，Firecracker/Ubuntu **116 ms**；Cargo 约 **0.55–0.56 s**，Ubuntu **0.97 s**。遍历、搜索、写小文件与 npm 则明显落后，整体文件访问也慢于 Docker bind mount。

需要暂存、审查和选择性合入时，staged 的工具预算更低；需要独立 guest kernel 时，可以选择 VM，但要考虑小文件操作的累计等待。

## Motivation {#motivation}

开发工具会反复查询目录和属性、打开文件、安装依赖并生成构建产物。用户选型需要知道 pVisor 与熟悉的容器和 VM 在这些任务上的实际差距，既要看速度，也要看改动是否暂存、执行边界是否满足需求。

## 实验设计 {#interpretation}

选用完整工具环境中的两组横向对照：Docker 使用 rootless Engine 和 writable bind mount；Firecracker 使用完整 Ubuntu、发行版 generic 内核、initrd、systemd 与私有 ext4。pVisor 使用工具目录和 staged 文件视图。Linux 同机、两核预算，VM 为 2 vCPU / 16 GiB、热宿主缓存；Docker 对照每格 30 次，Ubuntu 对照每格 10 次，均 3 次预热。

七项操作使用固定输入，计时包含工具运行和结果校验，排除环境开机、镜像准备与下载。目录遍历包含 2,048 个文件、32 个子目录；读取校验 64 MiB SHA256，写入 256 个文件；Cargo 编译 64 个无外部依赖模块，npm 安装 32 个本地包。

**下表的范围是两种完整工具环境配置各自的 P50，表示配置差异，不是样本波动或置信区间。** Docker 和 Ubuntu 来自独立对照，不合并样本，也不将不同工具版本、内核和文件路径的差值全部归因于文件系统。制品摘要和完整 P95/P99 见原始报告；目录与 lazy 的细分测量见[技术分析](../design/filesystem-performance-analysis.md)。

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

### 与 Docker 相比 {#reference-fs}

Docker 的文件操作接近原生。pVisor staged 的 npm 安装处于相近水平，64 MiB 读取多约 **15 ms**，Cargo 约为 Docker 的 **2 倍**。但遍历约 **180 ms**，Docker 约 **5 ms**；写 256 个文件约 **188 ms**，Docker 约 **4 ms**。频繁扫描仓库、生成大量文件或反复运行 Git/rg 时，这些成本会累积。

pVisor VM 的读取约为 Docker 的 **2.7 倍**，npm 安装约 **1.7–2.3 s**，Docker 约 **0.23 s**。因此，单纯追求文件与工具执行速度，所测 Docker bind mount 更有优势。Docker 直接修改宿主挂载文件，pVisor 保留改动到 apply；这项工作流差别也应纳入选择。

### 与完整 Ubuntu VM 相比 {#full-ubuntu}

pVisor VM 的大块读取和小型 Cargo 编译更快，说明工具性能不能用一个统一倍数概括。与此同时，遍历约 **0.29–0.31 s**，Ubuntu 约 **19 ms**；搜索约 **0.52–0.55 s**，Ubuntu 约 **20 ms**。在 Ubuntu 同组对照中，pVisor VM 的写小文件约 **134 ms**、Ubuntu **36 ms**，npm 约 **2.26 s**、Ubuntu **1.08 s**。

对安装依赖、Git 和仓库扫描较多的任务，pVisor VM 仍有明显性能代价。需要频繁新建环境时，还应结合[启动等待](startup.md)与[完整任务耗时](agent-tasks.md)；已经常驻的环境更应关注这里的工具内部时间。

### 适用范围 {#acceptance}

这些是小型、离线、热缓存开发负载。大型仓库、真实 npm registry、冷磁盘和多任务吞吐未覆盖；Docker writable layer、overlay2、Docker Desktop 及对应 macOS 文件负载也没有同条件结果。[隔离验证](isolation-tests.md)说明不同配置的实际边界。

### 数据来源 {#run}

[Docker 对照汇总](../../assets/benchmarks/reference-env-20261004/summary.tsv) · [Docker 对照样本](../../assets/benchmarks/reference-env-20261004/samples.csv) · [Ubuntu 对照汇总](../../assets/benchmarks/full-ubuntu-20261004/summary.tsv) · [Ubuntu 对照样本](../../assets/benchmarks/full-ubuntu-20261004/samples.csv) · [方法与制品](methodology.md) · [细分测量与优化分析](../design/filesystem-performance-analysis.md)
