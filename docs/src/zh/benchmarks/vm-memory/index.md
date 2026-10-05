# 如何判断 VM 冷页回收是否值得开启？

## 主要结论 {#conclusions}

**现有数据不足以证明 pVisor 比 Docker、Firecracker 或 QEMU 更省整机物理内存；冷页回收应按实际工作集和恢复等待评估。** macOS 重复数据探针观察到 guest 冷页驻留下降，但池自身也消耗内存，不能将驻留下降换算成可增加的 Agent 数量。

| 需求 | 选型含义 |
|---|---|
| 闲置环境较多、可接受首次访问等待 | 测量实际工作集的净物理内存与恢复成本 |
| 活跃编译、随机或不可压缩数据 | 现有重复数据探针不能证明收益 |
| 与 Docker / Firecracker / QEMU 比容量 | 没有同工作集的净物理内存排名 |

## Motivation {#motivation}

等待模型响应的 VM 可能保留冷页，但恢复时仍要读取这些页。容量规划需要知道整机究竟省了多少，以及下一次工具执行是否变慢；只看 guest 驻留会漏掉回收池和恢复的成本。

## 实验设计 {#interpretation}

<a id="experiment-design"></a>

B-VM-MEMORY 使用 Apple M4、24 GiB RAM、macOS/HVF。启用与关闭共享冷页池分别运行两个 VM，每个写入 64 MiB 的重复可压缩数据；参数矩阵 40 次，2 GiB 延长观察 4 次。检查恢复后数据完整，同时观察 guest 驻留、池/在途数据、footprint 和 CPU。

RAM proxy 是 guest 驻留与池/在途数据的代理量，并非整机或 cgroup 的净物理内存。下表是观察窗口的汇总，不是 P50 或置信区间；样本不足以支持尾延迟结论。随机数据、真实工具工作集及同工作集 Docker/Firecracker/QEMU 对照未测。

## 实验数据和分析 {#results}

<a id="experiment-data"></a>

### 已观察的驻留变化 {#measurements}

单位 MiB；每组两个 VM。配置与观察窗口分别列出，不合并。

| 每 VM 配置 RAM | 全部就绪后的观察窗口 | 冷页池关闭 RAM proxy | 冷页池开启 RAM proxy |
|---|---|---:|---:|
| 256 MiB | 18–33 s | 231 | 27 |
| 512 MiB | 18–33 s | 243 | 61 |
| 2 GiB | 60–90 s | 309 | 124 |

这些观察说明页驻留会转移到其他存储与恢复路径，不能证明整机物理内存下降相同幅度。需要把池内存、backing、CPU 和恢复后的工具等待一起计入，才能决定是否开启。

### 与已有方案的证据范围 {#linux-lifecycle}

| 方案 | 相同工作集的净物理内存 | 恢复后工具耗时 | 可用比较 |
|---|---|---|---|
| pVisor macOS 冷页池 | 未建立完整净节约结论 | 重复数据探针，未测真实工具 | 开启/关闭的驻留观察 |
| Docker / Podman | 未测 | 未测 | [空闲并发占用](../density.md)，口径不同 |
| Firecracker / QEMU | 未测 | 未测 | [启动与工具任务](../compare-runtimes.md)，不能替代内存测量 |

Linux 与 macOS 的能力和内存口径不同，不能搬用这些驻留数字。独立 snapshot CLI 已退役，其数字不用于当前产品的恢复预算；当前可用入口见[CLI 参考](../../reference/cli.md)。
