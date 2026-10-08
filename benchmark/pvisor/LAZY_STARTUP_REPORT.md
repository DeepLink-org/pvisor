# Ubuntu 镜像按需读取能缩短多少启动等待？

## 主要结论

**在镜像服务已准备、宿主页缓存热、本机模拟远程协议的条件下，pVisor lazy VM 的冷客户端首条正确输出中位数为 183.7 ms，全部 30 次均早于 Docker 冷客户端；热客户端则通常是 Docker 更快。** 这是两条实际产品启动路径的条件性对照，不是纯 lazy 算法、纯 VMM、真实 WAN 或相同整机资源预算的比较。

| 客户端状态 | Docker 首条正确输出 | pVisor lazy VM 首条正确输出 | 选型含义 |
|---|---|---|---|
| 冷 | 582.9 ms（23/30）；1,524.4 ms（7/30） | P50 183.7 ms（30/30） | 镜像未缓存的短 shell 任务，按需读取减少客户端等待 |
| 热 | 82.6 ms（25/30）；202.3 ms（5/30） | P50 162.7 ms（30/30） | 已有完整本地镜像时，Docker 通常更快 |

Docker 的分离分布按仓库规则列出簇中位数和占比，不用一个 P50 掩盖分布。簇是描述性的，不代表已确定慢路径原因。

## Motivation

一次性环境运行短命令时，完整镜像下载、解包可能比任务本身更贵。用户需要区分服务端已经准备镜像后的客户端启动收益，以及首次建立镜像服务的成本；也需要知道本地镜像已缓存时 lazy VM 是否仍有优势。

## 实验设计

- Benchmark：`B-LAZY-STARTUP`。Linux x86_64 / KVM，Fedora 内核 `7.2.8-200.fc44.x86_64`；Docker 29.7.2，overlayfs/containerd image store。
- `ubuntu:latest` 固定到 Ubuntu 26.04 的 amd64 manifest：`sha256:88a381d5b5eeb2b35d3ad70925a362c37ce569daf43ede89ff818ec20e4d3794`。上游多平台 index 是 `sha256:f144425ff09be612d6d9ad965196e9cdc23dae1f42110a8a11a3e9a8198759f7`。两路径选中的 manifest 完全相同。
- Docker 从开源 Distribution `registry:3` 的本机镜像副本完整拉取；计数代理通过 HTTPS 提供副本。pVisor 从真实 `pvisor-cache serve` 的认证、未加密 TCP 服务按需读取文件；不是 OpenSandbox 的 `pvisor-daemon serve`。
- 服务端镜像预先准备。OCI 客户端要求可信 HTTPS，因此 cache 服务从 Docker Hub 导入同 digest，而不是从本机不安全 registry 导入。该准备不计入客户端时间，也不把两种服务准备视作受控 A/B。
- 两路径执行同一个 `/bin/sh` 工作负载：读取 `/etc/os-release`，断言 Ubuntu 26.04，输出一次 `LAZY_READY ubuntu 26.04`，成功退出。ready 为启动 CLI 到观察到完整标记，completion 为启动 CLI 到退出与输出流关闭，包含各自客户端开销和收尾。
- 每组 30 个正式样本，seed `20261007`，同轮随机交替运行 Docker/lazy，每路径冷后紧接热。另有一次预检及三轮 warmup，不入正式统计。共 120 个正式有效样本、零失败，不剔除慢有效样本；失败预检独立保留。
- Docker 冷样本前只删除 benchmark 镜像，并校验每次完整获取所有 blob；热样本不访问 registry。lazy 每对使用新的 `XDG_CACHE_HOME`，热启动复用它，每次新 VM/工作区/stage。host page cache 和服务端热；这不是全机冷磁盘，Docker snapshotter 完全冷状态未独立证明。
- Docker 配置两核、CPU 0/1、2 GiB 硬限制、零额外 swap；pVisor launch affinity 0/1、2 vCPU、2 GiB guest RAM。registry/cache 在 CPU 2/3。**Docker daemon/containerd 未限制到两核，代理未固定 CPU，VM guest RAM 也不是宿主进程树硬上限；不声称整机预算等价。** 没有额外独立资源执法或宿主完整性探针。
- 所有有效样本校验输出与成功退出，lazy 还校验 Run Bundle 声明 VM/暂存执行。字节统计截至 completion，不是 ready 时刻，且不是完整网络 wire bytes。
- 服务地址仅绑定 loopback；未注入 RTT、带宽限制，没有测真实远程主机、并发、大型工作负载、完整 systemd 引导或 OpenSandbox bootstrap。

## 实验数据和分析

### 启动耗时

单位 ms，每格 n=30。Docker 分离分布列两个簇；其他列 P50。P95 仅供参考，不代表稳定尾延迟。

| 路径/缓存 | Ready：簇中位数或 P50 | Ready P95 参考 | Completion：簇中位数或 P50 | Completion P95 参考 |
|---|---:|---:|---:|---:|
| Docker 冷 | 582.9（23/30），1,524.4（7/30） | 1,855.1 | 600.6（19/30），1,465.1（11/30） | 1,944.8 |
| lazy 冷 | 183.7 | 229.2 | 331.0 | 381.2 |
| Docker 热 | 82.6（25/30），202.3（5/30） | 210.3 | 119.6 | 311.0 |
| lazy 热 | 162.7 | 182.1 | 295.5 | 330.6 |

冷 lazy ready 全部落在 **170.1–302.1 ms**，冷 Docker ready 在 **528.2–2,052.4 ms**；本批冷路径的方向不依赖单个总体中位数。warm Docker 多数样本快于 lazy，但存在较慢簇。lazy completion 比 ready 高约百毫秒，表示 CLI/Job 收尾不能忽略。

补充不确定性：以完整轮为单位配对 bootstrap 5,000 次，统计量为 Docker 减 lazy 的**边际总体中位数差**，冷缓存 95% CI 为 **389.2–833.9 ms**，热缓存为 **−86.1 至 −48.2 ms**。该统计量不能代表两个 Docker 簇的中心，不据此给出单一倍数/百分比排名，也不作纯 lazy 的因果解释。分簇规则复用 `publication.py`，不是事后剔除样本。

### 客户端内容传输

单位 bytes，每格 n=30；每个条件内所有样本字节数完全相同。

| 路径/缓存 | 文件内容或 OCI blob 响应 | 含相关元数据的响应计数 |
|---|---:|---:|
| Docker 冷 | 41,842,292 | 41,843,684 |
| lazy 冷 | 2,580,417 | 2,588,104 |
| Docker 热 | 0 | 0 |
| lazy 热 | 0 | 575 |

Docker 内容列为压缩 OCI 层及 config；lazy 为读取的解压后文件内容。冷 lazy 只读 shell、loader、libc、ld cache、os-release，**应用内容载荷比完整 OCI blobs 少 93.8%**；这不是相同编码的压缩率或完整网络流量减少率。Docker 响应列不含 HTTP headers/TLS，lazy 响应列含协议帧及元数据，两者均不含 TCP/IP。热 lazy 仍有 ping/prepare 请求，不是零网络依赖。

### 准备成本及来源

| 准备操作 | 单次观察耗时 | 口径 |
|---|---:|---|
| Docker Hub → 本机 Distribution mirror | 12.483 s | skopeo 拉取并推送所选平台镜像 |
| Docker Hub → cache 服务首次准备 | 15.130 s | 拉取、解包、构建索引并返回 prepared handle |

两项各 n=1，来源路径和工作不同，不做速度排名或摊销临界次数计算。**183.7 ms 不包含 15.130 s 的首次 cache 准备成本。**

使用预存 release 静态 musl 制品，内嵌 firmware；不是重新构建后的当前源码性能证明。当前 HEAD 和 dirty source manifest 单独留档，构建时源码关系未知，不把结果归因于 checkout 内尚未提交的 OverlayFS 改动。二进制在采样后摘要复核一致并保留副本。

- pVisor SHA-256：`d3f09687cca60b617629c215fa1359c441ab6ac19f8c63b68f1dcdf7614e6b30`
- cache SHA-256：`1d29e6b85cf4789557253851a84a3086cb3394eec5995d47bd630ae36b6f9e22`
- 测量 harness SHA-256：`6f19828e5057d3e15afaf8c13081d3213b8ed764096794ef9c0abfef7914694b`
- 原始 report SHA-256：`629a518417a82d90ab26894edcf214f27430bcde89719382c6f231ba48509fa6`

[整理后的 CSV](lazy-startup-summary.csv)；[复现说明](README.md#lazy-image-client-startup)。本地原始证据位于 `benchmark/pvisor/.data/lazy-startup-local-20261007/`：report、单次 stdout/stderr、请求字节计数、Run Bundles、原始 harness、源码摘要、制品副本。原始 report 的初始单中位数 summary 不用于正文；保留原字节，分布感知统计另存 `analysis.json`。用户文档已整理为[中文专题](../../docs/src/zh/benchmarks/lazy-image-startup.md)和[英文专题](../../docs/src/en/benchmarks/lazy-image-startup.md)，站点只提供加工统计与来源 CSV，原始证据仍留在本地。
