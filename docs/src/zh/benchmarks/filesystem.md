# 选择执行模式后，文件系统任务要等多久？

## 主要结论 {#conclusions}

**pVisor host 在已测的巡检和编译任务中接近直接运行；需要 VM 隔离时，写回能缩短批量产物写入的等待。**

- **写回的收益取决于任务形态。** 含批量产物写入的解包发布明显缩短；以读取和元数据为主的仓库巡检在两种 VM 配置下耗时接近，未观察到明显收益。
- **同步写入是主要额外成本。** 逐写即同步时写回反而更慢，宜用默认 VM 配置；允许缓冲合并的密集小写，写回明显更短。
- **大工作集写入存在完成性短板。** 640 MiB 工作集的持续写入，本批两种 pVisor VM 配置均超时；其他规模需另测。

结果来自三个独立环境的本机浅测；适用条件见[实验设计](#interpretation)，逐项数字见[实验数据和分析](#results)。

## Motivation：选择执行模式前要回答什么 {#motivation}

Agent 的仓库巡检、搜索、增量编译和产物发布会反复读写文件。为这些任务选择执行模式——host、staged、容器或 VM——本质是在隔离需求和等待之间做权衡。选型前需要回答三个问题：启动要等多久，任务从执行到产物落盘要等多久，以及工作集变大或并发增加后任务能否顺利完成。

## 实验设计 {#interpretation}

### 比较什么 {#comparison-design}

在同一台机器上，对比直接运行（Native）、pVisor host、文件改动暂存模式（staged）、默认 pVisor VM、开启虚拟机写回的 pVisor VM，以及 Docker、Firecracker、QEMU q35 和 QEMU microvm。所有配置使用相同输入、程序和工具，分别衡量各自完整存储路径的成本。容器镜像和虚拟机磁盘均提前准备好。

| 负载 | 测什么 | 回答什么问题 |
| --- | --- | --- |
| 3 个完整任务 | 仓库巡检与搜索；修改、增量编译并运行；离线解包、生成产物并同步发布 | 缓存与写回的收益能否进入实际工作流 |
| 12 个基础操作 | 文件查询、目录遍历、重复读取、不同大小的写入、小文件、逐次同步、读写混合、创建与删除 | 哪些访问方式受益，哪些仍有额外等待 |
| 4 个资源组合 | 128 或 640 MiB 工作集，1 或 4 个并发写入者；每轮固定写入 1 GiB | 工作集变大或并发增加后，任务能否完成、吞吐怎样变化 |

### 怎样测量 {#measurement}

- **资源与缓存：** Linux x86_64、Ryzen 7 9700X、Btrfs；实验进程与私有后台服务共用两核、2 GiB 内存预算且禁用 swap，虚拟机配置 2 vCPU、512 MiB 内存。保留宿主页缓存，后续轮次复用环境状态和校验带来的缓存。容器与虚拟机的缓存和内存限制范围不同。
- **采样：** 每个负载、每种配置使用三个新环境，随机交替执行；每个环境连续运行三轮。三轮合计后，以三个独立环境的中位数和范围报告，不把九轮当成九个独立样本，不合成总分。
- **计时：** Ready 只计执行器启动到共同测试程序就绪，排除任务、退出和清理。任务完成时间包括实际工作和最后的文件、目录同步；准备、额外完整校验和退出分别记录。
- **校验与失败：** 校验逐轮输出、最终文件内容、编译产物和工作区隔离。每环境上限 90 秒；某项配置首次失败后，其余计划样本标为未执行，不补零、不挑掉有效慢值，也不设置外部干扰门禁。不足三个有效环境的单元不报中位数。

这些是三环境浅测，适用于已准备好环境的本机任务。首次镜像获取、冷磁盘、可靠尾延迟、网络代理和更大规模并发需另测。各方案的内核、文件系统与缓存边界不同，结果衡量完整配置；详细存储路径和开关见[复现配置](#configuration-details)。

## 实验数据和分析 {#results}

写回的收益取决于写入能否合并，以及任务要求多频繁地同步。

### 启动与完整任务 {#complete-task}

同批九种配置，每格 **N=3 中位数**。Ready 单位 **ms**，来自固定负载的三个新环境；任务单位 **s**，为每个环境连续三轮完成时间之和，再对三个环境取中位数，排除启动与退出。范围及逐轮数据见折叠明细。

| 模式 | Ready（ms） | 仓库巡检（s） | 增量编译（s） | 解包发布（s） |
| --- | ---: | ---: | ---: | ---: |
| Native | 2.63 | 0.055 | 0.051 | 1.733 |
| pVisor host | 18.31 | 0.054 | 0.051 | 2.038 |
| pVisor staged | 23.07 | 0.525 | 0.090 | 4.703 |
| pVisor VM | 111.88 | 2.155 | 0.543 | 11.271 |
| pVisor VM + writeback | 115.65 | 2.128 | 0.346 | 5.928 |
| Docker | 70.15 | 0.057 | 0.054 | 2.053 |
| Firecracker (stock kernel) | 355.85 | 0.151 | 0.081 | 4.305 |
| QEMU q35 | 690.86 | 0.105 | 0.061 | 4.065 |
| QEMU microvm | 319.05 | 0.143 | 0.062 | 3.851 |

写回能改善包含批量产物写入的任务，仓库巡检中的读取与元数据成本仍然存在。pVisor host 的巡检和编译接近直接运行；进入暂存或虚拟机路径后，启动、读取、同步的额外成本需要按任务分别预算。

<details markdown="1">
<summary>查看完整任务范围与三轮变化</summary>

单位 **s**，每格为“每个环境内三轮完成时间之和”的 N=3 中位数 [最小–最大]；三个工作流各自报告。完成时间为工作加最终同步/发布阶段，排除 Ready 和退出。

| Runtime | 仓库巡检与搜索 | 修改、编译、执行 | 解包与产物发布 |
| --- | --- | --- | --- |
| Native | 0.055 [0.053–0.056] | 0.051 [0.049–0.053] | 1.733 [1.724–1.808] |
| pVisor host | 0.054 [0.053–0.054] | 0.051 [0.050–0.166] | 2.038 [1.963–3.531] |
| pVisor staged | 0.525 [0.524–0.527] | 0.090 [0.088–0.223] | 4.703 [4.623–4.830] |
| pVisor VM | 2.155 [2.040–2.253] | 0.543 [0.504–0.543] | 11.271 [10.954–11.488] |
| pVisor VM + writeback | 2.128 [2.059–2.238] | 0.346 [0.337–0.350] | 5.928 [5.901–6.624] |
| Docker | 0.057 [0.053–0.062] | 0.054 [0.053–0.058] | 2.053 [2.030–2.146] |
| Firecracker (stock kernel) | 0.151 [0.141–0.157] | 0.081 [0.063–0.082] | 4.305 [4.299–4.336] |
| QEMU q35 | 0.105 [0.101–0.105] | 0.061 [0.059–0.062] | 4.065 [2.169–5.487] |
| QEMU microvm | 0.143 [0.128–0.147] | 0.062 [0.061–0.074] | 3.851 [2.703–4.827] |

![三个完整任务的逐轮完成时间](../../../overrides/assets/benchmarks/filesystem-layered-references/workflow-rounds.svg)

散点是三个独立环境，连线是对应轮次的中位数。编译每轮都有真实修改；发布每轮产生新目录，后两轮没有变成空操作。发布与编译包含可合并的小写，仓库巡检主要读取和查询元数据；写回不覆盖全部成本。普通可变工作区也不会因重复运行自动取得封存输入的缓存资格。各任务独立判断，不将发布收益外推到所有工具。

</details>

### 小写合并与同步边界 {#cache-status}

单位 **ms**，每环境三轮完成时间之和的 **N=3 中位数**。小写任务每轮写入 4 MiB，每次 1 KiB；逐次同步任务每轮写入 1 MiB，每次 4 KiB 后立即同步。

| 配置 | 密集小写完成（ms） | 逐次同步完成（ms） |
| --- | ---: | ---: |
| Native | 25.61 | 298.14 |
| pVisor host | 24.77 | 608.67 |
| pVisor staged | 193.37 | 356.77 |
| pVisor VM | 649.99 | 404.73 |
| pVisor VM + writeback | 36.49 | 543.49 |
| Docker | 31.66 | 355.65 |
| Firecracker (stock kernel) | 45.98 | 1015.66 |
| QEMU q35 | 24.17 | 1043.30 |
| QEMU microvm | 24.62 | 1160.67 |

允许缓冲合并时，写回模式的密集小写（36.49 ms）与 Native（25.61 ms）、Docker（31.66 ms）处于相近水位，默认 VM 则明显更慢；每次写后立即同步限制了合并空间，开启写回后反而更慢，而默认 VM 在这项负载上短于同批 Firecracker 与 QEMU 参照。评估持续写入应计入最终同步，不能只看写入调用返回。

<details markdown="1">
<summary>请求粒度、同步边界与全部机制明细</summary>

单位 **ms**，三轮完成时间之和的 N=3 中位数。顺序写每轮 4 MiB；逐写同步每轮写 1 MiB、每 4 KiB fsync；命名空间任务每轮 128 个文件，保留内容读回与删除后的目录同步。

| Runtime | 4 MiB / 1 KiB 写 | 4 MiB / 1 MiB 写 | 逐 4 KiB fsync | 创建、同步、重命名、删除 |
| --- | --- | --- | --- | --- |
| Native | 25.61 | 18.60 | 298.14 | 198.68 |
| pVisor host | 24.77 | 17.35 | 608.67 | 212.09 |
| pVisor staged | 193.37 | 20.63 | 356.77 | 335.82 |
| pVisor VM | 649.99 | 29.89 | 404.73 | 682.79 |
| pVisor VM + writeback | 36.49 | 36.82 | 543.49 | 696.58 |
| Docker | 31.66 | 21.72 | 355.65 | 165.90 |
| Firecracker (stock kernel) | 45.98 | 29.67 | 1015.66 | 564.97 |
| QEMU q35 | 24.17 | 21.24 | 1043.30 | 547.26 |
| QEMU microvm | 24.62 | 22.26 | 1160.67 | 585.23 |

![请求粒度与工作、同步完成吞吐](../../../overrides/assets/benchmarks/filesystem-layered-references/write-request-size.svg)

1 MiB 粒度每轮只有四次应用写入，与 1 KiB 的 4,096 次写入具有不同的合并空间；逐写 fsync 又限制了延迟空间。两种请求粒度只构成两个实测锚点，连线不代表中间粒度已测。左图为工作阶段吞吐，右图计入最终同步；逐写 fsync 等工作负载的工作阶段本身也包含同步。只报告写入返回会漏掉排空成本。

![全部机制的工作与同步完成耗时比](../../../overrides/assets/benchmarks/filesystem-layered-references/mechanism-relative-time.svg)

横轴为同批中位数之比，Native = 1，越小越短；各机制独立，未计算总分。N=3 的描述性水位不能充当确认性加速比或稳定尾延迟。混合任务分别记录前台读与整体完成，防止用后台吞吐改善掩盖响应变慢。下表为首轮 N=3 中位数；后台写和前台读使用不同文件。

| Runtime | 首轮前台读 ms | 首轮混合任务完成 ms |
| --- | --- | --- |
| Native | 3.83 | 13.23 |
| pVisor host | 4.01 | 15.69 |
| pVisor staged | 5.86 | 30.47 |
| pVisor VM | 13.21 | 70.84 |
| pVisor VM + writeback | 11.41 | 24.33 |
| Docker | 4.55 | 17.61 |
| Firecracker (stock kernel) | 11.51 | 22.32 |
| QEMU q35 | 8.51 | 24.17 |
| QEMU microvm | 8.31 | 13.14 |

</details>

### 工作集与并发边界 {#resource-boundary}

每轮写入 **1 GiB**，只改变工作集 **128/640 MiB** 和并发写入者 **1/4**。128 MiB 下九种配置均完成，三轮完成时间之和的 N=3 中位数如下（单位 **s**，每轮四个文件、合计写入 1 GiB）：

| 配置 | 128 MiB、1 写入者（s） | 128 MiB、4 写入者（s） |
| --- | ---: | ---: |
| Native | 2.676 | 1.571 |
| pVisor host | 2.722 | 1.494 |
| pVisor staged | 13.182 | 9.247 |
| pVisor VM | 41.545 | 15.751 |
| pVisor VM + writeback | 4.195 | 2.073 |
| Docker | 2.822 | 1.550 |
| Firecracker (stock kernel) | 3.006 | 1.825 |
| QEMU q35 | 2.566 | 1.685 |
| QEMU microvm | 2.605 | 2.414 |

128 MiB 下，写回 VM 与容器及参考 VM 处于同一量级，默认 VM 的持续同步写入成本明显更高；四个写入者分摊每轮固定的 1 GiB 写入，合计完成时间相应更短。640 MiB 工作集下，默认与开启写回的 pVisor VM 各有两个单元在 90 秒内未完成；这四个单元均没有完整 N=3 结果。超时不计作零吞吐，也不能换算成正常耗时。

<details markdown="1">
<summary>查看工作集、并发曲线与失败明细</summary>

![四个资源锚点的完成吞吐与最终同步占比](../../../overrides/assets/benchmarks/filesystem-layered-references/resource-workers.svg)

每轮固定 1 GiB 逻辑写入、四文件、4 KiB 请求；只改变工作集 128/640 MiB 与 worker 数 1/4。吞吐使用完成时间，散点表示独立环境；缺失点表示该单元没有完整 N=3 结果，不表示零吞吐。工作集变化也改变保留内容量与最终同步成本，四点不足以定位容量拐点。

| 负载 | 配置 | 已执行失败 / 后续未执行 | 结果 |
| --- | --- | --- | --- |
| 640 MiB 工作集、1 worker | pVisor VM | 1 / 2 | 90 秒超时；无耗时中位数 |
| 640 MiB 工作集、1 worker | pVisor VM + writeback | 1 / 2 | 90 秒超时；无耗时中位数 |
| 640 MiB 工作集、4 worker | pVisor VM | 1 / 2 | 90 秒超时；无耗时中位数 |
| 640 MiB 工作集、4 worker | pVisor VM + writeback | 1 / 2 | 90 秒超时；无耗时中位数 |

超时只能说明这些配置没有在协议上限内完成任务，不能换算成正常耗时或精确吞吐，也不能仅凭现象认定根因。对持续大批量写入的判断，应先解决这些完成性问题，再扩大扫描。

</details>

### 启动等待的取值范围 {#quick-results}

共同静态 payload 的 Ready 中位数：pVisor VM 111.88 ms、Firecracker 355.85 ms、QEMU q35 690.86 ms、QEMU microvm 319.05 ms。启动等待与后续任务耗时分别判断。

<details markdown="1">
<summary>查看九种配置的 Ready 与范围</summary>

单位 **ms**，固定元数据负载启动前的共同静态 payload Ready，每配置三个新环境；未把所有负载的启动混为更多独立样本。中位数与范围排除 namespace 准备、任务执行、退出和收尾；这不是旧版 Python Ready 或完整发行版启动。

| Runtime | Ready ms | Min–max ms |
| --- | --- | --- |
| Native | 2.63 | 0.81–2.64 |
| pVisor host | 18.31 | 16.57–19.51 |
| pVisor staged | 23.07 | 22.34–28.14 |
| pVisor VM | 111.88 | 111.37–112.75 |
| pVisor VM + writeback | 115.65 | 115.32–138.86 |
| Docker | 70.15 | 68.53–75.74 |
| Firecracker (stock kernel) | 355.85 | 354.80–388.50 |
| QEMU q35 | 690.86 | 685.43–707.84 |
| QEMU microvm | 319.05 | 299.70–327.80 |

</details>

### 复现配置与数据覆盖 {#configuration-details}

<details markdown="1">
<summary>查看存储路径、计时细节与有效样本数</summary>

<!-- measurement-status:start -->

计划 19 项负载 × 9 种配置 × 3 个新环境，共 513 个。实际执行 505 个，501 个通过，4 个失败；另有 8 个按既定规则未执行。167 / 171 个负载配置单元具有完整 N=3 结果。每个通过的环境内部执行三轮，共 1503 条有效轮记录；三轮相关，不作为独立样本。

<!-- measurement-status:end -->

对照 Native、pVisor host、staged、默认 CLI VM、显式开启 guest writeback 的 CLI VM、Docker、Firecracker、QEMU q35 和 QEMU microvm。九种配置在同一批次随机交替，使用相同字节的静态负载、离线输入与 Python/git/C 工具链，不拼接此前五模式数据。输入只读、输出写入各模式的工作区；普通 CLI 使用 Mutable 所有权，不宣称启用了 Immutable lower 缓存，两种 pVisor VM 的 host writeback 关闭、产品路径关闭 DAX，仅 guest writeback 显式设为 0/1。

| 配置 | 工作区存储路径 |
| --- | --- |
| Native / pVisor host | 宿主 Btrfs 目录 |
| pVisor staged | FUSE 暂存文件系统 |
| pVisor CLI VM，两种写回配置 | virtio-fs、OverlayFS 与暂存输出 |
| Docker | 预准备私有 rootless overlay2；输出在容器可写层，输入只读 bind |
| Firecracker / QEMU q35 / QEMU microvm | 私有 ext4 root/work 镜像、virtio-blk；相同官方 stock 内核、模块 initrd 与最小 PID1 |

参考 VM 仅启动共同负载所需的最小 userspace，不启动发行版服务。Firecracker 显式 Writeback、QEMU 显式 cache=writeback，保留 FLUSH；正常卸载和 VMM 退出后只读导出输出，交给共同 host oracle。Docker 在退出后复制容器输出。磁盘准备、容器创建与输出导出不计入 Ready 或任务；Ready 从执行器启动前的共同标记至 payload Ready，包含参考 VM 的内核启动。内核、存储栈与缓存边界的差异属于所测配置，不能解释为纯 VMM 开销。

| 层次 | 固定负载 | 用途 |
| --- | --- | --- |
| 12 个机制负载 | 正项/缺项查询、目录枚举、重开顺序读、固定乱序读、1 KiB/1 MiB 连续写、4 KiB 乱序覆盖、256 个小文件、逐写 fsync、前台读与后台写、创建—同步—重命名—删除 | 区分请求粒度、复用、同步与元数据成本 |
| 4 个资源负载 | worker 数为 1 或 4，工作集为 128 或 640 MiB；固定四文件、4 KiB 写、每轮合计 1 GiB 逻辑写入 | 比较并发和工作集变化，不预设已经达到稳态或容量拐点 |
| 3 个完整任务 | 仓库巡检与搜索；修改—增量编译—执行校验；离线解包—小文件及 32 MiB 产物生成—同步发布 | 分别衡量任务完成时间，不合成通用总分 |

Linux x86_64 / Ryzen 7 9700X / Btrfs，宿主页缓存不清理。仓库与离线包各含 1,024 个 4 KiB 文件；发布另生成 128 个 4 KiB 小文件和 32 MiB 流写产物。整个串行 campaign、私有 Docker daemon/container 共用两核 quota、2 GiB 内存及零 swap 父组；所有 VM 配置 2 vCPU/512 MiB，实际 guest MemTotal 另记在来源摘要。宿主未向 user cgroup 委派 cpuset，daemon 和观察者继承 CPU 0、1 affinity，Docker payload 单独设置并记录实际 affinity；不宣称 early runc 阶段也有硬 cpuset 约束。

Native/host/容器使用父组内存，VM 另受 guest 内存边界限制；不能声称可用缓存等量。资源负载以固定 512 MiB 为尺度，128/640 MiB 不代表原生或容器的实际内存比例；更大工作集也增加最终保留和同步的数据量。

每个“负载 × 配置”计划三个独立新环境，配置顺序随机交替；每环境连续三轮，保留状态与校验预热的页，每格九条轮记录只有三个独立环境，不跨负载互相预热。Ready 从执行器前的共同启动标记计至负载 Ready，排除 namespace 准备、任务和退出。任务完成为工作时间加最终同步，保留周期性 fsync、文件/目录同步及检查后的 close；准备、额外全量验证和退出另记；工作流自带的内容验收和程序运行检查保留在工作时间内。增量编译每轮真实修改一个编译单元；初始 100 个单元的完整编译属于计时外准备。

每次执行器进程运行限 90 秒。正常退出、逐轮结果、命名空间和暂存隔离均须通过；确定性数据逐字节核对，编译产物在任务内执行校验，退出后完整读回并检查 ELF 与文件清单。首个失败后，该负载与配置余下的计划样本明确跳过，不补跑、不记为零；不足三个有效环境的格子不报中位数。保留有效慢值与失败，不设安静期、外部干扰或全量清单门禁。N=3 仅作探索，不支持稳定尾延迟、冷磁盘或跨机器的普遍排名。

</details>

### 历史数据 {#reference-fs}

<details markdown="1">
<summary>查看独立保留的历史口径</summary>

原七工具的 N=5 Ready/驻留环境统计与来源、历史 N=30 统计，以及此前五模式分层对照，各自保留原始口径；不填入新的 19 项表格，也不计算跨批次优化幅度。

</details>

### 来源与复现 {#run}

[工作流图 SVG](../../../overrides/assets/benchmarks/filesystem-layered-references/workflow-rounds.svg) · [请求粒度图 SVG](../../../overrides/assets/benchmarks/filesystem-layered-references/write-request-size.svg) · [资源图 SVG](../../../overrides/assets/benchmarks/filesystem-layered-references/resource-workers.svg) · [机制图 SVG](../../../overrides/assets/benchmarks/filesystem-layered-references/mechanism-relative-time.svg)

[全部分层统计](../../../overrides/assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-levels.csv) · [Ready](../../../overrides/assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-ready.csv) · [失败与缺失](../../../overrides/assets/benchmarks/data/filesystem/layered-references/filesystem-layered-references-failures.csv) · [来源摘要](../../../overrides/assets/benchmarks/filesystem-layered-references/provenance.json)

本地完整统计中的 write_ops/read_ops 是负载显式逻辑操作数，不是 guest→host 请求数，也不包含子工具内部全部 I/O。

2026-10-17，独立用户配置批次；完整 CSV/JSON、原始记录、冻结入口、工具及固件收据保存在本地 `benchmark.new/filesystem/.data/user-layered-references-v2-20261017/`。[复现步骤](https://github.com/DeepLink-org/pvisor/blob/main/benchmark.new/filesystem/README.md#user-workloads)。[缓存与异步写工程分析](../design/filesystem-io-performance-analysis.md)独立解释实现开关与请求计数，不用于补齐用户对照。
