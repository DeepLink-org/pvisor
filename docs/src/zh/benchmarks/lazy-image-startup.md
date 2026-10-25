# 惰性镜像能节省多少磁盘、传输和等待？

## Motivation {#motivation}

压缩镜像适合磁盘敏感、首次只访问少量文件的短任务：减少常驻解包副本，并按工作集交付远程内容。在所测短任务和库树扫描中，已有完整缓存或需要遍历大量文件时，原生 Docker 更快；独立 guest kernel 也需要额外实例内存。应按磁盘预算和实际访问范围选择，不能把压缩率当成全面性能优势。

四种真实镜像的单独库存节省 71%–73%，四镜像共享库存节省 74%。冷 shell 总响应减少 89.9%，Python 小任务减少 57.1%；完整标准库 mmap 扫描只减少 9.7%，并且等待更长。四实例扣除空基线后的每实例内存为 Docker 8.31 MiB、pVisor 48.95 MiB。

## 实验设计 {#interpretation}

测量完整镜像的分配量、客户端启动到正确输出的 Ready、响应载荷和实例内存增量，作为镜像供给与使用成本。这些成本代表磁盘容量、短任务等待和部署预算。数值越低，同一工作负载需要的存储、传输或资源越少。

2026-10-25，Linux x86_64/KVM、Ryzen 7 9700X，当前冻结 release；原生 Docker 29.7.2/rootless containerd overlayfs，pVisor 使用默认 V2。双方固定同一 linux/amd64 manifest，校验 OCI 内容、发行版／Python 版本、私有文件与正常退出。以下四类独立批次不合并样本：本地库存各 N=3；预准备 shell Ready N=30／内存 N=5；远程三种负载冷／热各 N=30；本地 Python 及两遍 mmap 各 N=30。延迟各有三轮预热、随机交替，保留所有正确慢样本；不报告 P99。

本地库存从空私有 store 开始，包含元数据、正常内容共享与 Docker 压缩 blobs＋解包 snapshots；扣除 Docker 空库存，不含导入源、工具和运行写层。双方目录关闭 Btrfs 透明压缩，硬链接只计一次；分配块不等于独占物理设备字节。预准备实例不含 pull。内存统计整个受限产品组，包含 daemon／VMM 和已计费缓存；扣同组无实例基线后计算实例增量，不是整机净物理内存。

远程服务预先准备在本机；冷客户端缓存为空，热客户端紧接复用内容，但每次都是新 VM／容器。Docker 计完整 OCI 拉取＋运行，pVisor 计实际按需 spawn；排除服务准备和私有 daemon 启动，保留宿主页缓存。应用固定 CPU 0,1、服务 2,3；配置 2 CPU／512 MiB 容器或 2 vCPU／512 MiB guest。Docker 使用私有 HTTPS registry，cache 使用 token 鉴权 loopback TCP，传输／执行边界不同，计时不解释为纯格式差异或 WAN。

本地运行组限制 4 CPU／4 GiB／零 swap；shell 为 1 CPU／256 MiB，Python／mmap 为 2 CPU／512 MiB。容器 limit 和 guest RAM 范围不同，不代表相同应用可用内存。mmap 校验整个 Python 标准库树的 1262 个普通文件、25,557,453 字节，摘要独立从 OCI 层派生；本地同实例读两遍，每遍含遍历、mmap 和 SHA-256。第二遍包含页缓存复用，不是解码吞吐或随机首次缺页测试。

## 实验数据和分析 {#results}

### 本地镜像库存 {#storage}

MiB，三个新库存的中位数；四镜像组合是真正共享的 store。

| 镜像库存 | Docker MiB | pVisor V2 MiB | 节省 |
| --- | --- | --- | --- |
| Alpine 3.20 | 12.53 | 3.59 | 71.3% |
| Ubuntu 24.04 | 110.18 | 31.64 | 71.3% |
| Python 3.12 slim | 176.68 | 47.15 | 73.3% |
| Python 3.13 slim | 172.34 | 46.18 | 73.2% |
| 四镜像共享库存 | 363.16 | 94.29 | 74.0% |


![本地完整库存的磁盘占用](../assets/benchmarks/lazy-v2-capacity.svg)

Docker 共享库存中压缩 blobs 为 90.29 MiB、解包 snapshots 为 272.75 MiB。主要结构差异是 V2 不常驻完整解包树；其编码池不保证比 OCI 压缩 blobs 更小。结果不包含导入峰值、在线 GC 或长期缓存增长。

### 远程冷／热客户端 {#application-comparison}

Ready 单位 ms，N=30；分离组显示 `组中位数 (样本数/30)`，不用单个 P50 概括。按预定排序间隔规则分组，所有点均保留；差值是配对中位差及 5,000 次 bootstrap 95% CI，不是两项中位数相减。完整退出另存原始证据。

| 负载／客户端缓存 | Docker Ready ms | pVisor Ready ms | PV−Docker [95% CI] ms |
| --- | --- | --- | --- |
| Shell / cold | 392.99 | 198.25 | -194.70 [-201.47, -190.29] |
| Shell / warm | 68.49 | 162.25 | +93.16 [+91.20, +95.30] |
| Python / cold | 686.46 | 505.75 | -180.81 [-189.82, -173.98] |
| Python / warm | 132.92 | 341.08 | +207.51 [+205.60, +211.45] |
| 标准库 mmap / cold | 679.78 | 1601.68 | +919.44 [+907.06, +936.33] |
| 标准库 mmap / warm | 103.80 | 1067.12 | +963.57 [+957.46, +966.41] |


![冷暖客户端的全部启动样本与响应载荷](../assets/benchmarks/lazy-v2-remote.svg)

冷 shell 和 Python 小任务受益于少取内容；热客户端原生 Docker 更快。完整库树扫描的等待高于完整 OCI 拉取后运行：按需路径的元数据和文件访问成本可能抵消少下载的收益。这里的 Python 是标准库校验，不是历史 NumPy 负载。

冷客户端响应载荷，MiB、中位数、N=30；metadata 计入总响应。Docker 文件列含压缩 OCI blobs，pVisor 文件列为原始 Read 数据，编码方式不同；总量是应用响应，不含 HTTP headers、TLS 或 TCP/IP，也没有未经测量的 Ready 流量截点。

| 负载 | Docker 总响应 MiB | PV 文件 MiB | PV 索引 MiB | PV 总响应 MiB | 减少 |
| --- | --- | --- | --- | --- | --- |
| shell | 28.39 | 2.80 | 0.06 | 2.86 | 89.9% |
| python | 43.18 | 18.43 | 0.08 | 18.53 | 57.1% |
| mmap-scan | 43.18 | 38.70 | 0.15 | 39.01 | 9.7% |


全部热样本均为零内容取回；pVisor 仍有少量 Prepare/Ping。响应节省不能直接当成公网账单或传输时间节省。

### 完整准备后的读取与内存 {#local-access}

ms、N=30；Ready 是宿主观察，mmap 两遍是实例内阶段时钟，二者不能相加。首次扫描保留宿主页缓存，第二遍复用同一实例页缓存。

| 本地指标 | Docker ms | pVisor ms | PV−Docker [95% CI] ms |
| --- | --- | --- | --- |
| Shell Ready | 79.53 | 106.67 | +26.80 [+25.84, +28.20] |
| Python Ready | 132.70 | 343.88 | +211.27 [+203.16, +216.87] |
| mmap 首遍 | 25.47 | 619.98 | +594.53 [+593.84, +595.97] |
| mmap 同实例重复 | 22.31 | 461.79 | +439.48 [+438.70, +439.95] |


![本地创建与首次、重复 mmap 扫描的全部样本](../assets/benchmarks/lazy-v2-runtime.svg)

远程读取不是全部代价：本地池也经过 VM 文件服务、路径解析与 guest page cache。该对照包含运行时与文件系统差异，不能把差值全部归为 Zstd 解压；也不能由热重复扫描推导随机 mmap 或写入性能。

整组内存，MiB、五个新产品组中位数；只测空闲 shell/sleep。

| 空闲部署内存 | Docker MiB | pVisor MiB | PV−Docker [95% CI] MiB |
| --- | --- | --- | --- |
| 镜像已准备，无实例 | 212.21 | 35.11 | -177.50 [-178.00, -170.77] |
| 完整组，4 实例 | 244.05 | 230.66 | -13.39 [-14.95, -8.84] |
| 扣基线后每实例，4 实例 | 8.31 | 48.95 | +40.64 [+40.06, +41.48] |


pVisor 的固定管理／缓存计费较小，原生 Docker 的实例增量更低。镜像页缓存通常可回收，guest 匿名 RAM 与它不同；共享既有 Docker daemon 时不能给每个新容器摊入整个固定成本。四实例不建立最大密度或盈亏平衡规模。

### 服务准备与证据 {#preparation}

| 服务准备（单次） | s |
| --- | --- |
| ubuntu-24.04 | 1.81 |
| python-3.12-slim-bookworm | 2.49 |


以上是从已校验压缩 OCI blobs 开始、仍需上游 manifest 解析的单次发布，不是首次公网拉取水位。发布池 78.42 MiB，保留的 OCI staging 284.10 MiB，二者都在服务端；客户端节省并不消除它们。没有与之同条件的准备速度排名。

[设计与缓存边界](../design/lazy-image-v2.md#cache-costs) · [来源摘要](../assets/benchmarks/lazy-image-v2.provenance.json) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/storage/README.md)。完整时钟、逐请求计数、OCI 摘要、失败预检及被门禁拒绝的批次、源码／二进制与独立复算保留在本地实验目录。旧存储与 OverlayBD／NumPy 数据保留原口径，不作为当前 V2 的性能数字。真实 S3／WAN、并发、随机 mmap、copy-up、导入峰值和长期回收尚未验证。
