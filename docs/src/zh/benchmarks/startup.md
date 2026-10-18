# 启动一个可用环境要等多久？

## Motivation {#motivation}

Agent 应用存在大量突发、短时的调用：一个任务结束就丢弃环境，下一个任务重新创建。每次调用都要重新支付启动等待，成本随调用量累积。启动延迟决定第一条命令多久能开始，也决定开发者愿意多频繁地为单个任务创建隔离环境。

pVisor 在 VM 上的优化把启动压到接近容器：Ready 中位数 130.71 ms，对比 Docker rootless 的 105.03 ms，也低于参考 VM（Firecracker MMIO 287.13 ms、QEMU microvm 260.10 ms、QEMU q35 663.25 ms）。host 与 staged 更快，分别为 33.52 ms 和 50.59 ms。

## 实验设计 {#interpretation}

测量从启动到首条校验通过的 shell 输出，作为环境 Ready 时间。这段时间是系统响应调用方的能力；Ready 时间越短，整个体系应付突发性大规模请求的能力越强。

对照使用 Linux x86_64 / KVM，AMD Ryzen 7 9700X，Fedora 宿主。对照为 native、pVisor host、pVisor staged、pVisor VM、Docker rootless / overlay2、Firecracker（virtio-MMIO）、QEMU q35 和 QEMU microvm。进程固定到 CPU 0,1，VM 均为 2 vCPU / 128 MiB；进程和容器不设内存上限。

Firecracker、QEMU 和 QEMU microvm 使用 Fedora 的完整发行版内核，Linux 7.2.8-200.fc44.x86_64。pVisor 的虚拟化基于 libkrun，使用该项目配套 libkrunfw 的定制内核，Linux 6.12.109。

使用已准备的用户态环境和相同的 `/bin/sh` 输出探针，每次执行使用新环境与私有工作区，VM 每次重新启动，不使用快照或环境池。热页缓存，每组 3 次预热、60 次正式样本，组内随机交替执行。环境准备和输入复制不计入启动时间。内核、设备和存储路径不同，结果反映各配置的完整启动成本；这个探针不代表完整发行版开机、冷镜像、Agent CLI 初始化、并发容量或快照恢复。

Ready 截止到校验后的首条 shell 标记。Firecracker 在校验标记和结果、确认无 panic 后受控终止。失败、panic、输出不匹配或制品改变的样本不计入耗时分布；保留有效慢样本。

## 实验数据和分析 {#results}

### 各配置的 Ready 时间 {#ready}

八个条件于 2026-10-11 测量，3 次预热后各 60 个正式样本。输入为已准备 rootfs、VM 的 2 vCPU / 128 MiB、热缓存和 CPU 0,1 亲和性。Python `perf_counter_ns` 在进程创建前开始，收到完整的 `REFERENCE_READY` 行时停止：包含进程创建、管道读取和观察者调度，不包含解释器启动和输入准备。全部样本还须通过输出、Run Bundle 和输入不变的校验。

{{ table("startup-timing.ready.render.json") }}

pVisor VM 的 Ready 中位数为 130.71 ms，位于 Docker rootless 的 105.03 ms 与参考 VM 的 260–663 ms 之间。host 与 staged 跳过内核和设备路径，分别为 33.52 ms 和 50.59 ms。

表格按每个 `condition` 取[统计 CSV](../../../overrides/assets/benchmarks/data/startup/startup-timing.csv)中最新的有效行；`condition` 与 `batch` 共同定位结果。标准差采用正式样本的样本标准差。整表复测使用 `just benchmark startup/timing`；仅复测 VM 使用 `just benchmark startup/timing "pvisor vm"`。

冷镜像的客户端等待与内容下载见独立的[镜像按需启动](lazy-image-startup.md)实验；镜像服务准备成本单列，样本不与这里的预准备环境合并。

### 来源与复现 {#run}

统计、源码与制品来源和原始记录保存在对应本地批次或文档数据归档。复现见[启动实验手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark.new/startup/README.md)，被测制品见[来源 JSON](../assets/benchmarks/startup-timing.provenance.json)。

[比较方法](index.md#interpretation)
