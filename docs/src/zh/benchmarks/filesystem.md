# 文件系统与开发工具开销

同机 Docker 对照显示：64 MiB 读取为原生/Docker **33 ms**、pVisor staged **49 ms**；遍历 2,048 文件为原生/Docker **5 ms**、staged **180 ms**。单次读取增加十几毫秒，小文件密集路径仍是短板；VM 离线 npm 约 **1.73 秒**，Docker 约 **0.23 秒**。

## Motivation

Agent 经常反复读目录、搜索和修改文件。应同时看到任务本身和启动、视图准备、记录所需的总时间，才能决定是否值得开启暂存或 VM。

## 实验设计 {#interpretation}

同一输入比较 native、host、staged、safe、libkrun VM、rootless Podman/crun 与 pVisor OCI。主批次每格 3 次预热、30 次测量，热宿主缓存；准备镜像和复制输入不计时。worker 包含工具运行与输出校验；wall 包含启动到退出。metadata/git/rg 使用 2,048 文件、32 目录；read 校验 64 MiB SHA256；write 写 256×60 KiB；cargo 编译 64 个无外部依赖的小模块并验证结果 2016；npm 离线安装 32 个本地包，不访问 registry。

## macOS

本轮在 Linux 实测，以下工作负载没有 macOS 样本；macFUSE/FSKit 开销与并发容量均未测。既有 macOS/HVF 数据继续保留在 [VM 启动时间](startup.md)与[VM 内存报告](vm-memory/index.md)，不移作本页结果。

## Linux：2026-10-05，真实 VM/FUSE A/B 基线 {#e2e-baseline}

这次启动真实 KVM VM，并挂载真实宿主 FUSE，补齐上轮适配层微基准的
端到端验证。宿主设备可用，最初沙箱没有暴露 `/dev/kvm` 和 `/dev/fuse`。
两份 GNU/Linux release 制品来自同一份 `a1020d4b` 源码归档，只有
OverlayCore 的 `core.rs` 不同；二者都加入相同的 guest stdio 端口就绪修复。
可执行文件、firmware、源码补丁和当轮脚本均保存摘要。

每轮随机交替 native、优化前/后的 FUSE staged 与优化前/后的 VM，共五格；
每格 3 次预热、30 次测量。共 150 个测量任务、1,050 个工具测量，所有
正确性检查通过。每个任务使用新的 workspace/upper，在同一个环境中按顺序
执行七种负载；输入复制不计时。宿主缓存为热缓存，CPU 亲和性固定在两个
物理核 `0,1`，VM 为 2 vCPU、16 GiB。保留的历史工具环境 fixture 写入
256×64 KiB，共 16 MiB；这与当前 product-v1 的 60 KiB 文件不同，不合并
历史百分位数。宿主有并行任务，1 分钟 load average 从 10.38 降至 3.06；
固定亲和性是共同预算，不是独占 CPU。以下是单批筛查数据。

下表是工具内部操作与校验的 P50，单位 ms；负百分比表示耗时下降。

| 负载 | Native | FUSE 前→后 | 变化 | VM 前→后 | 变化 |
|---|---:|---:|---:|---:|---:|
| metadata | 4.93 | 92.38 → 78.25 | -15.3% | 224.36 → 195.24 | -13.0% |
| read | 32.56 | 68.30 → 67.82 | -0.7% | 116.98 → 117.90 | +0.8% |
| write | 3.90 | 201.36 → 198.92 | -1.2% | 243.10 → 257.16 | +5.8% |
| git | 15.11 | 190.36 → 170.46 | -10.5% | 484.14 → 494.86 | +2.2% |
| rg | 7.66 | 100.80 → 90.58 | -10.1% | 476.92 → 464.97 | -2.5% |
| cargo | 52.92 | 112.30 → 110.11 | -1.9% | 513.46 → 512.74 | -0.1% |
| npm | 173.19 | 221.67 → 217.47 | -1.9% | 1570.05 → 1503.67 | -4.2% |

七项负载合计的启动到退出 P50：native **454.50 ms**，FUSE staged
**1288.69 → 1230.93 ms（-4.5%）**，VM **4829.04 → 4777.55 ms（-1.1%）**。
元数据收益已在真实路径复现，但不能宣称 VM 的 git、写入或整体任务都有
改善；VM write 和 git 本批反而变慢。小幅变化与写入变化需要在较空闲宿主
上复测，不能仅由一个 P50 判定收益或回归原因。

一次先行预热曾出现 VM 退出码为 0、任务写入完成，但 stdout 没有任何
工作负载标记。基准立即停止，失败报告保留。普通 VM runner 现在声明实际
非终端 stdio 所需的 named ports；guest 在启动工具前等待名称就绪，最多
5 秒，缺失则失败。新增测试覆盖迟到的端口名、稍后新增的端口和缺失端口，
不加入固定启动等待。正式批次的 68 个 VM 运行（含预检与预热）都通过输出
检查。基准还验证实际隔离类型、lower 没有写入、upper 含完整 256 个文件；
制品摘要相同、清单不符、缺样本或中断都不能成为成功的 A/B 汇总。

另开的一次 profiling 不计入上表。候选 VM 两个非空 Core 实例的最后累计
检查点仍分别有 **126,034 / 168,027** 次物理父目录 metadata 调用、
**57,547 / 53,161** 次 mount identity 查询尝试。两个 dispatch 检查点为
**20,545 inline / 424 pool** 和 **24,398 inline / 100 pool**，各有 2 个
worker。宿主候选的完整 Core profile 有 341 次 journal fsync，累计约
110 ms。Core 的重复物理路径检查、首次观察/写入的 journal 成本，以及
小请求仍走 inline 的调度路径应优先分别验证；单纯增加 worker 或更换
virtiofsd 不能据此认定会消除这些开销。VM profile 缺少 final record，计数
只覆盖检查点之前的工作；嵌套 inclusive span 不相加，admission 不等于排队
等待时间。

验证：guest 5 项、VM 全包 292 项（2 项跳过）、不含 control 的 executor
24 项和基准 24 项测试通过，guest Clippy 与基准 Ruff 通过。当前工作区
较宽 executor 子集另有 9 项 VM control 测试失败，保留在验证归档中，本次
没有修改 control 实现，也不将这些检查算作通过。

从同一源码构建两份 GNU/Linux 制品，仅在两次构建之间应用待测修改；
输出目录必须是新目录。先以 `--samples 1 --warmups 0` 预检。

```bash
python3 benchmark/pvisor/filesystem_ab.py \
  --assets target/reference-env-final-20261004 \
  --baseline /absolute/path/to/pvisor-before \
  --candidate /absolute/path/to/pvisor-after \
  --firmware /absolute/path/to/libkrunfw-directory \
  --output target/filesystem-ab-new \
  --cpu-affinity 0,1 --samples 30 --warmups 3
```

[逐样本 CSV](../../assets/benchmarks/filesystem-ab-20261005/samples.csv) ·
[协议、制品与 P50/P95/P99](../../assets/benchmarks/filesystem-ab-20261005/summary.json) ·
[独立诊断](../../assets/benchmarks/filesystem-ab-20261005/profiles.json) ·
[报告、脚本、失败样本与验证日志](../../assets/benchmarks/filesystem-ab-20261005/evidence.tar.gz)

## Linux：2026-10-05，OverlayCore 路径解析优化 {#resolution-optimization}

本次只测 release 模式的 virtio-fs OverlayFs 适配层，不启动 VM、不挂载
宿主 FUSE，也不记录 preimage journal。输入位于 `/tmp` tmpfs：32 个目录、
每目录 64 个 18 B 文件；深目录案例的父目录深度为 8。每轮使用新的 inode
表，宿主缓存为热缓存，创建输入和适配器不计时。

保存优化前后的测试可执行制品，用 nextest 的 binaries metadata 交替运行
三个批次，顺序为旧→新、新→旧、旧→新。每格每批次 2 次预热、8 次无
profiling 测量，另有 1 次诊断；表中 P50 只来自每版本的 24 次无 profiling
样本。CPU 未固定亲和性，二进制摘要和各批次中位数保存在原始汇总中。

| 适配层操作，2,048 文件 | 优化前 P50 ms | 优化后 P50 ms | 耗时下降 |
|---|---:|---:|---:|
| lookup + getattr | 35.15 | 26.35 | 25.0% |
| lookup + open + getattr + release | 37.61 | 28.07 | 25.4% |
| 深目录 lookup + open + getattr + release | 170.51 | 96.85 | 43.2% |
| opendir + readdirplus + releasedir | 29.00 | 20.24 | 30.2% |

修改位于共享 OverlayCore：候选 upper 的物理父目录刚被检查为不存在或
非目录时，直接跳过该候选的 whiteout/opaque 标记探测。没有跨请求缓存
属性或不存在结果，后续路径组件和最终物理祖先仍重新检查。深目录诊断中，
whiteout 与 opaque 探测各从 38,016 次降至 4,352 次。标记不会通过 upper
祖先符号链接影响 lower 可见性；新增测试还验证同一遍历中新出现的 upper
目录与 whiteout，以及请求之间的 upper 内容变化。

Core/host 适配层 106 项测试和 virtio-fs/descriptor/文件系统快照 57 项
测试通过，三个相关包的 Clippy 全 targets 检查通过。最初沙箱未暴露 `/dev/kvm`
和 `/dev/fuse`，当时 VM 全包测试在 KVM 初始化处失败；该微基准没有测
真实 VM/FUSE、journal/文件内容读写和 macOS。后续宿主设备验证与端到端
复测见[本页 P0 基线](#e2e-baseline)。两个制品之间
工作区另有 VM 重构，适配层案例不执行 UART/VMM/CPU 初始化；制品摘要
是本次测量身份依据。以上降幅不能替代下方历史工具工作负载的重新测量。

复现单版本适配层测量：

```bash
cargo nextest run --locked --release -p pvisor-vm \
  --run-ignored only --no-capture -E 'test(small_file_adapter_benchmark)'
```

[逐样本与诊断计数](../../assets/benchmarks/overlay-resolution-20261005/samples.json) · [制品、协议与汇总](../../assets/benchmarks/overlay-resolution-20261005/summary.json)

## Linux：2026-10-04 {#results}

### 完整 Ubuntu 的工具内部对照 {#full-ubuntu}

下表排除新环境开机，只计操作与结果校验。新批次每格 N=10、3 次预热；相同 fixture、两核预算、VM 16 GiB。pVisor 使用宿主目录和 staged virtio-fs，Ubuntu 使用官方 generic 内核、发行版工具和私有 ext4。旧 Docker N=30 矩阵继续保留，不合并百分位数。

| Workload | Native P50/P95 ms | pVisor staged P50/P95 ms | pVisor VM P50/P95 ms | Ubuntu P50/P95 ms |
|---|---|---|---|---|
| metadata | 4.85 / 5.11 | 177.80 / 195.52 | 291.82 / 359.75 | 18.64 / 19.90 |
| read | 33.29 / 48.97 | 48.12 / 61.21 | 88.83 / 127.21 | 115.51 / 117.69 |
| write | 3.75 / 4.43 | 186.95 / 208.86 | 134.37 / 154.03 | 36.07 / 36.35 |
| git | 14.66 / 17.07 | 172.18 / 194.52 | 613.69 / 807.73 | 136.50 / 140.81 |
| rg | 7.58 / 10.90 | 140.52 / 145.52 | 521.65 / 554.49 | 20.40 / 22.89 |
| cargo | 52.79 / 57.17 | 104.21 / 131.18 | 563.24 / 658.37 | 969.09 / 977.41 |
| npm | 218.82 / 245.85 | 256.29 / 292.67 | 2260.17 / 2408.63 | 1079.89 / 1110.93 |

可以据此定位遍历、搜索、编译和安装的实际等待；不能由某一个读取数字得出 block device 总是比 FUSE 快。数据同时改变了内核、工具版本、文件系统和暂存语义。长任务选型更应结合 worker 与[完整闭环](agent-tasks.md#full-ubuntu)，而不是只比较开机时间。

[逐样本 CSV](../../assets/benchmarks/full-ubuntu-20261004/samples.csv) · [分布与阶段计时](../../assets/benchmarks/full-ubuntu-20261004/summary.json) · [方法与复现](methodology.md#full-ubuntu)

### 完整工具环境的 Docker 基线 {#reference-fs}

新增同机 Docker Engine 实测，不再借用 Podman 数字。相同两核预算与输入、每格 30 次、3 次预热；fixture 与首版相同，七种操作在一个新环境中依次执行。表中只计操作和校验，不含环境启动；整体修复任务另见[完整环境](agent-tasks.md#reference-env)。两个批次分别保留，不合并百分位数。

| Workload | Native P50/P95 ms | Docker P50/P95 ms | pVisor staged P50/P95 ms | pVisor VM P50/P95 ms |
|---|---|---|---|---|
| metadata | 5.04 / 6.78 | 5.06 / 7.50 | 180.13 / 202.88 | 310.54 / 351.59 |
| read | 33.07 / 41.44 | 33.24 / 45.06 | 48.77 / 61.60 | 89.27 / 99.34 |
| write | 3.96 / 5.09 | 3.95 / 5.65 | 189.28 / 204.67 | 144.66 / 164.67 |
| git | 15.75 / 20.50 | 16.07 / 20.07 | 177.81 / 195.97 | 456.21 / 786.35 |
| rg | 7.98 / 11.81 | 8.03 / 9.63 | 144.61 / 159.00 | 545.82 / 673.25 |
| cargo | 58.71 / 70.16 | 56.40 / 73.75 | 112.80 / 140.81 | 549.57 / 659.23 |
| npm | 183.46 / 197.57 | 231.45 / 288.60 | 222.97 / 256.33 | 1727.04 / 1978.79 |


这些数字给出具体位置：Docker 的 metadata/read/write 接近原生；staged 遍历小文件比 Docker 多约 **175 ms**，64 MiB 读取多约 **16 ms**。读取一次只多十几毫秒，反复遍历小文件会累积明显成本。VM 的离线 npm 安装约 **1.73 秒**，Docker 约 **0.23 秒**，这条路径仍有明显差距。不能用约 86 ms 的 VM 启动时间替代这个工具预算。

任务核对文件数量/大小、SHA256、Git clean、搜索命中、编译结果与安装包数量。Docker 是 writable bind mount，pVisor 使用 staged 视图；Firecracker/QEMU 在私有 ext4 内执行。文件路径不同是实际部署成本的一部分；这不是相同文件系统只替换 VMM 的因果实验。其他运行时的各操作分布也在本轮汇总中。

[配置与复现](methodology.md#reference-env) · [逐样本 CSV](../../assets/benchmarks/reference-env-20261004/samples.csv) · [分布与阶段计时](../../assets/benchmarks/reference-env-20261004/summary.json) · [原始证据](../../assets/benchmarks/reference-env-20261004/evidence.tar.gz) · [兼容性矩阵](../../assets/benchmarks/reference-env-20261004/compatibility.json)


### 首版 Linux 矩阵：独立批次

| Workload | Backend | N | Worker P50 ms | vs native | Wall P50 / P95 / P99 ms |
|---|---|---|---|---|---|
| metadata | native | 30 | 4.84 | +0.0% | 23.44 / 29.17 / 30.71 |
| metadata | host | 30 | 4.94 | +2.0% | 34.02 / 44.90 / 58.18 |
| metadata | staged | 30 | 149.60 | +2988.5% | 225.03 / 295.08 / 314.55 |
| metadata | safe | 30 | 171.26 | +3435.6% | 288.73 / 346.47 / 363.59 |
| metadata | vm | 30 | 263.08 | +5331.3% | 636.44 / 828.38 / 892.94 |
| metadata | podman | 30 | 4.95 | +2.1% | 138.99 / 158.58 / 159.34 |
| metadata | container | 30 | 4.83 | -0.3% | 320.21 / 419.26 / 479.51 |
| read | native | 30 | 32.66 | +0.0% | 52.81 / 125.98 / 143.36 |
| read | host | 30 | 31.80 | -2.6% | 64.65 / 213.37 / 232.88 |
| read | staged | 30 | 40.76 | +24.8% | 95.24 / 338.92 / 378.04 |
| read | safe | 30 | 41.38 | +26.7% | 153.98 / 386.69 / 390.96 |
| read | vm | 30 | 100.58 | +208.0% | 476.56 / 1379.77 / 1436.40 |
| read | podman | 30 | 32.68 | +0.1% | 170.64 / 408.95 / 452.05 |
| read | container | 30 | 33.57 | +2.8% | 405.55 / 945.63 / 1092.93 |
| write | native | 30 | 3.81 | +0.0% | 22.95 / 26.54 / 55.29 |
| write | host | 30 | 3.81 | -0.1% | 34.16 / 46.23 / 93.69 |
| write | staged | 30 | 192.46 | +4945.6% | 252.61 / 444.20 / 812.30 |
| write | safe | 30 | 199.56 | +5131.6% | 312.83 / 549.31 / 762.48 |
| write | vm | 30 | 114.68 | +2906.4% | 476.94 / 694.88 / 1003.73 |
| write | podman | 30 | 3.76 | -1.4% | 139.68 / 151.53 / 340.87 |
| write | container | 30 | 3.82 | +0.1% | 356.89 / 406.97 / 627.19 |
| git | native | 30 | 14.72 | +0.0% | 36.14 / 79.21 / 165.50 |
| git | host | 30 | 14.51 | -1.4% | 54.34 / 65.11 / 92.63 |
| git | staged | 30 | 203.33 | +1281.7% | 333.16 / 458.84 / 1316.66 |
| git | safe | 30 | 225.99 | +1435.7% | 394.38 / 491.36 / 960.78 |
| git | vm | 30 | 535.06 | +3536.0% | 929.22 / 1558.97 / 2447.31 |
| git | podman | 30 | 15.55 | +5.7% | 158.87 / 209.35 / 215.16 |
| git | container | 30 | 15.78 | +7.2% | 375.18 / 434.76 / 490.77 |
| rg | native | 30 | 6.72 | +0.0% | 26.54 / 28.70 / 29.90 |
| rg | host | 30 | 6.64 | -1.2% | 44.01 / 44.79 / 45.02 |
| rg | staged | 30 | 167.90 | +2399.9% | 288.07 / 312.76 / 320.23 |
| rg | safe | 30 | 190.82 | +2741.2% | 349.80 / 380.69 / 389.61 |
| rg | vm | 30 | 340.98 | +4976.9% | 717.00 / 769.33 / 780.18 |
| rg | podman | 30 | 5.90 | -12.2% | 141.36 / 154.73 / 160.82 |
| rg | container | 30 | 6.39 | -4.8% | 355.33 / 390.96 / 395.69 |
| cargo | native | 30 | 49.09 | +0.0% | 67.21 / 84.93 / 86.36 |
| cargo | host | 30 | 47.07 | -4.1% | 83.86 / 94.88 / 95.49 |
| cargo | staged | 30 | 90.31 | +84.0% | 146.28 / 167.31 / 168.42 |
| cargo | safe | 30 | 96.00 | +95.6% | 188.83 / 209.83 / 224.90 |
| cargo | vm | 30 | 541.56 | +1003.1% | 897.61 / 959.14 / 959.43 |
| npm | native | 30 | 157.56 | +0.0% | 175.60 / 205.84 / 434.16 |
| npm | host | 30 | 156.38 | -0.7% | 184.73 / 204.95 / 552.20 |
| npm | staged | 30 | 187.87 | +19.2% | 235.76 / 256.13 / 608.48 |
| npm | safe | 30 | 232.30 | +47.4% | 318.58 / 432.25 / 708.12 |
| npm | podman | 30 | 208.41 | +32.3% | 339.64 / 474.65 / 857.37 |
| npm | container | 30 | 195.29 | +23.9% | 515.88 / 552.03 / 891.62 |

### 分析

暂存视图的连续读取约 +25%，离线 npm 约 +19%；元数据约 31 倍、写小文件约 50 倍。倍率很大，也要同时看原生只有几毫秒和增加约 150–190 ms 的绝对值。host 几乎没有 worker 开销，总时长仍增加 CLI 与记录成本。safe 再增加命名空间、限制和代理准备；VM 多数整项任务在约 0.5–1 秒量级。

该历史批次的 OCI 对照使用 Podman，当时 Docker daemon 不可访问；后续已补充 rootless Docker Engine 的 bind-mount 数据，见上方完整环境对照。Docker overlay2 和 Docker Desktop 仍未测量。pVisor OCI 每个 Job 准备私有 rootfs，wall 反映这个成本；worker 单独显示工具执行成本。

### 工具兼容性补测

主批次容器 cargo 缺少链接启动文件，是镜像准备错误；补齐 glibc/GCC 文件后发现 Fedora 链接脚本还要求 /lib64/libmvec.so.1；最终补全该路径的 cargo-ready 批次全部通过，单独呈现，前两次镜像错误保留在报告。VM 的 1 GiB 配置会使 Node V8 地址空间预留失败；补测使用 **16 GiB 地址空间配置**，与 1 GiB 主批次分别列出。

| Workload | Backend | N | Worker P50 / P95 / P99 ms | Wall P50 / P95 / P99 ms |
|---|---|---|---|---|
| cargo | native | 30 | 52.90 / 65.79 / 76.17 | 71.96 / 89.41 / 103.04 |
| cargo | host | 30 | 52.60 / 70.49 / 75.30 | 84.38 / 111.79 / 124.56 |
| cargo | staged | 30 | 116.07 / 139.93 / 233.90 | 166.65 / 210.57 / 397.55 |
| cargo | safe | 30 | 119.77 / 138.77 / 228.23 | 209.92 / 242.16 / 342.22 |
| cargo | vm | 30 | 567.42 / 1365.43 / 2047.17 | 1271.15 / 2683.43 / 3889.27 |
| npm | native | 30 | 162.86 / 433.44 / 488.04 | 181.47 / 472.60 / 531.93 |
| npm | host | 30 | 163.56 / 430.53 / 475.14 | 205.34 / 522.04 / 595.23 |
| npm | staged | 30 | 213.17 / 437.03 / 612.15 | 266.48 / 544.87 / 780.57 |
| npm | safe | 30 | 256.70 / 717.56 / 777.67 | 339.63 / 983.34 / 1085.49 |
| npm | vm | 30 | 985.29 / 3238.12 / 4617.06 | 1661.91 / 5172.18 / 5755.11 |
| npm | podman | 30 | 217.06 / 547.44 / 833.45 | 365.17 / 883.42 / 1239.59 |
| npm | container | 30 | 202.48 / 587.65 / 601.74 | 550.89 / 1368.06 / 1469.75 |


| Cargo corrected /lib64 image | N | Worker P50/P95/P99 ms | Wall P50/P95/P99 ms |
|---|---|---|---|
| native | 30 | 51.52 / 59.70 / 62.44 | 70.85 / 80.25 / 82.02 |
| podman | 30 | 49.29 / 54.35 / 55.75 | 181.18 / 187.71 / 189.70 |
| container | 30 | 51.99 / 57.94 / 59.96 | 405.08 / 425.10 / 425.46 |

## 边界与下一轮 {#acceptance}

VM 主批次使用 host rootfs `/` 和 2 vCPU/1 GiB，并提供宿主只读视图；这是工具兼容性配置，不能据此宣称宿主敏感文件不可读。隔离配置另见[隔离验证](isolation-tests.md)。这些小型、热缓存、离线任务不是完整大仓库构建，也不是冷磁盘吞吐。Linux FUSE 已测，macFUSE/FSKit、真实 registry 安装与 Docker/overlay2 留给后续同协议测量。

## 复现与证据 {#run}

从仓库根目录运行；输出目录必须是新目录。动态 firmware 入口需要 GNU/Linux 版 CLI；静态 musl 制品使用另一套 firmware 入口，不能混用。Linux、KVM/FUSE/user namespaces、Python 3.14、Rust/GCC、Git/rg、Node 24/npm、Podman/crun 是本机工具环境；Agent 套件另需 Claude/Codex CLI。

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --suites filesystem --samples 30 --warmups 3
```

先用 `--samples 1 --warmups 0` 检查环境。脚本、输入定义和正确性断言在 `benchmark/pvisor/v1/`；报告会复制二进制、firmware 和当轮脚本并记录摘要。失败不会进入性能分布；准备、策略拒绝和工作负载错误分别保留。每个页面写明有效样本数；小样本 P95/P99 只描述本批次，不估计长期尾延迟。

[环境、制品与采样方法](methodology.md#product-v1) · [批次清单](../../assets/benchmarks/product-v1-20261004/manifest.json) · [逐样本汇总 CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [原始报告与日志归档](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz)。报告保留源码的未提交状态；当轮可执行制品 SHA256 是身份依据。归档不含数 GB 的 rootfs、二进制和可重建工作区；输入摘要及每轮脚本保留。
