# 文件密集任务要等多久？

## Motivation {#motivation}

pVisor host 在所测仓库巡检和增量编译中接近直接执行；需要 VM 隔离且写入能在同步前合并时，guest writeback 可以缩短发布等待。VM 的文件访问仍有额外成本，启动更快不保证任务更快，大工作集也可能超时。

三轮解包发布的中位数为 Native 1.545 s、pVisor 默认 VM 11.675 s、writeback VM 6.171 s；默认 VM 的 Ready 为 101.75 ms。完整任务、启动与资源失败分别报告。

## 实验设计 {#interpretation}

测量仓库检查、增量编译和同步发布的工作加最终同步时间，称为任务完成耗时。它代表所选执行模式处理文件任务的等待。耗时越短，同一预算内完成任务越快。

同机九种模式共用静态负载、确定性只读输入和 Python/git/C 工具链，镜像和磁盘预先准备。Linux x86_64 / Ryzen 7 9700X / Btrfs；整个进程树与私有 Docker 共享两核 quota、2 GiB、零 swap，CPU affinity 0,1。VM 为 2 vCPU/512 MiB；Native、host、容器的父组限额与 guest RAM 不等价。工具来自固定的 Fedora 45 环境，stock 参考内核为 7.2.11-300.fc45，pVisor 使用静态 release 及匹配的内嵌固件。

12 个机制负载、4 个资源负载、3 个完整工作流，每个负载/模式三个新环境，顺序随机，每环境三轮相关执行。主指标先把三轮完成时间相加，再对三个环境取中位数；宿主页缓存不清理，后续轮次继承验证预热。Ready 从共同的执行器启动标记到 payload Ready，排除 namespace 准备、任务、退出和清理；不能与任务时钟混为一项。

每个被测执行器进程限时 90 秒，必须正常退出、逐轮校验并通过独立 host 最终内容核对；暂存模式还须保持原工作区未变。某模式/负载首次失败后，余下计划样本记为未执行；不足三个有效环境不报中位数，不补零，不剔除有效慢样本。N=3 为探索性数据，不支持稳定尾延迟、冷磁盘或跨机器排名。

## 实验数据和分析 {#results}

### 启动与完整任务 {#complete-task}

| 模式 | Ready ms | 仓库巡检 s | 增量编译 s | 解包发布 s |
| --- | --- | --- | --- | --- |
| Native | 0.56 | 0.051 | 0.045 | 1.545 |
| pVisor host | 17.64 | 0.050 | 0.045 | 1.549 |
| pVisor staged | 22.79 | 0.454 | 0.083 | 4.556 |
| pVisor VM | 101.75 | 2.376 | 0.557 | 11.675 |
| pVisor VM + writeback | 103.45 | 2.289 | 0.353 | 6.171 |
| Docker | 52.60 | 0.052 | 0.048 | 1.637 |
| Firecracker (stock kernel) | 302.11 | 0.137 | 0.061 | 5.074 |
| QEMU q35 | 636.23 | 0.099 | 0.058 | 3.170 |
| QEMU microvm | 247.87 | 0.133 | 0.055 | 3.433 |

Ready 单位 ms；任务单位 s，均为三个新环境的中位数。任务列先在每个环境内累计三轮，排除初始准备和额外验收；编译每轮真实修改一个编译单元，发布每轮生成新目录。首次镜像拉取不在此范围。

host 的巡检和编译接近 Native，暂存与 VM 的文件访问增加等待。pVisor VM 就绪早于同批 stock 参考 VM，但这不抵消巡检和发布的访问成本；writeback 缩短发布任务，仍需按完整任务判断是否适合。

<details markdown="1">
<summary>查看完整任务范围与三轮变化</summary>

N=3 中位数 [最小–最大]，单位 s；未将三轮当成九个独立样本。

| 模式 | 仓库巡检 s | 增量编译 s | 解包发布 s |
| --- | --- | --- | --- |
| Native | 0.051 [0.051–0.056] | 0.045 [0.045–0.046] | 1.545 [1.540–1.619] |
| pVisor host | 0.050 [0.049–0.051] | 0.045 [0.045–0.046] | 1.549 [1.527–1.578] |
| pVisor staged | 0.454 [0.449–0.454] | 0.083 [0.083–0.085] | 4.556 [4.528–4.593] |
| pVisor VM | 2.376 [2.109–2.392] | 0.557 [0.556–0.576] | 11.675 [11.218–11.910] |
| pVisor VM + writeback | 2.289 [2.280–2.371] | 0.353 [0.349–0.366] | 6.171 [6.047–6.517] |
| Docker | 0.052 [0.051–0.057] | 0.048 [0.047–0.052] | 1.637 [1.561–1.644] |
| Firecracker (stock kernel) | 0.137 [0.136–0.154] | 0.061 [0.061–0.073] | 5.074 [5.005–5.081] |
| QEMU q35 | 0.099 [0.095–0.101] | 0.058 [0.055–0.059] | 3.170 [3.160–3.288] |
| QEMU microvm | 0.133 [0.132–0.143] | 0.055 [0.055–0.056] | 3.433 [3.350–4.858] |

![三个完整任务的逐轮完成时间](../assets/benchmarks/filesystem-layered-references/workflow-rounds.svg)

散点为独立环境，连线为对应轮次中位数。各任务分别判断，不合成总分；普通可变工作区不会因重复访问自动取得 Immutable lower 缓存资格。

</details>

### 小写合并与同步边界 {#cache-status}

| 模式 | 1 KiB 写 ms | 1 MiB 写 ms | 逐 4 KiB 同步 ms |
| --- | --- | --- | --- |
| Native | 18.904 | 11.367 | 261.038 |
| pVisor host | 17.623 | 10.722 | 254.600 |
| pVisor staged | 193.411 | 13.740 | 280.462 |
| pVisor VM | 649.800 | 24.794 | 303.493 |
| pVisor VM + writeback | 31.084 | 31.827 | 396.031 |
| Docker | 20.274 | 12.177 | 250.648 |
| Firecracker (stock kernel) | 30.513 | 22.107 | 831.941 |
| QEMU q35 | 17.759 | 14.857 | 882.360 |
| QEMU microvm | 17.777 | 14.844 | 909.498 |

单位 ms，三轮完成时间之和的 N=3 中位数。连续写每轮 4 MiB；逐写同步每轮 1 MiB、每 4 KiB fsync。延迟合并的空间由同步语义决定，不能只用 write 返回时间评估完成成本。

<details markdown="1">
<summary>请求粒度、同步边界与全部机制明细</summary>

| 操作 ms | Native | pVisor host | pVisor staged | pVisor VM | pVisor VM + writeback | Docker | Firecracker (stock kernel) | QEMU q35 | QEMU microvm |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 正项元数据 | 4.545 | 4.588 | 32.248 | 105.383 | 139.245 | 5.397 | 14.812 | 8.118 | 8.381 |
| 缺项元数据 | 2.498 | 2.403 | 77.970 | 432.963 | 432.771 | 2.988 | 15.931 | 15.233 | 15.304 |
| 目录枚举 | 2.466 | 2.505 | 9.566 | 16.214 | 16.132 | 2.749 | 4.623 | 3.289 | 3.950 |
| 重开顺序读 | 28.535 | 27.980 | 37.146 | 40.205 | 39.982 | 28.597 | 37.083 | 33.021 | 31.538 |
| 固定乱序读 | 10.357 | 10.240 | 27.742 | 137.974 | 163.752 | 10.479 | 139.761 | 100.418 | 154.622 |
| 1 KiB 写 | 18.904 | 17.623 | 193.411 | 649.800 | 31.084 | 20.274 | 30.513 | 17.759 | 17.777 |
| 1 MiB 写 | 11.367 | 10.722 | 13.740 | 24.794 | 31.827 | 12.177 | 22.107 | 14.857 | 14.844 |
| 4 KiB 乱序覆盖 | 11.668 | 11.893 | 55.527 | 150.683 | 26.931 | 12.081 | 38.049 | 14.445 | 16.400 |
| 小文件 | 280.802 | 277.178 | 380.104 | 820.276 | 691.822 | 276.624 | 391.725 | 357.702 | 403.307 |
| 逐写 fsync | 261.038 | 254.600 | 280.462 | 303.493 | 396.031 | 250.648 | 831.941 | 882.360 | 909.498 |
| 混合读写 | 16.069 | 16.855 | 64.701 | 174.719 | 37.030 | 22.251 | 34.416 | 23.280 | 22.606 |
| 创建与删除 | 145.796 | 146.854 | 266.569 | 668.033 | 626.365 | 141.702 | 442.769 | 449.161 | 472.677 |

单位 ms，同样累计三轮并包含最终同步。逻辑操作数不等于 guest→host 请求数。

![请求粒度与工作、同步完成吞吐](../assets/benchmarks/filesystem-layered-references/write-request-size.svg)

两个粒度是实测锚点，连线不表示中间粒度已测。

![全部机制的工作与同步完成耗时比](../assets/benchmarks/filesystem-layered-references/mechanism-relative-time.svg)

同批 Native 中位数为 1，越小越短；这是描述性水位，不是确认性加速比。

| 模式 | 首轮前台读 ms | 首轮整体完成 ms |
| --- | --- | --- |
| Native | 3.365 | 5.706 |
| pVisor host | 4.986 | 7.904 |
| pVisor staged | 5.549 | 22.133 |
| pVisor VM | 13.722 | 70.610 |
| pVisor VM + writeback | 12.131 | 21.692 |
| Docker | 3.410 | 7.124 |
| Firecracker (stock kernel) | 14.161 | 17.317 |
| QEMU q35 | 8.167 | 11.158 |
| QEMU microvm | 7.893 | 11.203 |

前台读与后台写使用不同文件，单列首轮读等待，避免整体吞吐掩盖前台响应。

</details>

### 工作集与并发边界 {#resource-boundary}

| 模式 | 128 MiB × 1 s | 128 MiB × 4 s | 640 MiB × 1 s | 640 MiB × 4 s |
| --- | --- | --- | --- | --- |
| Native | 2.446 | 1.362 | 2.672 | 2.529 |
| pVisor host | 2.431 | 1.337 | 3.365 | 1.576 |
| pVisor staged | 13.149 | 9.064 | 14.070 | 9.295 |
| pVisor VM | 43.460 | 16.411 | — | — |
| pVisor VM + writeback | 4.412 | 1.816 | — | — |
| Docker | 2.527 | 1.424 | 3.150 | 2.352 |
| Firecracker (stock kernel) | 2.802 | 1.615 | 5.348 | 2.577 |
| QEMU q35 | 2.284 | 1.400 | 3.875 | 1.866 |
| QEMU microvm | 2.464 | 1.386 | 3.900 | 2.010 |

单位 s，三轮完成时间之和的 N=3 中位数；每轮逻辑写入 1 GiB、固定四文件，只改变工作集与 worker 数。`—` 表示未取得完整 N=3，不能解释为零耗时。工作集扩大也增加最终保留与同步的字节，四个锚点不证明容量拐点。

<details markdown="1">
<summary>查看工作集、并发曲线与失败明细</summary>

![工作集和 worker 数下的完成吞吐与失败](../assets/benchmarks/filesystem-layered-references/resource-workers.svg)

| 模式 | 负载 | 环境 | 结果 |
| --- | --- | --- | --- |
| pVisor VM + writeback | 640 MiB × 1 | 1 | 超时 |
| pVisor VM | 640 MiB × 1 | 1 | 超时 |
| pVisor VM | 640 MiB × 4 | 1 | 超时 |
| pVisor VM + writeback | 640 MiB × 4 | 1 | 超时 |

失败现场和超时保留在原始批次；不以成功轮次补齐失败环境。

</details>

### 启动等待的取值范围 {#quick-results}

<details markdown="1">
<summary>查看九种配置的 Ready 与范围</summary>

| 模式 | Ready ms [min–max] |
| --- | --- |
| Native | 0.56 [0.56–0.61] |
| pVisor host | 17.64 [17.34–18.25] |
| pVisor staged | 22.79 [22.79–22.87] |
| pVisor VM | 101.75 [100.73–104.92] |
| pVisor VM + writeback | 103.45 [101.50–106.76] |
| Docker | 52.60 [49.85–57.38] |
| Firecracker (stock kernel) | 302.11 [296.11–325.60] |
| QEMU q35 | 636.23 [634.79–655.04] |
| QEMU microvm | 247.87 [240.66–284.25] |

固定正项元数据负载的 N=3，中位数 [最小–最大]；未把各负载 Ready 混为更大的 N。这是共同静态程序就绪，参考 VM 包含内核启动，不是完整发行版服务启动。

</details>

### 复现配置与数据覆盖 {#configuration-details}

<!-- measurement-status:start -->

计划 19 项负载 × 9 种配置 × 3 个新环境，共 513 个。实际执行 505 个，501 个通过，4 个失败；另有 8 个按既定规则未执行。167 / 171 个单元具有完整 N=3 结果，1503 条有效轮记录。

<!-- measurement-status:end -->

<details markdown="1">
<summary>查看存储路径、计时细节与有效样本数</summary>

此对照使用共同的目录 rootfs，不经过 lazy cache；压缩镜像的文件访问成本见[镜像实验](lazy-image-startup.md#local-access)。

Native/host 输出在 Btrfs；staged 使用 FUSE 暂存；两种 pVisor VM 使用 virtio-fs、OverlayFS 与暂存输出。普通 CLI Job 入口为 `spawn`，host writeback 关闭，产品关闭 DAX，guest writeback 分别请求关闭/开启。Docker 使用新私有 rootless overlay2，可写层承载输出、输入只读 bind，退出后复制输出供独立校验。

Firecracker、QEMU q35、QEMU microvm 共用未修改的官方 stock 内核、匹配模块 initrd、最小 PID1 和私有 ext4 root/work 磁盘，virtio-blk、FLUSH 与正常卸载保持启用。QEMU microvm 开启 ACPI 进行设备发现。磁盘准备、容器创建和输出导出均不计入 Ready/任务。工具、内核和存储栈差异属于配置，不解释为纯 VMM 开销。

仓库与离线包各 1,024 个 4 KiB 文件，增量编译有 100 个翻译单元，发布另生成 128 个 4 KiB 文件和 32 MiB 流写产物。资源负载的 128/640 MiB 按固定 512 MiB 参考尺度定义，不是 Native/容器实际内存比例。宿主 user cgroup 未委派 cpuset，报告继承 affinity，不宣称早期 runc 有硬 cpuset 保证。完整文件及目录同步、内容校验和暂存隔离均未减少。

</details>

### 历史数据 {#reference-fs}

此前七工具、五模式与九模式结果各自保留原始口径，不与当前样本拼接，不由跨批次差异计算优化幅度。

### 来源与复现 {#run}

[全部分层统计](../assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-levels.csv) · [Ready](../assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-ready.csv) · [失败与缺失](../assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-failures.csv) · [来源摘要](../assets/benchmarks/filesystem-layered-references/provenance.json)

2026-10-25，源码 `9e1b8f15`，基于 main `92a4ce4e`。完整原始记录、冻结工具、二进制与固件、预检失败和派生图表保留在本地 `benchmark/filesystem/.data/user-rebase-20261025-c/`；此前公开材料也已归档。[复现步骤](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/filesystem/README.md#user-workloads) · [缓存与异步写工程分析](../lab-notes/filesystem-io-performance-analysis.md)。工程开关归因不用于补齐用户数据。
