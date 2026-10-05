# 网络策略增加多少等待，与普通 OCI 相比如何？

## 主要结论 {#conclusions}

**本地 1 KiB HTTP 请求 P50 为原生 **0.95 ms**、pVisor 宿主代理 **1.24 ms**、VM **3.83 ms**。32 MiB 传输吞吐分别约 **869、417、155 MiB/s**。代理的小请求增量较小，VM 的批量传输成本更明显；这些结果不代表公网模型响应时间。**

| 需求 | 选型含义 |
|---|---|
| 本地小请求 | 代理增加的等待较小 |
| 批量下载或本机高速传输 | 关注 VM 通路吞吐 |
| 必须阻止 direct socket | 选择可强制执行的边界 |

## Motivation {#motivation}

模型响应可能持续数秒，但工具调用也会频繁下载、小请求和读取流。把请求延迟、传输吞吐与启动成本拆开，才能判断本地代理的影响。

## 实验设计 {#interpretation}

服务在同一宿主 IP 上提供 HTTP，无 Internet/TLS/DNS。每组 3 次预热、30 个批次；小请求每批 256 个 1 KiB 响应、并发 8、每请求新连接，共 7,680 个请求；bulk 每次 32 MiB；stream 每 2 ms 输出一块、共 10 块。核对响应长度与 SHA256，stream 首字节是响应体首字节，不是模型 TTFT。VM 用 auto TCP 路径；host/OCI 用 proxy，Podman 用 host network。

这些结果来自 Linux/x86_64；macOS 的对应负载未测。每项数字的制品、缓存条件与样本保存在关联报告中。

固定制品与测量日期按表注明。失败与校验不通过的样本不计入成功耗时，失败数量单列；既有数据没有事先的宿主干扰剔除规则，所有通过校验的慢样本保留。30 次及更少采样的 P95 仅为观察参考，不给 P99 或稳定尾延迟承诺。

## 实验数据和分析 {#results}

| Network configuration | Backend | Batches | 1 KiB P50/P95 ms | 32 MiB P50 MiB/s | Stream first body P50/P95 ms |
|---|---|---|---|---|---|
| proxy / VM | native | 30 | 0.95/1.42 | 869.4 | 1.10/1.20 |
| proxy / VM | host | 30 | 1.24/2.42 | 416.8 | 1.37/1.63 |
| proxy / VM | vm | 30 | 3.83/8.63 | 154.7 | 4.57/5.19 |
| host-network OCI | native | 30 | 0.99/1.44 | 861.4 | 1.07/1.18 |
| host-network OCI | podman | 30 | 1.01/1.43 | 853.7 | 3.53/3.60 |
| host-network OCI | pVisor OCI | 30 | 1.30/2.30 | 811.0 | 3.78/3.98 |

### 分析与拒绝验证

小请求 host 相对 native P50 增加约 0.29 ms，VM 增加约 2.88 ms。大块传输 P50 为原生约 869 MiB/s、host 417 MiB/s、VM 155 MiB/s；吞吐包括连接和读取，数据摘要校验在传输计时之后。网络 worker 总时长还包含摘要计算。它们是本机 HTTP 通路能力，不预测公网带宽。

host deny-all 的私有网络命名空间和 VM deny-all 各 **30/30** 次阻止 direct socket，Bundle 同时确认 network_non_bypassable。普通 host proxy 的限制是协作式；direct socket 可以绕过，拒绝耗时不能当成吞吐优势。
### 用熟悉的请求和下载理解这些数字 {#baseline-meaning}

原生 HTTP 是没有 pVisor 通路的基线；Podman/crun 是已测的普通 OCI 路径。所测配置 host proxy 比原生每次小请求增加约 0.29 ms，VM 增加约 2.88 ms。如果外部服务本身耗时 100 ms，单看这部分附加成本约为 0.3% 和 2.9%；这是代入假设的解释，不是公网或模型 API 实测。

下载大文件时不能用同样的直觉：32 MiB 按所测配置吞吐折算，原生传输约 37 ms、VM 约 207 ms。大量本机小请求、下载依赖、传模型响应是不同负载。[Docker 完整工具环境对比](agent-tasks.md#reference-env)采用 network none，不能借用其任务耗时宣称 Docker bridge 或公网性能已经测过。

### 适用边界 {#acceptance}

没有公网、TLS、DNS 或真实模型延迟对照；本地首字节不等于模型 TTFT。网络路径与 host-network OCI 的边界不同。

