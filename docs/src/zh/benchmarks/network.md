# 网络策略增加多少等待，与普通 OCI 相比如何？

## 主要结论 {#conclusions}

**在本地 HTTP、八线程并发小请求中，原生、pVisor 宿主代理和 VM 的请求中位耗时分别约 0.69、10.09、2.10 ms；32 MiB 的 VM 传输约 193.62 ms。宿主代理的小请求成本值得关注；本地网络结果不能换算为公网模型响应时间。**

| 需求 | 选型含义 |
|---|---|
| 高频本地小请求 | 宿主代理的附加等待明显，先核对业务请求频率 |
| VM 内下载依赖 | 把传输与环境启动分别预算 |
| 阻止直接 TCP 连接 | host/VM deny-all 的正负对照已测；延迟不是隔离证明 |

## Motivation {#motivation}

Agent 会请求模型、下载依赖和读取流式响应，三种负载对代理成本的敏感度不同。请求延迟、首字节、完整传输和启动开销需要分开，才能判断等待来自哪里。

## 实验设计 {#interpretation}

Linux/x86_64，同一宿主上的本地 HTTP 服务，没有公网、TLS 或 DNS。比较原生、pVisor host proxy、VM auto TCP、Podman host network 和 pVisor OCI proxy。所有 payload 使用两个固定 CPU（宿主 0/1、VM 两个 vCPU），但内存没有相同的硬上限，HTTP 服务在 payload 预算之外；这不是资源密度排名。

小请求每批 256 个新 TCP 连接、八线程、每次 1 KiB；先取每批请求中位数，再统计 30 个独立批次，不能把 7,680 个相关请求当作独立样本。bulk 每批传输 32 MiB，服务按 1 KiB 块写出；stream 为十个 13 字节事件，每次间隔 2 ms。首字节是 HTTP 响应体首字节，不是模型 TTFT。传输计时包括建立连接和读取，之后的摘要校验不计入传输，但计入 worker 总时间。

每个条件三次预热、30 个正式批次，17 个条件按轮随机交替。完整内容长度与 SHA-256、CPU 亲和性、保留的命令输出和 Run 边界均须通过；host/VM deny-all 另测直接 socket 拒绝，并核对 Bundle 的强制边界证据。失败不能进入耗时统计；全部慢样本保留，没有事后剔除。P95 仅作参考，不报告 P99。

## 实验数据和分析 {#results}

测量日期 2026-10-06（本地时间）。17 个条件共 510 个正式批次，全部通过，输入未被修改。各性能格 N=30；单位为 ms，P50 为独立批次统计量的中位数。

### 小请求和流式首字节

| 模式 | 每批小请求中位数的 P50 | 相对原生差值，95% 区间 | Stream 首字节 P50 | Stream 完整读取 P50 |
|---|---:|---:|---:|---:|
| 原生 | 0.69 | — | 1.12 | 19.71 |
| pVisor host proxy | 10.09 | +9.40 [9.36, 9.45] | 4.23 | 22.83 |
| pVisor VM auto TCP | 2.10 | +1.40 [1.37, 1.43] | 7.88 | 26.36 |
| Podman host network | 0.71 | +0.01 [−0.001, 0.019] | 3.79 | 22.48 |
| pVisor OCI proxy | 10.23 | +9.54 [9.49, 9.61] | 6.87 | 25.32 |

差值来自同轮配对、5,000 次 bootstrap。Podman 小请求区间包含零，未检出与原生的差异；其他通路增加等待。host/OCI proxy 与 VM auto 使用不同网络路径，不能把它们的差值全部归因于 VM 或容器本身。

### 32 MiB 传输

| 模式 | 传输时间：簇中位数或 P50，ms | 吞吐：对应簇中位数或 P50，MiB/s |
|---|---|---|
| 原生 | 32.61（17/30）；74.90（13/30） | 981.37；427.21 |
| pVisor host proxy | 35.65（20/30）；76.28（10/30） | 897.55；419.50 |
| pVisor VM auto TCP | 193.62（30/30，未分簇） | 165.27 |
| Podman host network | 36.45（17/30）；76.63（13/30） | 877.90；417.58 |
| pVisor OCI proxy | 38.84（20/30）；80.02（10/30） | 823.83；399.94 |

除 VM 外，传输分布有两个明显簇，按各簇数量和中位数展示，不用单个 P50 排名或给出一个“VM / 原生”比例。簇划分是描述规则，尚未验证其原因。服务的 1 KiB 写出模式和 Python HTTP 实现都影响吞吐，因此这些数字不代表 VMM 的网络带宽上限。

### 拒绝验证与适用边界 {#acceptance}

host deny-all 和 VM deny-all 各 30/30 批次阻止对同一本地服务的直接 socket 连接，Bundle 同时确认 `network_non_bypassable`；allow 条件完整响应通过作为正对照。该结果不替代全面网络安全审计。

### 与熟悉方案比较 {#baseline-meaning}

Podman 使用 host network，提供普通 OCI 对照；pVisor OCI 还经过策略代理，所测边界不同。Docker bridge、Firecracker/QEMU 网络、macOS、TLS 和公网 API 未测，不补成数字排名。完整任务的 Docker/Firecracker/QEMU 对照见[端到端任务](agent-tasks.md)。

### 数据下载与复现 {#run}

[网络统计 CSV](network-summary.csv) · [配对比较 CSV](network-comparisons.csv) · [制品与验证摘要](network-provenance.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)

原始报告、响应计时、命令日志、输入与构建清单，以及逐次输出证据审计保存在本地 `.data/`。
