# 惰性镜像能省多少启动等待？

## Motivation {#motivation}

Agent 频繁创建一次性环境，短任务往往只读取镜像中的一小部分，却要等完整拉取完成才能开始。pVisor 按需读取镜像文件：任务可以在下载完成前开始，后续运行复用已缓存的内容。

冷启动的 shell 客户端 Ready 为 186.1 ms，只读取 2.58 MB 内容，完整 Docker OCI 镜像为 41.84 MB。同一镜像的四文件对照中，pVisor 冷、热客户端的等待都短于 OverlayBD。完整镜像已缓存时，应用启动实验里 Docker 的启动等待仍更短，因为缓存命中后挂载和执行器启动开销依然存在。

## 实验设计 {#interpretation}

测量从启动到首条校验输出的时间，以及冷客户端实际取回的字节数，作为冷启动成本。冷启动成本是新环境在产出第一个结果前付出的等待与数据量；两项都越低，任务开始得越早、搬运的数据越少。

两个实验都在 Linux 上使用预先准备好的本机镜像服务。文件读取对照从同一个 Ubuntu 26.04 镜像读取并校验四个文件，在同一执行环境中比较 pVisor 与 OverlayBD，只改镜像加载方式。启动对照分别运行 Ubuntu shell 和 NumPy 脚本：Docker 走完整拉取，pVisor 在 VM 中按需读取；每次试验都新建容器或 VM。

两者都用两核跑任务、另两核跑镜像服务。Docker 限 2 GiB 内存，pVisor 为 2 GiB guest RAM，两者口径不同。冷客户端从空镜像缓存开始，热客户端复用已取回内容。3 轮预热后做 30 轮配对、路径顺序随机；Ready 为主指标，Completion 延伸到正常退出。冷客户端必须发生取回，热客户端不得重复取回；执行、校验或清理失败会使该批次无效。

结果适用于服务已准备好的本机短任务。首次服务准备、公网、大并发和批量文件 I/O 需另测；文件读取结果不能代替镜像内应用启动。

## 实验数据和分析 {#results}

惰性镜像按访问需要获取文件，让环境准备随任务推进。

### 按需获取对短任务启动的影响 {#application-comparison}

Shell 和 NumPy 分别测于 2026-10-10 的独立批次，每个条件 n=30，以 Docker 完整拉取为参照。表中的等待差为 Ready 中位数差；取数减少按截至 Completion 的应用层总响应正文计算。

| 任务 | 冷缓存：pVisor 少等（ms） | 冷缓存：pVisor 取数减少 | 热缓存：pVisor 多等（ms） |
|---|---:|---:|---:|
| Shell | 575.0 | 91.2% | 60.9 |
| NumPy | 396.6 | 65.4% | 339.2 |

冷客户端按需获取文件，两种任务均减少了响应载荷和中位等待。完整镜像已缓存时，Docker 启动更快。惰性加载的收益取决于任务读取多少内容，以及这些内容是否已在本地。

<details markdown="1">
<summary>查看启动时间分布、置信区间和完整取数统计</summary>

#### Ubuntu shell 客户端启动等待 {#startup}

测量日期 2026-10-10，每格 n=30、共 120 正式样本，零正式失败、零按速度剔除。单位 ms；非分离分布报告 P50，分离分布报告各簇中位数与样本比例。分簇原因未确定。

| 路径 / 缓存 | Ready：P50 或簇中位数（占比） | Ready P95 | Completion：P50 或簇中位数（占比） | Completion P95 |
|---|---:|---:|---:|---:|
| Docker 冷 | 597.2 (23/30); 1,563.5 (7/30) | 1,923.6 | 690.2 (25/30); 1,744.7 (5/30) | 2,032.0 |
| pVisor lazy 冷 | 186.1 | 256.5 | 312.5 | 381.2 |
| Docker 热 | 85.0 (24/30); 212.6 (6/30) | 225.5 | 113.6 (22/30); 280.3 (8/30) | 317.2 |
| pVisor lazy 热 | 151.0 | 184.2 | 267.3 | 310.0 |

按完整轮配对 bootstrap 5,000 次，报告 **Docker 减 lazy 的边际 Ready 中位数差**；n=30 对，单位 ms。区间描述总体中位数，不代表单个簇的中心或每次启动。

| 客户端缓存 | Ready 中位数差（ms） | 配对 95% CI（ms） |
|---|---:|---:|
| 冷 | 575.0 | [384.5, 841.3] |
| 热 | -60.9 | [-68.3, -30.4] |

#### 小型 Python/NumPy 脚本 {#numpy}

独立批次同样测于 2026-10-10，每格 n=30、共 120 正式样本，无失败或按速度剔除；单位 ms，沿用相同分布规则。

| 路径 / 缓存 | Ready：P50 或簇中位数（占比） | Ready P95 | Completion：P50 或簇中位数（占比） | Completion P95 |
|---|---:|---:|---:|---:|
| Docker 冷 | 1,001.5 (19/30); 2,233.3 (11/30) | 2,836.5 | 1,041.7 (19/30); 2,305.9 (11/30) | 2,877.1 |
| pVisor lazy 冷 | 778.0 | 1,086.1 | 925.0 | 1,307.3 |
| Docker 热 | 120.7 (24/30); 196.6 (6/30) | 211.5 | 161.9 (24/30); 271.1 (6/30) | 276.7 |
| pVisor lazy 热 | 462.2 | 521.3 | 606.8 | 677.0 |

同样报告 Docker 减 lazy 的边际 Ready 中位数差，n=30 对、5,000 次配对 bootstrap，单位 ms。

| 客户端缓存 | Ready 中位数差（ms） | 配对 95% CI（ms） |
|---|---:|---:|
| 冷 | 396.6 | [231.7, 1,055.3] |
| 热 | -339.2 | [-345.9, -332.4] |

这些配置下，冷客户端中位等待支持 lazy，热客户端支持 Docker。NumPy 导入需要更多文件，按需路径也会增加取数工作；Completion 还应计入 Job 收尾成本。中位数更小不承诺每次启动都更快。

#### 客户端载荷 {#transfer}

单位 bytes，计数截至 Completion，每格 n=30，条件内所有样本计数相同。

| 负载 / 路径 / 缓存 | 文件内容或 OCI blobs | 二进制 Metadata Data | 总响应 | 相比同负载冷 Docker 总响应减少 |
|---|---:|---:|---:|---:|
| Shell / docker / 冷 | 41,842,292 | 0 | 41,843,684 | 基准 |
| Shell / docker / 热 | 0 | 0 | 0 | — |
| Shell / lazy / 冷 | 2,580,417 | 1,117,945 | 3,702,376 | 91.2% |
| Shell / lazy / 热 | 0 | 0 | 641 | — |
| NumPy / docker / 冷 | 115,930,809 | 0 | 115,932,435 | 基准 |
| NumPy / docker / 热 | 0 | 0 | 0 | — |
| NumPy / lazy / 冷 | 36,957,665 | 3,085,982 | 40,072,915 | 65.4% |
| NumPy / lazy / 热 | 0 | 0 | 816 | — |

减少比例为 `1 − lazy 总响应 / Docker 总响应`，同负载仅比较冷客户端。Docker 内容是压缩层及 config，lazy 内容仅为解压后 Read Data。二进制索引页单列为 Metadata Data，并计入总响应；该列衡量应用响应正文，不代表完整网络流量。Docker 响应不含 HTTP headers/TLS，两路径均不含 TCP/IP 开销。热 lazy 仍有 Prepare/Ping。

</details>

### 按需读取与缓存复用 {#stage-comparison}

测于 2026-10-17，每个条件 n=30；120 次正式执行和 12 次预热全部通过校验，零失败、零剔除。这组实验只读取并校验四个文件，与应用启动批次分别统计。

| 客户端缓存 | pVisor Ready P50（ms） | OverlayBD Ready P50（ms） | pVisor 等待减少 |
|---|---:|---:|---:|
| 冷 | 78.15 | 233.29 | 66.5% |
| 热 | 52.15 | 161.19 | 67.6% |

pVisor 完成四文件校验的中位等待为冷缓存 78.15 ms、热缓存 52.15 ms。两种方案的热客户端均复用了文件内容。计时包含打开镜像、挂载和执行，差异来自整套加载实现。

<details markdown="1">
<summary>查看文件读取的完整时间、置信区间和取数统计</summary>

测量日期 2026-10-17，每格 n=30；120 次正式执行及 12 次预热全部通过独立审计，零失败、零剔除。单位 ms，均为非分离分布的 P50；不同日期与应用启动批次分别统计。

| 文件供给路径 / 客户端缓存 | Ready P50（ms） | Completion P50（ms） |
|---|---:|---:|
| pVisor host FUSE / 冷 | 78.15 | 99.97 |
| OverlayBD ext4 / 冷 | 233.29 | 384.27 |
| pVisor host FUSE / 热 | 52.15 | 83.62 |
| OverlayBD ext4 / 热 | 161.19 | 296.69 |

按轮配对 bootstrap 5,000 次，中位数差定义为 **pVisor 减 OverlayBD**，负值表示 pVisor 等待更短；等待减少比例为 `1 − pVisor P50 / OverlayBD P50`，由同批未舍入统计值计算。每格 n=30，差值单位 ms，减少比例单位 %；四个差值区间均不包含零。

| 客户端缓存 | 指标 | 中位数差及 95% CI（ms） | 等待减少及 95% CI（%） |
|---|---|---:|---:|
| 冷 | Ready | -155.14 [-171.64, -145.16] | 66.5 [63.2, 68.8] |
| 热 | Ready | -109.04 [-116.76, -103.18] | 67.6 [66.1, 69.0] |
| 冷 | Completion | -284.29 [-299.20, -268.76] | 74.0 [71.8, 75.0] |
| 热 | Completion | -213.08 [-234.06, -202.03] | 71.8 [70.4, 77.8] |

完成后观测的应用层响应如下，单位 bytes、每格 n=30，报告 P50；OverlayBD 冷内容量随实际 Range 取数变化。

| 文件供给路径 / 客户端缓存 | 文件内容载荷 P50（B） | 元数据载荷 P50（B） | 总响应 P50（B） |
|---|---:|---:|---:|
| pVisor host FUSE / 冷 | 2,575,738 | 921,337 | 3,500,548 |
| OverlayBD ext4 / 冷 | 4,292,610 | 1,502,328 | 5,794,938 |
| pVisor host FUSE / 热 | 0 | 0 | 619 |
| OverlayBD ext4 / 热 | 0 | 0 | 0 |

少量文件读取时，pVisor 的文件级路径在这组条件下同时减少了等待和内容获取。热客户端两边都没有再次下载文件内容，仍有打开、挂载及 stage 执行成本。pVisor 内容为解压后的 `Read Data`，OverlayBD 内容为原始压缩 OCI 层的 Range 响应；总响应分别按 cache 的帧与数据、registry 响应 body 计数，均不含 TCP/IP，registry 另不含 HTTP headers。这些计数不等于线上流量，也不推算 Ready 截点的传输量。

</details>

### 服务端缓存准备 {#preparation}

单位 s，每项 n=1，均在客户端计时之外。Registry 发布不含此前校验/归档步骤；完整准备阶段保留在原始证据中。

| 缓存准备操作 | Shell | NumPy |
|---|---:|---:|
| 复制 cache store | 5.503 | 3.562 |
| 原始 OCI blobs → registry | 0.501 | 0.301 |
| cache-service 缓存 Prepare | 2.385 | 0.002 |

这些是本地缓存准备成本。当前制品的首次上游下载、解包、索引成本未测，历史首次拉取使用不同制品，不代入这里。

<details markdown="1">
<summary>与上游性能表的比较范围</summary>

### 与上游性能表的关系 {#upstream-comparison}

[TurboOCI 的官方性能表][ref-turbooci-performance]测量从拉取镜像到服务可用的时间，惰性路径使用 `rpull`。这里的 stage 负载是四文件只读校验，另有独立的 shell、NumPy 应用启动实验。上游服务、镜像和环境与这些条件不同，其秒数不与本机毫秒结果相除，也不用于计算加速倍数；表中数字均来自各自列明的本项目实测批次。

</details>

### 来源与复现 {#run}

<details markdown="1">
<summary>查看统计下载、来源与复现方法</summary>

<!-- measurement-status:start -->

相同 stage 执行器的文件加载对照已完成新框架测量；VM / Docker 的 shell、NumPy 数据保留独立历史批次。见[复测覆盖](index.md#measurement-coverage)。

<!-- measurement-status:end -->

[Stage 统计 CSV](../../../overrides/assets/benchmarks/data/lazy/lazy-stage-summary.csv) · [Stage 配对差异 CSV](../../../overrides/assets/benchmarks/data/lazy/lazy-stage-comparisons.csv) · [Stage 来源摘要](../assets/benchmarks/lazy-stage.provenance.json) · [Stage 复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark.new/startup/README.md#stage-lazy)

Stage 原始证据保留在 `benchmark.new/startup/.data/overlaybd-lab-20261017/stage-lazy-20261017-a/`；独立审计复算原始时钟、文件校验、挂载与设备身份、Run Bundle、逐请求字节、33 轮后台观察及清理，并核对 semspec 导出。二进制、runtime 库与冻结 harness 有摘要收据，精确源码构建对应关系未验证。公开下载仅包含加工统计与精简来源，原始样本和完整清单留在本地证据目录。

[复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#current-implementation-dockerlazy-comparison)

完整 CSV/JSON 与原始证据保留在 `benchmark/pvisor/.data/lazy-shell-current-formal-20261010/` 和 `lazy-numpy-current-formal-20261010-2/`，各批 `derive.py` 重查全部 136 次启动、冻结输入、保留 OCI 摘要和清理收据后生成 CSV。构建证据为 `.data/lazy-current-build-20261010/`：1,193 个冻结源码输入及实测二进制摘要与独立新构建一致，核验记录的来源关系但不证明 hermetic 可复现。失败预检/批次及 2026-10-07 历史数据独立保留。这些观测也不与[预准备环境启动](startup.md)合并。

</details>

### 参考文献 {#references}

1. containerd. [OverlayBD README（v1.0.18）][ref-overlaybd]。块级分层镜像、TCMU 后端及与 Accelerated Container Image 的关系。
2. containerd. [Overlaybd — TurboOCIv1][ref-turbooci]，含 [Performance][ref-turbooci-performance]。原始 OCI 内容、元数据镜像和上游性能表的计时定义；引用仓库 `main` 文档，访问于 2026-10-17。
3. Huiba Li 等. [DADI: Block-Level Image Service for Agile and Elastic Application Deployment][ref-dadi]. USENIX ATC 2020, pp. 727–740。块级按需镜像服务的研究背景。

[ref-overlaybd]: https://github.com/containerd/overlaybd/blob/v1.0.18/README.md
[ref-turbooci]: https://github.com/containerd/accelerated-container-image/blob/main/docs/TURBO_OCI.md
[ref-turbooci-performance]: https://github.com/containerd/accelerated-container-image/blob/main/docs/TURBO_OCI.md#performance
[ref-dadi]: https://www.usenix.org/conference/atc20/presentation/li-huiba
