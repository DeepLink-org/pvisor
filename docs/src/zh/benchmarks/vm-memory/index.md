# 多个 VM 如何节省内存？

## Motivation {#motivation}

Agent 在工具执行期间加载依赖、读取文件、处理数据，多个环境可能同时使用相同内容。pVisor 尽早共享重复页面，并在后续读取时保持共享，减少同时驻留的内存。选择机制要看工作集寿命：短命工具和长期驻留的库页面对应的取舍不同。

相同随机内容的预检中，物理池约 1.7 秒达到持续减少 128 MiB 的观测目标，KSM 约需 27.8 秒，但窗口末占用更低。评估时要同时看扫描 CPU、窗口末尾占用和私有 RAM 恢复代价，而不是单一峰值。

## 实验设计 {#interpretation}

测量一段时间内整组的计费内存与 PSS，以及首次完整读回的耗时，作为内存占用。内存占用是这组环境运行时的实际代价；同样读回耗时下占用越低，同一宿主能放下的环境越多。

每组四个实例、每个 64 MiB 数据，整体处在四核 / 2 GiB / 零 swap 的预算内。VM 为 256 MiB、1 vCPU；每个容器限制 256 MiB、一个 CPU 配额。每个条件在就绪后跑一个 60 秒窗口；内容、私有写、退出和资源检查必须通过，不完整的批次不计入。

数值按整组报告。PSS 按映射比例分摊共享页，组计费内存取 cgroup 的 `memory.current`；每 VM 平均值等于总量除以四，不代表再加一个 VM 的边际成本。Docker 使用专用 rootless daemon，Firecracker 与 pVisor 的内核和文件路径不同，全局 ksmd 在组的 CPU 配额之外运行。重复轮次、真实 Agent 延迟和长期容量仍未测量。

### 区分去重与压缩 {#dedup-modes}

下表的标签是实验对照，不是可以任意组合的配置项。物理共享以 4 KiB 页为单位；本地与池化压缩以 64 KiB 块为单位，并拒绝压缩收益不足的数据。不可变文件的 DAX 直接映射同一个 backing 文件，文件读取提示只用于给扫描器排优先级。

| 实验标签 | 内存机制 | 跨 VM 复用 | 后续读取 |
|---|---|---|---|
| `off` | 关闭额外的 RAM 去重与冷压缩 | 不做主动合并 | 读取已有页面 |
| `ksm` | 宿主内核扫描并合并相同页 | 内核管理共享页 | 保持共享，写时复制 |
| `local` | 每个实例压缩冷块，可在本地存储内去重 | 不跨 VM 共享压缩对象 | 恢复实例私有 RAM |
| `physical` | daemon 持有内容相同的共享物理页 | VM 映射同一页 | 保持共享，写时复制 |
| `compressed` | daemon 保存可复用的压缩冷块 | VM 复用压缩对象 | 恢复实例私有 RAM |

## 实验数据和分析 {#results}

### 去重与共享库曲线 {#linux-lifecycle}

[静态工作集曲线](../../lab-notes/vm-memory-performance-analysis.md#pool-static-results)包含原版 Firecracker、独立 rootless Docker，以及 pVisor off、KSM、physical、compressed 和 local。三种工作集分别绘制 0–60 秒的整组计费内存和 PSS，并保留首次读回与 CPU 读数。

![三种工作集的 60 秒内存曲线：Firecracker、Docker 与 pVisor](../../assets/benchmarks/memory-pool-static-idle.svg)

图中每行依次为重复内容、跨实例相同随机内容、各实例独有随机内容；左列是整组 cgroup 计费内存，右列是进程 PSS。横轴为全部实例就绪后的秒数，纵轴为 MiB，每条件 N=1。物理池在相同随机内容上较早下降，KSM 在观察窗末尾占用更低；独有随机内容则显示共享收益有限。

### 回收速度与 CPU 成本 {#reclaim-cost}

下表对应跨实例相同随机内容：每条件 N=1，内存为四实例整组在 60 秒时的计费 MiB，CPU 为 60 秒窗口累计 CPU 秒。回收时间从各自全部就绪的计费占用开始，记录持续减少 128 MiB、并保持至观察窗结束的采样区间；“未达到”表示窗口内未达到此目标。

| 模式 | 60 秒计费 MiB | 组 CPU 秒 | 持续减少 128 MiB 的时间，秒 |
|---|---:|---:|---:|
| firecracker | 365.67 | 0.164 | 未达到 |
| docker-rootless | 423.99 | 0.183 | 未达到 |
| pvisor-off | 398.91 | 0.386 | 未达到 |
| pvisor-ksm | 177.33 | 0.384 | 27.723–27.773 |
| pvisor-physical | 222.61 | 5.924 | 1.688–1.739 |
| pvisor-compressed | 377.89 | 2.838 | 未达到 |
| pvisor-local | 373.48 | 2.341 | 未达到 |

KSM 另有 **0.41 CPU 秒**全局 ksmd 开销，位于组配额外，不能完全归因到本组。组 CPU 包含 VM、内存池和组内控制工作；这些读数衡量完整路径成本。

这组曲线呈现了回收速度与持续成本的取舍：物理池约 1.7 秒达到观测目标，KSM 约 27.8 秒达到该目标，并在 60 秒时取得更低的占用和 CPU 读数。短时工作集更关注前几秒已经释放的内存；长期保留的页面则有更长时间积累共享收益。具体选择取决于页面可共享寿命和 CPU 预算。

这里没有测量真实 Agent 调用时长分布，也未证明亚秒级调用的收益。达到目标的时间不代表首次合并或完整收敛；持久 VM 中的页面可以跨多次工具调用继续共享。

### 读回后还省内存吗？ {#readback-cost}

以下另取重复内容负载，每条件 N=1；内存为四实例整组计费 MiB。完整读回耗时包含四实例的数据读取、逐字节与摘要校验及控制传输。

| 模式 | 读回前 MiB | 读回后 MiB | 完整读回 ms |
|---|---:|---:|---:|
| pvisor-off | 398.92 | 399.09 | 621.99 |
| pvisor-local | 117.46 | 374.58 | 1157.44 |
| pvisor-compressed | 186.17 | 419.73 | 1099.13 |
| pvisor-physical | 149.14 | 149.10 | 839.57 |

物理池读取后保持低占用，但仍有访问成本。压缩路径的低空闲读数会随私有 RAM 恢复而回升；频繁重新访问时，应把这部分成本纳入选择。不同路径还改变块大小、准入与 IPC，不能把全部耗时差异归为压缩算法成本。

### 共享库：文件提示与 DAX {#library-curves}

[共享库曲线](../../lab-notes/vm-memory-performance-analysis.md#pool-library-results)比较相同 `.so` 的加载路径，包含文件提示开启／关闭及显式 DAX。曲线从全部就绪开始；启动期间已发生的共享体现在起始占用中。完整读回和私有写入校验保留，暂不展示 post-write 曲线。

![共享库负载的 60 秒计费内存与 PSS 曲线](../../assets/benchmarks/memory-pool-library-idle.svg)

![共享库就绪后前 5 秒的内存曲线放大图](../../assets/benchmarks/memory-pool-library-early.svg)

共享库两图来自同一组 N=1 预检，纵轴均为整组 MiB。前 5 秒放大图保留原始采样点，便于观察文件提示开启／关闭的早期差异；显式 DAX 的共享已体现在起始占用中。扫描 CPU 和读回代价见[详细分析](../../lab-notes/vm-memory-performance-analysis.md#pool-readback)。

### 来源与配置 {#run}

当前曲线的完整来源和复现协议见 [memory 实验记录](https://github.com/DeepLink-org/pvisor/blob/main/benchmark.new/memory/README.md#published-curves-and-archived-history)。原始失败、未完成重复测量和写入后证据均保留，不补入成功预检。

启用物理池见 [daemon 指南](../../guides/daemon/index.md#memory-pool)；VM 压缩、去重和卸载配置见 [CLI 控制选项](../../reference/cli.md#vm-instance-control)。
