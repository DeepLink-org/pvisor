# 镜像按需读取能为 shell 和 NumPy 任务缩短多少启动等待？

## 主要结论 {#conclusions}

**在 Linux/KVM、镜像服务已准备、宿主页缓存热的本机协议模拟中，pVisor lazy VM 的冷客户端正确输出 P50 为 Ubuntu shell 186.1 ms、NumPy 小脚本 778.0 ms；两种负载的同批 Docker 对照均支持更短的冷客户端中位等待，热客户端则是 Docker 更快。**

| 需求 | 选型含义 |
|---|---|
| 未缓存镜像，启动短 shell 或 NumPy 任务 | 这些条件下，按需读取减少内容下载和冷客户端中位等待 |
| 已有完整本地镜像 | Docker 更早输出正确标记；lazy VM 仍需 VM/Job 启动和收尾 |
| 首次部署镜像服务 | 另计上游下载、解包和索引成本；缓存准备不衡量这些工作 |

## Motivation {#motivation}

一次性环境运行短命令时，完整镜像下载、解包可能比任务本身更贵。选择按需读取前，需要区分服务端准备和客户端启动成本，也要判断镜像已缓存后是否仍有优势。

## 实验设计 {#interpretation}

`B-LAZY-STARTUP`，Linux x86_64/KVM、Fedora 内核 7.2.8-200.fc44.x86_64、Docker 29.7.2、overlayfs/containerd image store。每种负载两条路径使用同一个固定 amd64 manifest：Ubuntu 26.04 shell，以及 `amancevice/pandas` 中的 Python 3.13.14 / NumPy 2.5.2（不导入 pandas）。Docker 从新建 Distribution registry 拉取完整压缩 blobs；pVisor 通过真实 `pvisor-cache serve` 按需读取，启用客户端二进制索引页、上游连接池和持久私有桥。静态 release 制品与内嵌 firmware 对应保留的独立新构建证据。

Shell 先校验 `/etc/os-release` 再输出唯一标记。NumPy 使用 `-B -u`、`PYTHONHASHSEED=0`、数值库各单线程，检查版本并校验 int64 4×4 数组的平方和 1240、矩阵乘积和 3680，再输出标记；所有启动必须退出 0。**Ready** 到观察到正确输出，**Completion** 到进程退出与输出流关闭，包含收尾。

每种负载独立成批，30 轮随机交替 Docker/lazy，每路径冷后紧接热，每次新容器/VM/workspace/stage；初始轮及三轮预热不计入统计。Docker 冷启动必须获取全部 blobs；lazy 冷启动使用新客户端缓存，热启动复用对应缓存且不得传输文件内容。任一正确性、计数或清理失败使批次无效，慢有效样本全部保留。采样期间无并发构建或测试。

| 控制项 | 实际配置与范围 |
|---|---|
| 负载预算 | CPU 0/1；Docker 两核 quota、2 GiB 硬限制且无额外 swap；VM 2 vCPU、2 GiB guest RAM，与整个宿主进程树上限不同 |
| 服务 | registry/cache 固定 CPU 2/3；daemon/containerd、代理未作相同整机预算约束 |
| 网络与缓存 | 本机 loopback，HTTPS registry 与认证、未加密 TCP cache；无 RTT/带宽注入；服务与宿主页缓存热 |
| 隔离与校验 | 私有 user/mount/PID namespace、pivot_root 保留已有 listener；校验输出、Run Bundle、冻结摘要、冷/热计数与 namespace 清理 |

Registry 准备使用校验后的保留**原始压缩 OCI blobs**，cache 服务复制支持的 store 后执行缓存 Prepare；没有测量上游拉取、解包、建索引时间。宿主目录为 read/write bind，上述检查不证明完整宿主完整性。真实 WAN、并发、完整发行版引导、大型计算、相同整机预算以及纯 VMM/lazy 算法贡献未测。不同负载和历史批次不合并，也不据其差值计算实现加速比。

## 实验数据和分析 {#results}

### Ubuntu shell 客户端启动等待 {#startup}

测量日期 2026-10-10，每格 n=30、共 120 正式样本，零正式失败、零按速度剔除。单位 ms；非分离分布报告 P50，分离分布报告各簇中位数与样本比例。P95 仅参考，分簇原因未确定。

| 路径 / 缓存 | Ready：P50 或簇中位数（占比） | Ready P95 | Completion：P50 或簇中位数（占比） | Completion P95 |
|---|---:|---:|---:|---:|
| Docker 冷 | 597.2 (23/30); 1,563.5 (7/30) | 1,923.6 | 690.2 (25/30); 1,744.7 (5/30) | 2,032.0 |
| pVisor lazy 冷 | 186.1 | 256.5 | 312.5 | 381.2 |
| Docker 热 | 85.0 (24/30); 212.6 (6/30) | 225.5 | 113.6 (22/30); 280.3 (8/30) | 317.2 |
| pVisor lazy 热 | 151.0 | 184.2 | 267.3 | 310.0 |

按完整轮配对 bootstrap 5,000 次，**Docker 减 lazy 的边际 Ready 中位数差**为：冷：**575.0 ms**，95% CI **[384.5, 841.3] ms**；热：**-60.9 ms**，95% CI **[-68.3, -30.4] ms**。区间描述总体中位数，不代表单个簇的中心或每次启动。

### 小型 Python/NumPy 脚本 {#numpy}

独立批次同样测于 2026-10-10，每格 n=30、共 120 正式样本，无失败或按速度剔除；单位 ms，沿用相同分布规则。

| 路径 / 缓存 | Ready：P50 或簇中位数（占比） | Ready P95 | Completion：P50 或簇中位数（占比） | Completion P95 |
|---|---:|---:|---:|---:|
| Docker 冷 | 1,001.5 (19/30); 2,233.3 (11/30) | 2,836.5 | 1,041.7 (19/30); 2,305.9 (11/30) | 2,877.1 |
| pVisor lazy 冷 | 778.0 | 1,086.1 | 925.0 | 1,307.3 |
| Docker 热 | 120.7 (24/30); 196.6 (6/30) | 211.5 | 161.9 (24/30); 271.1 (6/30) | 276.7 |
| pVisor lazy 热 | 462.2 | 521.3 | 606.8 | 677.0 |

Docker 减 lazy 的边际 Ready 中位数差为：冷：**396.6 ms**，95% CI **[231.7, 1,055.3] ms**；热：**-339.2 ms**，95% CI **[-345.9, -332.4] ms**。这些配置下，冷客户端中位等待支持 lazy，热客户端支持 Docker。lazy 冷 Ready 范围为 682.8–2,397.3 ms，中位数更小不承诺每次启动都更快；Completion 还应计入 Job 收尾成本。

### 客户端载荷 {#transfer}

单位 bytes，计数截至 Completion，每格 n=30，条件内所有样本计数相同。

| 负载 / 路径 / 缓存 | 文件内容或 OCI blobs | 二进制 Metadata Data | 总响应 |
|---|---:|---:|---:|
| Shell / docker / 冷 | 41,842,292 | 0 | 41,843,684 |
| Shell / docker / 热 | 0 | 0 | 0 |
| Shell / lazy / 冷 | 2,580,417 | 1,117,945 | 3,702,376 |
| Shell / lazy / 热 | 0 | 0 | 641 |
| NumPy / docker / 冷 | 115,930,809 | 0 | 115,932,435 |
| NumPy / docker / 热 | 0 | 0 | 0 |
| NumPy / lazy / 冷 | 36,957,665 | 3,085,982 | 40,072,915 |
| NumPy / lazy / 热 | 0 | 0 | 816 |

Docker 内容是压缩层及 config，lazy 内容仅为解压后 Read Data。二进制索引页单列为 Metadata Data，并计入总响应，因此文件内容减少不能直接解释为完整网络流量减少。Docker 响应不含 HTTP headers/TLS，两路径均不含 TCP/IP 开销。热 lazy 仍有 Prepare/Ping。

### 服务端缓存准备 {#preparation}

单位 s，每项 n=1，均在客户端计时之外。Registry 发布不含此前校验/归档步骤；完整准备阶段保留在原始证据中。

| 缓存准备操作 | Shell | NumPy |
|---|---:|---:|
| 复制 cache store | 5.503 | 3.562 |
| 原始 OCI blobs → registry | 0.501 | 0.301 |
| cache-service 缓存 Prepare | 2.385 | 0.002 |

这些是本地缓存准备成本。当前制品的首次上游下载、解包、索引成本未测，历史首次拉取使用不同制品，不代入这里。

### 数据下载与来源 {#run}

[Shell 统计 CSV](lazy-startup-summary.csv) · [Shell 差异与准备 CSV](lazy-startup-details.csv) · [Shell 来源 CSV](lazy-startup-provenance.csv)

[NumPy 统计 CSV](lazy-numpy-summary.csv) · [NumPy 差异与准备 CSV](lazy-numpy-details.csv) · [NumPy 来源 CSV](lazy-numpy-provenance.csv) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#current-implementation-dockerlazy-comparison)

原始证据保留在 `benchmark/pvisor/.data/lazy-shell-current-formal-20261010/` 和 `lazy-numpy-current-formal-20261010-2/`，各批 `derive.py` 重查全部 136 次启动、冻结输入、保留 OCI 摘要和清理收据后生成 CSV。构建证据为 `.data/lazy-current-build-20261010/`：1,193 个冻结源码输入及实测二进制摘要与独立新构建一致，核验记录的来源关系但不证明 hermetic 可复现。失败预检/批次及 2026-10-07 历史数据独立保留。这些观测也不与[预准备环境启动](startup.md)合并。
