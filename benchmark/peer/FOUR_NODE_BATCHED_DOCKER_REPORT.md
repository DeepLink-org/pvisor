# 批量 I/O 版本的四实例通信：仍明显落后于 Docker bridge

## Motivation

当前批量 I/O 版本的四个 pVisor VM 在每实例 512 MiB 宿主内存硬限制下完成了复杂通信测试，但吞吐和延迟仍明显落后于四个 Docker bridge 容器。减少 packet 系统调用尚未消除整个 VM 网络路径的性能成本。

30 对正式样本中，全互连批传输的聚合有效吞吐中位数为 **pVisor 1.373 Gbit/s、Docker 10.097 Gbit/s**；256 字节 RPC RTT P50 中位数为 **349.49 µs、12.07 µs**，四跳 RPC 为 **928.75 µs、19.60 µs**。同批中位数水平下，Docker 吞吐约为 pVisor 的 **7.36 倍**，pVisor 两种 RPC 延迟分别约为 Docker 的 **28.95 倍、47.38 倍**。正式批次 0 失败、0 OOM、0 剔除。

## 实验设计

测量逐字节校验通过的应用有效吞吐、RPC 往返时间和实例 cgroup CPU/内存，作为四实例通信成本。它们反映固定实例预算下完成并发通信的速度和资源消耗。吞吐越高、延迟和资源消耗越低，同样预算能完成的通信越多。

本报告属于 [B-FOUR-PEER-ENG](../README.md#b-four-peer-eng) 工程对照。使用当前工作区已验证的批量 I/O 制品，核对源码及 SHA-256 后冻结：基于 `09a7df92`，增加 VMM TX gather 及 Linux peer `sendmmsg/recvmmsg`，同时包含此前 read-ahead、guest gather write 和 smoltcp 按需轮询。构建为 Rust `1.98.1`、GNU `release`、CLI gateway feature；不是额外速度优化的 `performance` profile。没有为本次测量修改运行时代码。

宿主为 AMD Ryzen 7 9700X、Linux `7.2.11-200.fc44.x86_64`，Docker Engine `29.8.2`。pVisor 使用冻结的本地 `libkrunfw.so.5.6.2`，guest Linux `6.12.109`；没有重新编译当前 `fw/`。完整身份见[制品收据](.data/docker-batch-20261024/build-receipt.json)、[当前源码核验](.data/docker-batch-20261024/audit.json)和[正式来源](.data/docker-batch-20261024/formal/manifest.json)。

| 条件 | pVisor | Docker |
|---|---|---|
| 实例 | 4 个真实 KVM VM | 4 个 rootful 容器 |
| 每实例宿主硬限制 | owner、run 前端、VMM、网络线程所在 scope：512 MiB | 容器 cgroup：512 MiB |
| guest 内存 | 配置 384 MiB，guest 可见约 391 MiB | 共享宿主内核，以 cgroup 限制内存 |
| swap / CPU quota | 0 / 1 CPU，1 vCPU | 0 / 1 CPU |
| 实际线程 affinity | CPU 4–7，逐线程核对 | cpuset CPU 4–7，逐线程核对 |
| 网络 | 直接 Unix datagram peer | 专用 Docker 用户 bridge |
| 地址 / MTU | `10.231.70.11`–`.14` / 1500 | 相同 |
| 程序 | 同一冻结静态 musl worker、scratch/COPY 输入 | 相同，另从镜像内提取 worker 核对字节一致 |

pVisor 镜像的不可变 revision、catalog 和构建记录已归档；Docker 镜像 ID、镜像归档及镜像内 worker 均保留。两个引擎使用专用网络和同一输入，没有修改宿主 sysctl。用户 systemd 子树未委派 cpuset controller，pVisor 使用 `taskset` 并实际验证线程 affinity；CPU quota、内存和 swap 限制直接核对内核 cgroup 文件。控制器及采样器运行于 CPU 8–9。

| 负载 | 输入及拓扑 | 计量 |
|---|---|---|
| 全互连 RPC | 12 条有向连接，每条 500 次 256 字节 echo，共 6,000 次 | 请求往返时间、内容逐字节核验 |
| 全互连批传输 | 12 方向各 4 MiB，共 48 MiB，64 KiB 块 | 总 forward payload 除以最慢客户端持续时间，接收端完整校验 |
| 四跳 RPC | 每节点 200 次 256 字节请求绕四节点一圈，响应原路返回，共 800 次 | 计原始 payload 一次，不累加各跳字节 |

每个条件创建四个新实例，两个引擎不同时运行。按预声明随机种子交替执行 3 对预热、30 对正式样本；组内固定 RPC → 批传输 → 环形 RPC。连接建立、实例启动、客户端启动及 GO 屏障等待不计网络时钟，宿主页缓存保留，不能视为冷缓存或冷邻居性能。每个有效慢样本均保留；校验、预算、启动或干扰检查失败会使整批停止，不补选或合并。

报告每轮指标的中位数；延迟先取轮内 RTT P50/P95，再取 30 轮中位数，P95 仅参考，不报 P99。配对差为每轮 pVisor 减 Docker，用 5,000 次配对 bootstrap 给出 95% 区间。逐轮排序及四分位数已复核：批传输波动较宽，Docker RPC RTT P50 范围为 7.33–14.96 µs，但未见清晰分离的双峰。

CPU 为网络操作窗口内四实例 cgroup 的累计 CPU 秒，包括 guest/VMM、应用校验及输出等工作，不是转发函数的 CPU 独占时间。内存每 50 ms 采样，短暂峰值另由 `memory.peak` 保留。Docker 既有 dockerd/containerd、shim 及共享宿主内核的部分成本在容器预算之外；另记录服务观察值，不能把本实验当成整个系统在相同整机 cgroup 内的纯机制对照。

## 实验数据和分析

**N=30 对正式样本；差值为每轮 pVisor 减 Docker 的中位数。**

| 指标 | pVisor 中位数 | Docker 中位数 | 配对差中位数 [95% CI] |
|---|---:|---:|---:|
| 批传输有效吞吐，Gbit/s | 1.373 | 10.097 | −8.696 [−8.829, −8.473] |
| 全互连 RPC RTT P50，µs | 349.49 | 12.07 | +337.80 [+335.25, +339.14] |
| 全互连 RPC RTT P95，µs，参考 | 569.34 | 27.75 | +542.00 [+537.86, +548.04] |
| 四跳 RPC RTT P50，µs | 928.75 | 19.60 | +909.13 [+907.28, +914.04] |
| 四跳 RPC RTT P95，µs，参考 | 1,140.38 | 27.50 | +1,114.53 [+1,108.16, +1,121.47] |
| 全互连 RPC：四实例累计 CPU 秒 | 0.7120 | 0.0237 | +0.6879 [+0.6844, +0.6921] |
| 批传输：四实例累计 CPU 秒 | 0.7854 | 0.1495 | +0.6360 [+0.6250, +0.6463] |
| 四跳 RPC：四实例累计 CPU 秒 | 0.6427 | 0.0120 | +0.6307 [+0.6271, +0.6345] |

差异区间均未跨零。批传输逐轮范围为 pVisor **1.065–1.849 Gbit/s**、Docker **9.079–11.390 Gbit/s**，较慢和较快样本均保留。当前实现虽然已经合并系统调用，整个 guest/virtio/VMM/peer 路径与 Docker 共享宿主内核路径之间仍有明显差距；本批没有逐段 CPU profile，不能据此定量归因于某个线程、复制或唤醒。

**资源和完整性：**

| 检查 | pVisor | Docker |
|---|---:|---:|
| 每实例 `memory.max` | 512 MiB | 512 MiB |
| 正式样本最高单实例 `memory.peak` | 46.80 MiB | 11.54 MiB |
| OOM / OOM kill / memory max 事件 | 0 / 0 / 0 | 0 / 0 / 0 |
| RPC / bulk / ring 的 CPU throttling 事件 | 17 / 11 / 0 | 0 / 0 / 0 |
| 正式全互连 RPC 校验次数 | 180,000 | 180,000 |
| 正式四跳 RPC 校验次数 | 24,000 | 24,000 |
| 正式批传输校验量 | 1.40625 GiB | 1.40625 GiB |
| 正式批次失败 / 剔除 | 0 / 0 | 0 / 0 |

内存数字是 cgroup 归属记账，guest RAM 没有被全部触碰，既有缓存和共享宿主内核也不全部归入实例；不能把约 47 MiB 当成满载 VM 的大小，或将两列直接外推为生产部署密度。配额限制的影响已包含在测量中，没有筛掉 throttling 样本。

Docker 操作窗口内，dockerd/containerd 合计 CPU 增量的中位数分别约为 RPC 0、批传输 0.01 秒、环形 RPC 0.005 秒；读取以宿主调度 tick 为粒度，零不表示无开销，0.005 是样本中位数而非 5 ms 精度。Docker 窗口结束时 dockerd、containerd 的 RSS 快照中位数约 **120.27 MiB、57.61 MiB**。这是既有共享服务的观察值，未覆盖全部 shim/内核开销，也不能与容器 cgroup 内存直接相加当作新增物理占用。

独立复核读取了含预热在内的 **792 个客户端原始输出**和 **264 份正式资源限制记录**，重新计算 payload、持续时间、吞吐、RTT 及 cgroup CPU，均与结果一致。正式运行的 **130 次干扰检查**均未发现可见的同用户外部构建、测试或 KVM 工作；不可读进程及其他宿主工作仍是覆盖缺口，不等于整机完全隔离。

独立四节点预检的两个引擎均通过 12 方向 DNS、暂停接收一秒期间健康流继续通信、停止节点后其余三个节点通信，以及重启后的全互连恢复。完整正式批次中，pVisor 的 132 次新 VM 启动均成功；此前实验的启动退出 125 没有在本批复现，根因仍未定位，不能把本次通过描述为已修复。应用使用 TCP，未在本批采集链路丢弃和重传计数；最终内容正确不等于底层零丢包。

本报告与[旧 Docker 对照](FOUR_NODE_REPORT.md)、[系统调用批量化诊断](PACKET_BATCH_REPORT.md)分别保留，不用跨批次差异计算优化收益。准备阶段的镜像核验命令错误和修正单独记入[准备诊断](.data/docker-batch-20261024/preparation-diagnostics.json)，没有启动 guest 或进入测量。

复现见[运行说明](README.md#four-real-vms-versus-docker-bridge)。[正式统计](.data/docker-batch-20261024/formal/summary.json)、[来源与 schedule](.data/docker-batch-20261024/formal/manifest.json)、[独立复核](.data/docker-batch-20261024/audit.json)、[故障预检](.data/docker-batch-20261024/preflight/summary.json)、全部 RTT、资源曲线和制品均保留在 `.data/docker-batch-20261024/`。本次实例和两个专用网络已清理，冻结镜像及二进制保留供复现。
