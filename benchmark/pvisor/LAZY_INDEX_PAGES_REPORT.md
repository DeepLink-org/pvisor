# 客户端索引页：减少元数据 RPC 是否缩短 Python 导入等待

## 主要结论

**在同机 loopback、固定 NumPy 工作负载下，客户端二进制索引页减少冷请求，但未检出冷启动等待的显著变化；热 Ready 中位数减少 9.72 ms（2.18%）。** 这不是“大幅冷启动加速”的证据，也不回答 Docker、WAN 或并发吞吐问题。

| 指标，ms，n=30/格 | V2 RPC P50 | 客户端页 P50 | 页 − RPC，配对 bootstrap 95% CI |
|---|---:|---:|---:|
| 冷 Ready | 686.32 | 679.32 | −7.00 [−81.52, 20.75]：未检出差异 |
| 冷 Completion | 823.60 | 815.10 | −8.50 [−109.35, 20.17]：未检出差异 |
| 热 Ready | 446.61 | 436.89 | −9.72 [−12.80, −6.92] |
| 热 Completion | 581.88 | 580.01 | −1.86 [−19.86, −0.15]：区间接近零 |

## Motivation

Python 导入包含大量目录查询、候选文件探测和元数据读取。这个工程 A/B 判断：将 B+tree 查询移到客户端是否降低请求成本，以及增加索引传输和本地校验后是否仍改善实际任务等待。请求减少本身不能代替端到端结果。

## 实验设计

- Benchmark **B-LAZY-ENG**，engineering A/B；不进入历史用户启动页。
- 同一冻结静态 release 制品：两模式均设置 `PVISOR_LAZY_IMAGE_V2=1`；RPC 设置 `PVISOR_LAZY_INDEX_PAGES=0`，客户端页设置 `=1`。
- 固定 linux/amd64 镜像 `docker.io/amancevice/pandas@sha256:9a3a94039175ac799ad33c1a207997994ff9b24814e06508dddfa259b1ed9159`；只执行 Python 3.13.14 / NumPy 2.5.2，不导入 pandas。禁用 bytecode 写入，库线程数为 1；校验数组平方和 1240、矩阵乘积和 3680 后输出唯一 Ready 标记，必须成功退出。
- Linux x86-64/KVM，内核 `7.2.8-200.fc44.x86_64`；客户端 CPU 0/1、2 vCPU、2048 MiB guest RAM；服务与代理 CPU 2/3。guest RAM 不是 enclosing process 的总内存限制。
- 服务端已通过支持的命令准备镜像，本机 TCP loopback，无延迟注入，host page cache 为热。冷指新的客户端 XDG 缓存，不是物理冷磁盘；热紧接冷并复用客户端缓存，每次仍创建新 VM、workspace、stage。
- 30 随机配对轮；初始轮及 3 warmups 排除，共 136 次启动、120 次正式样本。无按速度剔除，任何正确性/代理/缓存失败使批次无效。
- 私有 user/mount/PID namespace、tmpfs、真实 `pivot_root` 隔离 Host listener；冻结启动文件摘要一致。计数代理校验 Data 摘要，分别统计 Read 与 Metadata 载荷。
- 中位数差的区间使用 5,000 次配对 trial bootstrap。冻结分布规则将所有格识别为 `unsplit`，并不证明单峰；保留所有慢样本。n=30 的 P95 仅供参考，不作尾延迟结论，不报告 P99。

## 数据和分析

### 请求与传输

以下计数在对应的全部启动中一致，包括排除轮。请求包含 Prepare/Ping；响应字节包括 JSON 帧和原始 Data，不含 TCP/IP 开销。

| 指标 | 冷 RPC | 冷客户端页 | 热 RPC | 热客户端页 |
|---|---:|---:|---:|---:|
| 请求数 | 296 | 234 | 4 | 4 |
| 连接数 | 2 | 2 | 1 | 1 |
| 文件内容，bytes | 36,951,980 | 36,951,980 | 0 | 0 |
| Metadata Data，bytes | 0 | 3,085,982 | 0 | 0 |
| 总响应，bytes | 37,239,665 | 40,065,947 | 816 | 816 |

- 冷 RPC：75 Stat + 38 List + 177 Read + 5 Ping + 1 Prepare。
- 冷客户端页：**零 Stat/List**；51 Metadata + 同样的 177 Read + 5 Ping + 1 Prepare。
- 热模式均只有 3 Ping + 1 Prepare；不传文件内容或索引页。
- 冷请求减少 62（20.95%），但总响应增加 2,826,282 bytes。RPC 的 `metadata_bytes=0` 不代表没有元数据传输，其元数据在 Stat/List 的 JSON 中。

### 本地工作与边界

客户端使用已发布格式中的 64 KiB B+tree 页和文件记录；权限、uid/gid、类型、大小、符号链接等不需下载文件内容。COMMIT 的 revision 摘要认证控制对象与 checksum catalog，catalog 认证页；只允许八个固定元数据对象的有界只读范围。损坏或权限错误不会降级为 RPC 掩盖失败；损坏的本地页可重新读取并校验。

已校验的原始页与解码节点共用 256 项 LRU 和内存预算，避免每个路径组件反复解码整个索引节点。预算跟随共享对象直到最后一个引用释放。初始加载不创建临时线程，以满足 VM runner 创建 Linux user namespace 的单线程条件。持久磁盘缓存没有全局淘汰配额。

仍存在路径从根解析、本地缓存认证和首次页传输成本；本批没有 CPU/I/O profile，不能给各成本分配精确比例。未检出冷延迟差异意味着不能据此宣布冷加速，也不证明在高 RTT 网络上没有收益。热 Ready 的收益小而明确；热 Completion 的区间靠近零，不能扩大为普遍任务加速。

### V1 兼容性

| 组合 | 行为 |
|---|---|
| 旧客户端 → 新服务 | 原请求继续支持；Prepared 的可选能力字段不改变旧字段 |
| 新客户端 → 旧服务 | 缺失能力默认 false；继续 Stat/List RPC，不发送 Metadata |
| 能力服务，envelope 1 | Metadata 使用原 Data 帧；每连接一次请求 |
| 能力服务，envelope 2 | 同样的 Metadata，经持久连接池传输 |
| 旧私有 binding | 缺失能力默认 false；runner 保留 RPC 路径 |
| `PVISOR_LAZY_IMAGE_V2=0` | 关闭索引页、预取、连接复用，保留 V1-compatible 模式 |
| `PVISOR_LAZY_INDEX_PAGES=0`，V2 开启 | 保留此前 RPC/预取及连接池，便于增量对照 |

存储格式及 `pvisor-v1` immutable handle 不变；存储格式、协议 envelope 和客户端优化开关是三个不同概念。

### 验证与证据

- 默认 `just test pvisor`：**541 passed，7 skipped**。
- `PVISOR_LAZY_IMAGE_V2=0 just test pvisor -- --no-fail-fast`：**541 passed，7 skipped**。
- `PVISOR_LAZY_INDEX_PAGES=0 just test pvisor -- --no-fail-fast`：**541 passed，7 skipped**。
- `just test-py benchmark/pvisor/test_lazy_image_v2.py`：**39 passed**。
- 独立 `CARGO_TARGET_DIR=target/lazy-index-pages-build just build release` 成功，编译包括 CLI/daemon 消费者；不替换 `target/release` 或停止原 Host listener。
- 审计核对 136 次输出/退出/VM Run Bundle、1,404 个冻结摘要、2,031 个构建输入、18,901 个 store 条目及 18,295 条代理记录；228 个独立 Data 切片摘要与账目吻合。退出/清理收据无 namespace 残留。

最终原始证据：`.data/lazy-index-pages-formal-20261009-2/`；构建收据：`.data/lazy-index-pages-build-20261009-4/`。独立审计 `derive.py` 可重新生成 [加工 CSV](lazy-index-pages-summary.csv)。[复现命令](README.md#client-index-page-comparison)。较早的失败预检及回退正式批次保留在各自 `.data/` 目录，不与本表合并，也不据跨批次差值宣称收益。

```text
report.json SHA-256
517ee5d3966b6fd18b8a19f188034358b68558aaf8925565097039cd8fc008b7
pvisor SHA-256
579ba30b3873b44175d9a99492a3753bdd3e824f0eff32d7e3a1f09b58d5117f
pvisor-cache SHA-256
d7212a3ae6ca6c7289631ae86b0109028a6dc662c9ab3d67f52362a492a65b9a
```

证据限制：构建收据与冻结输入一致，不代表 hermetic 可复现构建；未保留完整 wire 字节、每次环境快照或独立事件时间轨迹。namespace 清理由历史收据验证；host 树为 read/write bind，没有完整的测量前后 host 文件/listener 清单，因此不将其解释为只读 host 隔离证明。七项跳过测试、macOS/HVF、live S3、真实 WAN、并发及其他工作负载未覆盖。
