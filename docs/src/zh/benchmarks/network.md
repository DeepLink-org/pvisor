# 网络代理与 VM TCP 开销

同机 HTTP 小请求的 P50：原生约 0.95 ms、宿主代理 1.24 ms、VM 3.83 ms。流式响应首字节分别约 1.10、1.37、4.57 ms；代理和 VM 的成本在毫秒量级，大块传输的吞吐损失更明显。

## Motivation

模型响应可能持续数秒，但工具调用也会频繁下载、小请求和读取流。把请求延迟、传输吞吐与启动成本拆开，才能判断本地代理的影响。

## 实验设计 {#interpretation}

服务在同一宿主 IP 上提供 HTTP，无 Internet/TLS/DNS。每组 3 次预热、30 个批次；小请求每批 256 个 1 KiB 响应、并发 8、每请求新连接，共 7,680 个请求；bulk 每次 32 MiB；stream 每 2 ms 输出一块、共 10 块。核对响应长度与 SHA256，stream 首字节是响应体首字节，不是模型 TTFT。VM 用 auto TCP 路径；host/OCI 用 proxy，Podman 用 host network。

## macOS

本轮在 Linux 实测，以下工作负载没有 macOS 样本；macFUSE/FSKit 开销与并发容量均未测。既有 macOS/HVF 数据继续保留在 [VM 启动时间](startup.md)与[VM 内存报告](vm-memory/index.md)，不移作本页结果。

## Linux：2026-10-04 {#results}

| Batch | Backend | Batches | 1 KiB P50/P95/P99 ms | 32 MiB P50 MiB/s | Stream first body P50/P95/P99 ms |
|---|---|---|---|---|---|
| main | native | 30 | 0.95/1.42/4.67 | 869.4 | 1.10/1.20/1.40 |
| main | host | 30 | 1.24/2.42/7.16 | 416.8 | 1.37/1.63/1.69 |
| main | vm | 30 | 3.83/8.63/17.36 | 154.7 | 4.57/5.19/6.44 |
| OCI follow-up | native | 30 | 0.99/1.44/4.85 | 861.4 | 1.07/1.18/1.18 |
| OCI follow-up | podman | 30 | 1.01/1.43/9.07 | 853.7 | 3.53/3.60/3.64 |
| OCI follow-up | container | 30 | 1.30/2.30/10.06 | 811.0 | 3.78/3.98/4.05 |

### 分析与拒绝验证

小请求 host 相对 native P50 增加约 0.29 ms，VM 增加约 2.88 ms。大块传输主批次 P50 为原生约 869 MiB/s、host 417 MiB/s、VM 155 MiB/s；吞吐包括连接和读取，数据摘要校验在传输计时之后。网络 worker 总时长还包含摘要计算。它们是本机 HTTP 通路能力，不预测公网带宽。

host deny-all 的私有网络命名空间和 VM deny-all 各 **30/30** 次阻止 direct socket，Bundle 同时确认 network_non_bypassable。普通 host proxy 的限制是协作式；direct socket 可以绕过，拒绝耗时不能当成吞吐优势。
### 用熟悉的请求和下载理解这些数字 {#baseline-meaning}

原生 HTTP 是没有 pVisor 通路的基线；Podman/crun 是已测的普通 OCI 路径。本轮 host proxy 比原生每次小请求增加约 0.29 ms，VM 增加约 2.88 ms。如果外部服务本身耗时 100 ms，单看这部分附加成本约为 0.3% 和 2.9%；这是代入假设的解释，不是公网或模型 API 实测。

下载大文件时不能用同样的直觉：32 MiB 按本轮吞吐折算，原生传输约 37 ms、VM 约 207 ms。大量本机小请求、下载依赖、传模型响应是不同负载。新 [Docker 完整工具环境对比](agent-tasks.md#reference-env)采用 network none，不能借用其任务耗时宣称 Docker bridge 或公网性能已经测过。

## 边界与下一轮 {#acceptance}

请求 P95/P99 是同批次嵌套样本，连接间可能相关；没有宣称独立请求统计置信区间。OCI 补测与主批次分别列出，host network 未测 Docker bridge/CNI。UDP/IPv6/QUIC、真实模型 SSE、HTTPS 解密及公网 API 波动不在本轮。

## 复现与证据 {#run}

从仓库根目录运行；输出目录必须是新目录。动态 firmware 入口需要 GNU/Linux 版 CLI；静态 musl 制品使用另一套 firmware 入口，不能混用。Linux、KVM/FUSE/user namespaces、Python 3.14、Rust/GCC、Git/rg、Node 24/npm、Podman/crun 是本机工具环境；Agent 套件另需 Claude/Codex CLI。

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --suites network --samples 30 --warmups 3 --network-backends native,host,vm
```

先用 `--samples 1 --warmups 0` 检查环境。脚本、输入定义和正确性断言在 `benchmark/pvisor/v1/`；报告会复制二进制、firmware 和当轮脚本并记录摘要。失败不会进入性能分布；准备、策略拒绝和工作负载错误分别保留。每个页面写明有效样本数；小样本 P95/P99 只描述本批次，不估计长期尾延迟。

[环境、制品与采样方法](methodology.md#product-v1) · [批次清单](../../assets/benchmarks/product-v1-20261004/manifest.json) · [逐样本汇总 CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [原始报告与日志归档](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz)。报告保留源码的未提交状态；当轮可执行制品 SHA256 是身份依据。归档不含数 GB 的 rootfs、二进制和可重建工作区；输入摘要及每轮脚本保留。
