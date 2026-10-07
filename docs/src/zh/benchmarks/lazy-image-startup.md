# 镜像按需读取能为 shell 和 NumPy 任务缩短多少启动等待？

## 主要结论 {#conclusions}

**在 Linux/KVM、镜像服务已准备、宿主页缓存热的本机协议模拟中，pVisor lazy VM 上 Ubuntu shell 的冷客户端首条正确输出 P50 为 183.7 ms，全部 30 次均早于 Docker 冷客户端；小型 Python/NumPy 脚本虽减少内容传输，但未检出冷客户端 Ready 中位数差异；热客户端 Docker 更快。**

| 需求 | 选型含义 |
|---|---|
| 未缓存镜像，启动短 shell 任务 | 按需读取减少客户端内容下载和启动等待 |
| 未缓存镜像，启动小型 Python/NumPy 脚本 | 内容传输更少，未证明冷启动收益 |
| 已有完整本地镜像 | Docker 通常更早输出 shell 标记，NumPy 则是 Docker 更快 |
| 首次部署镜像服务 | 另计拉取、解包和索引成本；客户端时间不包含准备 |

## Motivation {#motivation}

一次性环境运行短命令时，完整镜像下载、解包可能比任务本身更贵。选择按需读取前，需要区分服务端准备和客户端启动成本，也要判断镜像已缓存后是否仍有优势。

## 实验设计 {#interpretation}

Linux x86_64 / KVM，Fedora 内核 7.2.8-200.fc44.x86_64，Docker 29.7.2，overlayfs/containerd image store。`ubuntu:latest` 固定为 Ubuntu 26.04 的同一个 amd64 manifest。Docker 从开源 Distribution `registry:3` 副本完整拉取；pVisor 通过真实 `pvisor-cache serve` 按需读取。该服务与 OpenSandbox 的 `pvisor-daemon serve` 不同。

Ubuntu shell 批次的两条路径运行相同的 `/bin/sh`：读取 `/etc/os-release`，校验 Ubuntu 26.04，输出一次正确标记并成功退出。**Ready** 从启动 CLI 到观察到完整标记；**Completion** 到进程退出与输出流关闭，包含收尾。负载代表短命令环境初始化，完整发行版引导、Agent CLI 和大型工具任务未测。

独立的 Python/NumPy 批次使用公开镜像 `amancevice/pandas:slim-3.0.5`，固定为 amd64 digest `sha256:9a3a94039175ac799ad33c1a207997994ff9b24814e06508dddfa259b1ed9159`。两条路径运行同一小型脚本，使用 Python 3.13.14、NumPy 2.5.2；镜像包含 pandas，但不导入。脚本使用 `-B -u`，设置 `OMP_NUM_THREADS=1`、`MKL_NUM_THREADS=1`、`OPENBLAS_NUM_THREADS=1` 和 `PYTHONHASHSEED=0`，校验两个版本，创建 `x = np.arange(16, dtype=np.int64).reshape(4, 4)`，断言 `np.square(x).sum() == 1240` 与 `(x @ x.T).sum() == 3680`，随后输出一次唯一标记并以退出码 0 结束。Ready 因而包含导入和数值正确性校验，代表小型数值脚本环境，不代表大型计算。下述 CPU/内存预算、loopback 协议和缓存/采样控制同样适用于 NumPy。NumPy 正式采样期间无并发测试、构建或内核 benchmark。两种负载批次分别统计，跨批次比较不证明负载或实现变化的因果影响。

每个批次的每路径/缓存状态 30 个正式样本，同轮随机交替 Docker/lazy，每路径冷后紧接热；一次初始预检和三轮预热不计入统计。每次新容器或 VM、工作区和 stage。Docker 冷启动前只删除测试镜像，并要求完整获取全部 blob；lazy 每对使用新客户端缓存，热启动复用它。服务端和宿主页缓存保持热；全机冷磁盘及 Docker snapshotter 完全冷状态未独立证明。

| 控制项 | 实际配置与范围 |
|---|---|
| 负载 CPU / 内存 | Docker：CPU 0/1、两核 quota、2 GiB 硬限制、零额外 swap；pVisor：launch affinity 0/1、2 vCPU、2 GiB guest RAM |
| 服务与客户端 | registry/cache 固定 CPU 2/3；Docker daemon/containerd 和计数代理未作完整两核约束；guest RAM 与宿主进程树硬上限不等价 |
| 网络 | 本机 loopback；Docker 经计数代理使用 HTTPS，cache 使用认证、未加密 TCP；无 RTT 或带宽注入 |
| 校验与拒绝 | 输出、成功退出、完整冷下载与热缓存命中；lazy 另校验 Run Bundle 的 VM/暂存声明。失败或超时使实验失败，慢有效样本不剔除 |

计入样本不代表独立资源执法或宿主完整性验证。可比较的是这些实际启动配置；真实 WAN、并发、相同整机预算、纯 lazy 算法或纯 VMM 贡献未测。服务端准备独立记录：cache 从 Docker Hub 导入同 digest，可信 HTTPS 的 OCI 客户端未从本机不安全 registry 导入。

## 实验数据和分析 {#results}

### Ubuntu shell 客户端启动等待 {#startup}

测量日期 2026-10-07，每格 n=30，共 120 个有效样本、零正式失败、零按速度剔除。单位 ms；非分离分布报告 P50，分离分布报告各簇中位数与样本比例。P95 仅供参考。

| 路径 / 缓存 | Ready：P50 或簇中位数（占比） | Ready P95 | Completion：P50 或簇中位数（占比） | Completion P95 |
|---|---:|---:|---:|---:|
| Docker 冷 | 582.9（23/30）；1,524.4（7/30） | 1,855.1 | 600.6（19/30）；1,465.1（11/30） | 1,944.8 |
| pVisor lazy 冷 | 183.7 | 229.2 | 331.0 | 381.2 |
| Docker 热 | 82.6（25/30）；202.3（5/30） | 210.3 | 119.6 | 311.0 |
| pVisor lazy 热 | 162.7 | 182.1 | 295.5 | 330.6 |

| 冷客户端 Ready 范围，n=30 | 最小 ms | 最大 ms |
|---|---:|---:|
| Docker | 528.2 | 2,052.4 |
| pVisor lazy | 170.1 | 302.1 |

冷 lazy 的全部观测早于冷 Docker，方向不依赖单个总体中位数。热 Docker 的多数样本等待更短，但存在较慢簇。Completion 显示 VM/Job 收尾成本也需要纳入短任务预算。分簇使用既有描述性规则，簇的原因尚未确定。

按完整轮配对 bootstrap 5,000 次，Docker 减 lazy 的**边际总体中位数差**的 95% 区间为：冷缓存 **389.2–833.9 ms**，热缓存 **−86.1 至 −48.2 ms**。区间不代表 Docker 各簇的中心，不据此给出统一倍数或纯 lazy 的因果收益。

### 客户端内容传输 {#transfer}

单位 bytes，每格 n=30，条件内所有样本计数相同；统计截至 Completion。

| 路径 / 缓存 | OCI blob 或文件内容 | 相关响应计数 |
|---|---:|---:|
| Docker 冷 | 41,842,292 | 41,843,684 |
| pVisor lazy 冷 | 2,580,417 | 2,588,104 |
| Docker 热 | 0 | 0 |
| pVisor lazy 热 | 0 | 575 |

Docker 内容列为压缩 OCI 层及 config，lazy 为解压后文件读取载荷。短 shell 仅需 shell、动态链接器、libc 等文件，lazy 内容载荷比完整 OCI blobs 少 **93.8%**。这描述两种交付方式的应用载荷；完整网络流量减少率未测。Docker 响应计数不含 HTTP headers/TLS，lazy 含协议帧和元数据，两者均不含 TCP/IP。热 lazy 仍需 ping/prepare 请求。

### 镜像服务准备成本 {#preparation}

单位 s，每项 n=1，单次观察值。

| 准备操作 | 耗时 | 包含的工作 |
|---|---:|---|
| Docker Hub → Distribution 副本 | 12.483 | skopeo 拉取并推送所选平台镜像 |
| Docker Hub → cache 首次准备 | 15.130 | 拉取、解包、建立索引并返回读取句柄 |

**183.7 ms 的冷客户端 Ready 不包含 15.130 s 的首次 cache 准备。** 两项准备的路径与工作不同，不做准备速度排名或摊销临界次数估计。

### 小型 Python/NumPy 脚本 {#numpy}

独立批次测量日期 2026-10-07，共 120 个正式样本，每格 n=30，零正式失败、零按速度剔除。单位 ms；按发布规则，Ready 和 Completion 分布均未分离，报告 P50。P95 仅供参考。

| 路径 / 缓存 | Ready P50 | Ready P95 | Completion P50 | Completion P95 |
|---|---:|---:|---:|---:|
| Docker 冷 | 1,077.6 | 2,367.1 | 1,118.1 | 2,417.8 |
| pVisor lazy 冷 | 1,213.0 | 1,312.1 | 1,400.8 | 1,532.4 |
| Docker 热 | 120.8 | 151.5 | 157.3 | 206.9 |
| pVisor lazy 热 | 520.5 | 626.7 | 686.9 | 825.1 |

按完整轮配对 bootstrap 5,000 次，冷客户端 **Docker 减 lazy 的边际 Ready 中位数差**为 **−135.3 ms**，95% CI **[−204.1, 176.7] ms**。区间跨零：**未检出冷 Ready 中位数差异**，不能判定任一路径可靠地更快。热客户端差异为 **−399.7 ms**，95% CI **[−408.0, −389.8] ms**，Docker 更快。

截至 Completion，冷内容量为 Docker **115,930,809 bytes**、lazy **36,951,980 bytes**，减少 **68.1%**；相关响应计数分别为 **115,932,435** 和 **37,153,682 bytes**。热内容量两者均为 **0 bytes**，响应计数 Docker 为 **0**、lazy 为 **750 bytes**。每格 n=30，条件内计数相同；沿用上述内容/响应口径与网络开销排除范围。内容减少未证明该脚本的冷启动等待减少。独立的服务端准备耗时为 Docker Hub → Distribution 副本 **33.944 s**、Docker Hub → cache **46.189 s**，各 n=1，均不计入客户端时间；单次观测不支持准备速度排名。

### 数据下载与来源 {#run}

[启动与载荷统计 CSV](lazy-startup-summary.csv) · [差异与准备成本 CSV](lazy-startup-details.csv) · [制品与实验来源 CSV](lazy-startup-provenance.csv) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#lazy-image-client-startup)

[NumPy 启动与载荷统计 CSV](lazy-numpy-summary.csv) · [NumPy 差异与准备成本 CSV](lazy-numpy-details.csv) · [NumPy 制品与实验来源 CSV](lazy-numpy-provenance.csv)

NumPy 与 Ubuntu shell 使用同一预存 launcher 及内嵌 firmware，cache-service 使用另行构建、兼容 Docker gzip 层的二进制；该对照不衡量实现改动的收益。精确制品摘要分别保留在来源 CSV。

`B-LAZY-STARTUP` 使用固定 manifest、预存 release 静态 musl 制品和内嵌 firmware；构建时源码关系未验证，当前源码摘要不证明制品来自该 checkout。制品摘要在采样后复核一致。原始样本、日志、Run Bundles、制品副本与冻结 harness 保留在本地忽略的 `.data/`，CSV 保留批次、条件、统计口径及原始报告摘要；不与[预准备环境启动](startup.md)的样本合并。
