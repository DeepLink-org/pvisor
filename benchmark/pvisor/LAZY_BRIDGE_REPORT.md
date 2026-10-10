# 私有网络桥连接复用能否减少 lazy image 启动等待

## 主要结论

**Unix 建连从冷启动 220/290 次降到通常 1 次，但固定 NumPy、同机 loopback 下未检出 Ready 或 Completion 的显著变化。** 这项改动补齐连接复用和 idle/慢连接资源隔离，不是启动加速证据。

下表为 ms，n=30/格；Δ 是持久桥减旧桥，95% CI 使用 5,000 次配对 trial bootstrap。所有区间包含零。两个模式是独立批次，不合并或据跨模式差值比较性能。

| 批次 / 缓存 | Ready P50 旧桥→持久桥 | Δ Ready 95% CI | Completion P50 旧桥→持久桥 | Δ Completion 95% CI |
|---|---:|---:|---:|---:|
| 索引页 / 冷 | 692.68 → 693.27 | [−4.51, 7.70] | 836.23 → 836.05 | [−20.37, 0.85] |
| 索引页 / 热 | 441.45 → 445.31 | [−0.20, 7.73] | 581.95 → 583.60 | [−0.46, 21.00] |
| RPC / 冷 | 684.28 → 684.66 | [−15.48, 13.49] | 824.81 → 843.99 | [−8.99, 33.59] |
| RPC / 热 | 454.55 → 453.32 | [−6.71, 3.60] | 600.63 → 600.83 | [−18.36, 17.68] |

## Motivation

外部 cache 服务的 TCP 连接池不能消除 runner 到私有 Unix 桥的逐请求建连。这个工程 A/B 判断补齐这一跳是否改善短任务等待，并分别验证两段连接；还需要确保持久连接的 idle、半帧和阻塞写入不占满桥的四个执行 worker。

## 实验设计

Benchmark **B-LAZY-ENG**，engineering A/B；不进入历史 Docker 用户页。同一冻结静态制品，两模式均启用客户端/上游 V2，仅 `PVISOR_LAZY_BRIDGE_V2=0/1` 改变。该开关同时比较旧的 socket-worker 调度和新的 handler/执行 worker 分离，不能解释为仅协议版本的效应。

- 独立 `bridge-pages` 批次固定索引页开启；独立 `bridge-rpc` 批次固定索引页关闭。
- 固定 `docker.io/amancevice/pandas@sha256:9a3a94039175ac799ad33c1a207997994ff9b24814e06508dddfa259b1ed9159`，只执行 Python 3.13.14 / NumPy 2.5.2，不导入 pandas。禁用 bytecode 写入，库线程数为 1；校验平方和 1240、矩阵乘积和 3680 后输出唯一 Ready，必须退出 0。
- Linux x86-64/KVM、内核 `7.2.8-200.fc44.x86_64`；客户端 CPU 0/1、2 vCPU、2048 MiB guest RAM；上游服务/计数代理 CPU 2/3。桥继承客户端 CPU 亲和性。guest RAM 不是整个进程树的内存限制。
- 预准备镜像，本机 TCP loopback，无人工延迟，host page cache 为热。冷为新客户端 XDG 缓存，热紧接冷并复用缓存；每次新 VM/workspace/stage。
- 每批 30 随机配对轮，初始轮及 3 warmups 排除，每批 136 次启动、120 正式样本；无速度剔除，任一正确性/计数/隔离错误使整批无效。
- 私有 user/mount/PID namespace、tmpfs、真实 pivot_root 隔离 Host listener。只在构建与测试完成后采样，两个正式批次顺序执行。
- 可选共享 mmap 计数避免逐请求日志/磁盘写入，在 namespace 子进程被 reap 且清理验证后读取。旧桥被拒的首次 V2 Ping 不记作成功帧；新桥的两次协商 Ping 计入帧数。

## 数据和分析

### 两段连接与载荷

冷启动计数；连接为每次启动的 P50。

| 指标 | 索引页旧桥 | 索引页持久桥 | RPC 旧桥 | RPC 持久桥 |
|---|---:|---:|---:|---:|
| runner→桥 Unix accepts | 220 | 1 | 290 | 1 |
| 桥转发实际请求 | 219 | 219 | 289 | 289 |
| 桥→服务 TCP 连接 | 1 | 1 | 1 | 1 |
| 含 host Prepare 的上游总连接 | 2 | 2 | 2 | 2 |
| 文件内容，bytes | 36,951,980 | 36,951,980 | 36,951,980 | 36,951,980 |
| Metadata Data，bytes | 3,085,982 | 3,085,982 | 0 | 0 |

索引页持久桥的正式 trial 27 使用两个 Unix/TCP 桥连接，其余正式冷启动使用一个；额外上游协商增加 2 Ping 和 44 响应字节。实际操作、载荷完全相同，无错误或重试证据；符合有界池在重叠调用下分配额外连接，精确调度原因未记录。该样本保留。

- 索引页桥转发：177 Read + 42 Metadata。
- RPC 桥转发：177 Read + 74 Stat + 38 List。
- 所有热启动桥的连接/帧/转发请求均为零，文件及二进制元数据传输为零。上游仅 host Prepare/Ping。
- 旧桥 accepts 比成功请求多一次，因为客户端先进行无副作用 V2 探测，EOF 后降级；不是额外文件请求。
- RPC 元数据在 JSON 响应中，不计入 Metadata Data 字节。

### 为什么没有明显提速

持久连接消除了 Unix socket 的反复创建、accept 和关闭，但没有减少实际转发次数、跨进程通信或 177 次内容读取。两个批次的上游调用累计耗时 P50 仍在约 138–149 ms；它包含连接池等待、网络、响应读取/校验，不是纯服务计算，也不能直接作为 Ready 关键路径分解。

新的 handler/执行队列增加一次调度交接，因此“减少 connect 次数”并不等价于减少同样数量的进程唤醒。热启动没有桥请求，复用无法优化这部分；新桥仍会创建固定执行 worker。现有证据支持将 Unix 建连视为较小成本，不能精确分配 VM 固定启动、内容验证和本地索引的耗时比例。

队列等待计数不能做同口径优化结论：旧桥从 accepted socket 入队起算，新桥从完整请求解析后入队起算。CSV 将其标为描述性计数。共享计数器额外原子操作/计时开销未单独测量，两模式均启用；不将结果解释为无 telemetry 的精确成本。

冻结分布规则未识别 Ready/Completion 为分离分布，所有慢样本保留。P95 仅参考，不宣称尾延迟改善或 P99。没有测量 WAN、吞吐或并发性能收益。

### 实现和资源测试

`crates/pvisor/src/image/cache/network.rs` 默认支持 V1 单交换和 V2 顺序持久交换。最多 32 个连接 handler、4 个执行 worker、16 个等待请求。idle/半帧读/响应等待/写入在 handler 中；worker 仅执行完整请求并非阻塞交付结果。队列满返回完整 busy 帧，连接接入满则关闭；超时后请求可能已执行，禁止自动重放。

每次交换继续检查固定 handle 与元数据白名单。线程只存在于独立桥子进程，不在进入 user namespace 前的 VM runner 创建。子进程退出负责 detached handler/worker 清理，非优雅排空。V1 开关保留旧协议和调度用于兼容/对照。

测试覆盖真实 Unix 多次交换和 Data hash、V1 关闭、四个 idle 加第五条真实请求、半帧与阻塞写入不占执行 worker、接入/队列饱和及清理、被 kill 后 mmap 计数保留。连接 I/O timeout 不是整帧绝对 deadline；持续滴入数据可长期占一个 handler，但数量有界。

### 验证、来源与限制

- 默认、`PVISOR_LAZY_IMAGE_V2=0`、`PVISOR_LAZY_BRIDGE_V2=0` 完整 Rust 回归：各 **548 passed，8 skipped**。
- `just test-py benchmark/pvisor/test_lazy_image_v2.py`：**45 passed**。
- `CARGO_TARGET_DIR=target/lazy-bridge-build just build release` 成功，含 CLI/daemon 消费者。独立目录，不替换原 release 或停止现有 listener。
- 独立审计全部 **272 次**正确输出、退出 0、唯一 VM Run Bundle；每批 1,400 个冻结源码与构建证明一致。桥原始统计与上游请求账目、内容切片摘要一致；无拒绝、队列满、busy、代理错误。namespace 清理收据通过。

[索引页加工 CSV](lazy-bridge-pages-summary.csv) · [RPC 加工 CSV](lazy-bridge-rpc-summary.csv) · [复现说明](README.md#private-host-network-bridge-comparison)。原始证据为 `.data/lazy-bridge-{pages,rpc}-formal-20261009/`，各有可重复的 `derive.py`；构建证明在 `.data/lazy-bridge-build-20261009/`。不与历史批次合并。

```text
pages report SHA-256
5b1ec25c26b253b1448c707753434c2c1425206b2420bd003f8caeed320de11d
rpc report SHA-256
b108f5776a226e4eca136c2b15a93991c4e16afce7ac3dfc975b12bdf9a979d4
pvisor SHA-256
4103413f49ee757d50061e8fe2b2553ae8efc0ea12411f36ee53c08ff720360d
pvisor-cache SHA-256
438508e391c1f7c56a991c6fc78f645b774e2260d604689530f61226447bb7e6
```

构建来源匹配不是 hermetic 可复现证明。计时没有独立事件轨迹；wire body 摘要由保留 store 重建，未保留完整 wire。共享文件不保证重启持久性，kill 后 gauges 可非零，也不是峰值。清理为历史收据核验；host 树 read/write bind，不是完整只读 host 隔离证明。八项 skipped 测试、macOS/HVF、live S3、真实 WAN及其他负载未覆盖。
