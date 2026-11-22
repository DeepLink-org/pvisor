# 启动一个可用环境要等多久？

## Motivation {#motivation}

对反复创建同规格沙箱的本地 API 用户，预留一个 VM 槽位能缩短命中时的启动等待；池空时仍要冷启动。对于直接启动的执行器，在已准备用户态、页缓存为热的条件下，pVisor VM 的首条输出等待接近容器，短于所测参考 VM；host 和 staged 的等待更短。

本地 API 串行创建时，预热命中将 `Running` 的中位数等待从 77.89 ms 降到 12.48 ms，将首条正确命令完成的中位数等待从 94.54 ms 降到 38.18 ms。独立的命令行实验中，pVisor VM 的 Ready 中位数为 130.71 ms，Docker rootless 为 105.03 ms。

## 实验设计 {#interpretation}

测量从发送创建请求到返回 `Running` 和到首条正确命令完成的时间，分别作为 API 创建等待和首次使用等待。独立的命令行实验以进程启动到首条校验通过的 shell 输出为 Ready 时间。这些等待越短，调用方越早能使用新环境。

### 本地 API 的 VM 预热池 {#daemon-warm-pool-design}

同一优化构建的 pVisor daemon 在 AMD Ryzen 7 9700X、Fedora Linux 7.3.3、KVM 上运行，使用同一已准备的 Ubuntu 24.04 镜像，每实例 4 vCPU、256 MiB 进程树内存上限。两台 daemon、观察者及 VM 共用固定的宿主 CPU 4–7、四核 CPU 配额、2 GiB 内存上限和零 swap；唯一配置差异是是否预留一个同规格 VM。

通过本机 HTTP 发起请求，计时包含镜像接入和实际服务就绪，不包含 daemon 启动、镜像导入或等待池补齐。冷启动与确认命中的串行请求按固定种子随机交替。每轮另向只有一个空闲槽位的服务同时发送两个同规格请求，分别记录命中和池空回退。预检和三轮正确性预热不计；此后保留所有有效操作，包括采样器自适应预热。每次都验证槽位归属、实际资源上限、命令输出、私有写入和清理；任何正确性失败或 OOM 使整批无效。

### 直接启动执行器 {#direct-launch-design}

对照使用 Linux x86_64 / KVM，AMD Ryzen 7 9700X，Fedora 宿主。对照为 native、pVisor host、pVisor staged、pVisor VM、Docker rootless / overlay2、Firecracker（virtio-MMIO）、QEMU q35 和 QEMU microvm。进程固定到 CPU 0,1，VM 均为 2 vCPU / 128 MiB；进程和容器不设内存上限。

Firecracker、QEMU 和 QEMU microvm 使用 Fedora 的完整发行版内核，Linux 7.2.8-200.fc44.x86_64。pVisor 的虚拟化基于 libkrun，使用该项目配套 libkrunfw 的定制内核，Linux 6.12.109。

使用已准备的用户态环境和相同的 `/bin/sh` 输出探针，每次执行使用新环境与私有工作区，VM 每次重新启动，不使用快照或环境池。热页缓存，每组 3 次预热、60 次正式样本，组内随机交替执行。环境准备和输入复制不计入启动时间。内核、设备和存储路径不同，结果反映各配置的完整启动成本；这个探针不代表完整发行版开机、冷镜像、Agent CLI 初始化、并发容量或快照恢复。

Ready 截止到校验后的首条 shell 标记。Firecracker 在校验标记和结果、确认无 panic 后受控终止。失败、panic、输出不匹配或制品改变的样本不计入耗时分布；保留有效慢样本。

## 实验数据和分析 {#results}

### 本地 API 新实例等待 {#daemon-warm-pool}

| 请求条件 | N | 创建到 `Running` P50 / P95（ms） | 创建到首条正确命令完成 P50 / P95（ms） |
| --- | ---: | ---: | ---: |
| 串行，默认冷启动 | 41 | 77.89 / 85.90 | 94.54 / 102.53 |
| 串行，预热命中 | 41 | 12.48 / 16.59 | 38.18 / 40.46 |
| 同时两个请求，预热命中 | 41 | 14.04 / 17.67 | 48.20 / 55.74 |
| 同时两个请求，池空回退 | 41 | 99.67 / 110.73 | 117.99 / 127.79 |

串行配对下，命中节省的时间中位数为 65.63 ms（`Running`，95% bootstrap 区间 63.48–66.76 ms）和 56.72 ms（首条命令完成，54.98–58.59 ms）。没有请求在 1 ms 内返回 `Running`。双请求实验共 82 个请求，41 个命中、41 个回退；这是每轮只有一个预留槽位的结果，不是生产命中率，也不能把并发回退时间和串行冷启动之差归因于预热池。

该批次于 2026-11-22 测量，无失败、未剔除有效操作；P95 仅供参考。它衡量本地 daemon，不含 SDK 或远程网络，也**不与下方命令行数据做跨运行时排名**。见[预热池实验复现步骤](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/startup/README.md#daemon-vm-warm-pool)。

### 各配置的 Ready 时间 {#ready}

八个条件于 2026-10-11 测量，3 次预热后各 60 个正式样本。输入为已准备 rootfs、VM 的 2 vCPU / 128 MiB、热缓存和 CPU 0,1 亲和性。Python `perf_counter_ns` 在进程创建前开始，收到完整的 `REFERENCE_READY` 行时停止：包含进程创建、管道读取和观察者调度，不包含解释器启动和输入准备。全部样本还须通过输出、Run Bundle 和输入不变的校验。

|  | N | Ready 中位数 ms | Ready 均值 ms | Ready 标准差 ms | Ready 最小 ms | Ready 最大 ms |
| --- | --- | --- | --- | --- | --- | --- |
| Native | 60 | 0.48 | 0.48 | 0.02 | 0.45 | 0.55 |
| pVisor host | 60 | 33.52 | 50.48 | 29.70 | 30.58 | 128.65 |
| pVisor staged | 60 | 50.59 | 52.77 | 17.87 | 46.18 | 188.29 |
| pVisor VM | 60 | 130.71 | 130.10 | 6.16 | 119.00 | 149.99 |
| Docker | 60 | 105.03 | 105.57 | 7.22 | 90.52 | 126.04 |
| Firecracker | 60 | 287.13 | 287.07 | 5.89 | 269.08 | 301.73 |
| QEMU | 60 | 663.25 | 667.53 | 28.73 | 631.38 | 742.13 |
| QEMU microvm | 60 | 260.10 | 259.53 | 9.57 | 243.28 | 288.39 |

pVisor VM 的 Ready 中位数为 130.71 ms，位于 Docker rootless 的 105.03 ms 与参考 VM 的 260–663 ms 之间。host 与 staged 跳过内核和设备路径，分别为 33.52 ms 和 50.59 ms。

表格按每个 `condition` 取[统计 CSV](../assets/benchmarks/data/startup/startup-timing.csv)中最新的有效行；`condition` 与 `batch` 共同定位结果。标准差采用正式样本的样本标准差。整表复测使用 `mise run benchmark -- startup/timing`；仅复测 VM 使用 `mise run benchmark -- startup/timing "pvisor vm"`。

冷镜像的客户端等待与内容下载见独立的[镜像按需启动](lazy-image-startup.md)实验；镜像服务准备成本单列，样本不与这里的预准备环境合并。

### 来源与复现 {#run}

统计、源码与制品来源和原始记录保存在对应本地批次或文档数据归档。复现见[启动实验手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/startup/README.md)，被测制品见[来源 JSON](../assets/benchmarks/startup-timing.provenance.json)。

[比较方法](index.md#interpretation)
