# 真正的批量 packet I/O 已生效，吞吐和 CPU 增益尚无有效完整对照

## Motivation

VMM 发包和 Linux peer 收发已经合并系统调用，完整四 VM 故障预检通过。独立诊断显示调用数明显减少，但正式性能批次因基线启动退出 125 而停止，不能据此发布吞吐或 CPU 提升比例。

相同混合预检中，VMM stream TX 调用记录约从 **72,760 次降至 27,035 次**；peer 发送路径约从 **72,703 次降至 31,573 次**，接收从 **99,861 次降至 53,236 次**。这些是独立 profile 的累计检查点，包含恢复流量和 EAGAIN，不是有效正式性能结果。

## 实验设计

测量数据校验、系统调用次数和实际批次大小，作为批量 I/O 是否落地的依据。它们区分一次系统调用处理多个包与在循环中逐包调用。调用更少说明跨内核边界的次数降低，完成相同工作所需时间和 CPU 是否降低仍须独立对照。

本报告属于 [B-FOUR-PEER-ENG](../README.md#b-four-peer-eng) 工程实验，不更新用户 benchmark 页。基线为 `09a7df92` 的 GNU release，已经包含此前 read-ahead、guest gather write 和 smoltcp 按需轮询。候选只在该冻结基线上增加本报告的批量 I/O、私有测试和 crate README；源码只差七个代码文件及两份 README，其他工作区修改未带入。制品、源码和验证清单见[构建收据](.data/batch-20261024/build-receipt.json)与[差异验证](.data/batch-20261024/source-verification.json)。

实现保持既有线程和协议：

- VMM 从已可用 TX descriptor 拷贝最多 32 帧到可复用的独立 buffer，使用一次 `sendmsg` gather write。每帧包含 virtio header 的大小不超过 65,562 字节；逻辑暂存帧数和大小均有界。复制完成才发布 used descriptor，guest 可复用已完成 buffer；socket 背压不保留 guest RAM lease。
- stream 短写按帧索引和帧内字节游标续写，包括截断在长度头内的情况；未完成批次先排空，再接收下一批。不会等待凑批，也不拼接复制整批 payload。畸形 descriptor 完成但不发送；其他 datagram/TAP backend 的 TX 路径保持原样。
- peer 使用固定可复用的 32 包 staging，按目标在已有 connected socket 上执行 Linux `sendmmsg`；一轮 guest 读结束或 staging 满即发送。部分成功后只继续未发送的 datagram；遇到 WouldBlock 丢弃该目标剩余包，其他目标继续，没有扩大 socket 预算或增加跨轮次拥塞重试队列。
- Linux `recvmmsg` 一次最多读 32 包，通过 Tokio readiness 处理 WouldBlock。每包仍独立验证来源路径、代际和帧；截断数据保留拒绝标记。其他 Unix 平台使用单消息 fallback，不增加每包 pathname 分配。

MTU 1500、1514 字节 peer frame 上限、PVN1 header、端到端 guest TCP、线程数及每目标 16 KiB socket 发送预算均保持不变；没有增加 GSO 或多队列。

两臂使用同一静态 musl worker 和 scratch 镜像、IP、固件及编译参数。Linux/KVM 每组四 VM，每个 guest 配置 384 MiB RAM，整个实例 scope 硬限制 512 MiB、swap 0、1 CPU quota，逐线程核对 CPU 4–7 affinity；控制器 CPU 8–9。冻结固件为本地 `libkrunfw.so.5.6.2`，guest Linux `6.12.109`，不是新编译的当前 `fw/` 制品。Linux/KVM 是实际验证平台；macOS 编译和性能没有实测。

正确性预检验证 12 方向 DNS、慢读一秒时健康通信、停止节点后的剩余三节点通信、重启恢复和完整批传输/四跳 RPC。profile 独立注入仅本实验进程的计数器，记录 mmsg 调用、成功 message 数及字节；每线程累计检查点去重取最大值，退出未刷新的尾部为下界，不能当精确丢包率或 CPU profile。

正式方案预声明 3 对预热、30 对随机交替、每条件四个新 VM，依次运行 12 方向 RPC、总计 48 MiB 批传输和四跳 RPC。全部慢样本保留，校验、预算、外部干扰或启动错误使整批无效。只尝试一次正式批次，不从失败批次选取已完成部分，不合并或重试以获得显著性。与旧 Docker 数据不计算优化比率。

## 实验数据和分析

**独立 profile：每臂一次完整混合预检；以下为累计检查点近似计数，非正式性能样本。**

| 路径 | 基线 | 候选 | 解释 |
|---|---:|---:|---|
| VMM stream TX：`send` → `sendmsg` | 72,760 次 | 27,035 次 | 合并已就绪 TX 帧 |
| 后端 peer TX：`write` → `sendmmsg` | 72,703 次 | 31,573 次 | 基线 `write` 含少量 eventfd 唤醒写，无法按 FD 精确拆分 |
| peer RX：`recvfrom` → `recvmmsg` | 99,861 次 | 53,236 次 | 包含 29,019 / 26,276 次 EAGAIN |
| 候选每成功 `sendmmsg` 的 message 数 | — | 约 2.25 包 | 70,302 messages / (31,573 calls − 340 EAGAIN) |
| 候选每成功 `recvmmsg` 的 message 数 | — | 约 2.48 包 | 66,994 messages / (53,236 calls − 26,276 EAGAIN) |

原来的逐帧边界确实已被系统调用批量化取代。上限 32 不表示每次实际凑齐 32：混合预检包含大量单包 RPC、ACK 和停启控制流量，也没有人为等待。不同臂的重传和检查点未刷新尾部会改变观察到的包数，不能把调用变化直接解释为相同比例的 CPU/吞吐收益，也不能将发送/接收 message 差作为丢包数。

**正确性与工程检查：**

- `JUST_TEMPDIR=/tmp just test pvisor-overlaynet pvisor-vm -- --features pvisor-vm/net --offline`：**685 通过，8 项既有跳过**。新增四项私有测试覆盖最大帧/短写游标、guest buffer 复用、畸形 descriptor、mmsg 部分发送顺序、来源路径及截断/空 datagram。
- 两 crate 的 all-targets clippy 和 warnings-denied rustdoc 通过；隔离候选的 CLI/gateway release 构建通过。公开 API 与 wire protocol 未变化。
- 未注入计数器的[完整四 VM 故障预检](.data/batch-20261024/preflight-clean/summary.json)通过；[独立 profile 预检](.data/batch-20261024/diagnostic/summary.json)也通过。两批的干扰检查均未命中外部工作。
- [独立复核](.data/batch-20261024/audit.json)读取了上述预检、profile 及正式尝试已完成阶段共 **472 个客户端原始输出**，内容和数量均正确；核对 **152 份限制记录**，每实例 512 MiB、swap 0、1 CPU 配额和线程 affinity 均符合要求。已完成通信阶段无 OOM/max 事件。该复核不使失败的正式批次重新有效。

**没有可发布的完整正式性能结果。** 正式尝试完成 16 个含预热配对轮次后，第 17 轮候选完成、基线在启动工作负载前退出 125；日志为空，completion 为 `code:125, error:null`。原始记录位于[失败实例](.data/batch-20261024/formal/16-baseline/owner-records/p4batch-59238d-0/completion.json)及[整批失败](.data/batch-20261024/formal/failure.json)。该批 72 次干扰检查未命中外部工作。整批停止，未对已完成部分计算吞吐或 CPU 提升，也没有自动重跑。

这次退出来自未包含本次批量 I/O 的基线，与此前基线启动退出 125 的现象一致；根因仍未确定。不能把它描述为候选转发内容错误，也不能由候选已完成的通信证明长期无故障。启动问题独立于本次新增的性能收益结论，仍需单独定位。

更早的预检入场和运行记录因外部构建/daemon KVM 测试被门禁拒绝，均保留在 `.data/batch-20261024/preflight*`，不纳入成功预检或正式数字。随后等待连续 20 秒空闲窗口，才执行上述预检、profile 和唯一一次正式尝试；没有终止外部工作或修改全局网络参数。

源码和逐包/RTT 输出、资源快照、完整 guard 日志、制品及随机 schedule 均保留在 `.data/batch-20261024/`。[系统调用原始汇总](.data/batch-20261024/diagnostic/counts.json)与[复现说明](README.md#comparing-peer-forwarding-implementations)可供核验。本实验 VM 和专用 `p4batch` 网络已清理，冻结镜像与二进制保留。
