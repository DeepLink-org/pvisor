# pVisor V2 与原生 Docker：镜像存储、启动和内存

## Motivation {#motivation}

pVisor V2 在这组真实镜像上明显减少磁盘库存；原生 Docker 容器在轻量 shell 负载下启动更快、每实例内存增量更低。选择 V2 的收益主要是压缩镜像供给和每实例 VM 边界，不能把镜像压缩解释为所有运行指标都优于原生容器。 该记录保留独立的本地工程批次；当前完整供给、远程取数和本地重复 mmap 结果见[惰性镜像基准](../benchmarks/lazy-image-startup.md)，批次不合并。

四种固定镜像的独立库存节省 71%–73%，真实四镜像共享库存节省 74%。已准备 Ubuntu 的 Ready 中位数为 Docker 80.69 ms、pVisor VM 106.38 ms；四实例时，扣除空基线后的平均增量分别为 8.46 MiB、48.83 MiB/实例。完整部署计费还受镜像页缓存和 daemon 固定开销影响，不能只用总量判断每实例成本。

## 实验设计 {#interpretation}

测量相同 OCI 镜像的文件分配量、启动到首条正确输出的等待和完整产品组内存计费，作为磁盘、启动和部署内存成本。这些指标反映镜像准备好之后的本地使用开销。数值越低，所需磁盘或内存预算越少、短任务等待越短。

2026-10-25，Linux x86_64/KVM、AMD Ryzen 7 9700X、内核 7.2.11-300.fc45。使用 rebase 到 main 前冻结的 V2 release/musl CLI，对照原生 Docker Engine 29.7.2/runc；没有 OpenSandbox、E2B、execd 或额外 API 服务。每个容器是 Linux namespace 容器或独立 pVisor guest kernel，隔离机制不同；没有启用额外物理内存池。

容量和运行时是两个独立批次，不合并样本。容量使用 Alpine 3.20、Ubuntu 24.04、Python 3.12/3.13 slim-bookworm 的固定 linux/amd64 manifest。逐个验证 manifest、config 和全部压缩 layer 的 SHA-256；Docker 的 RootFS diff_ids 和 pVisor catalog 身份也须相符。双方全部从新私有 store 导入，每个单镜像与真实四镜像组合各测三次。Docker 是独立 rootless/containerd overlayfs，包含正常层共享、压缩 blobs、解包 snapshots 和元数据，单列并扣除空 daemon 库存。pVisor 包含整个 Docker-profile store 的压缩内容、索引、config 和 catalog，不保留解包树或 layer archives。导入源、测量工具、二进制和运行写层不计入。

容量在运行校验前采集，硬链接 inode 只计一次，symlink 不跟随；不可读的子树使测量失败，不静默漏计。双方实验目录关闭 Btrfs 透明压缩，宿主全局配置未改。分配量为 `st_blocks × 512`，不等于独占物理设备字节，也不含全局文件系统元数据。逐实例校验 `/etc/os-release`、`/bin/sh` 摘要一致，重复 pull 不增加池内容。

启动使用同 digest Ubuntu，镜像和私有 daemon 预先准备。每次创建新实例，3 对预热、30 对 seed 819 随机交替；保留宿主页缓存，不含镜像拉取和服务启动。双方执行相同 shell：检查私有文件不存在、写入并读回 token、输出唯一 Ready 行，然后正常退出。时钟包含同一小型 cgroup launcher、CLI 创建和输出管道；由 semspec observer 在 Popen 前启动、Ready 行接收时停止，正常退出另计。不是存储索引 Open、冷磁盘或 SDK 创建耗时。所有有效慢样本保留，30 个样本不报告 P99。

运行组限制为 4 CPU quota、4 GiB、零 swap；CLI/服务固定 CPU 0–3。宿主没有委派 cpuset，使用同镜像 taskset 在应用前设置 Docker 的宿主 CPU mask 与单 vCPU VM 的 guest CPU 0；窗口内验证实际成员亲和性，不能据此证明所有短暂 init 线程过去的亲和性。双方请求 1 CPU/256 MiB，VM 的 guest RAM 与容器内存 limit 范围不同，不能称为等价隔离或相同应用可用内存。

内存另测五个随机配对的新产品组，一次只运行一组；dockerd、rootlesskit/pasta、containerd、shim 和容器，或 supervisor/VMM 全部纳入。镜像导入先在同组完成，空基线包含其页缓存／残留管理开销，以及双方相同的委派叶保持进程。无实例、1 实例、4 实例各观察三秒，每 200 ms 采集、取最后两秒中位数，再汇总五组。核对存活 owner/VMM、容器 PID、实际 cgroup 上限与归属、零 OOM/swap、私有 token 和原生删除；预检失败单独保留，不计入正式结果。观察者在组外，预先计费在组外的共享页缓存不计入，所以它不是整机新增物理内存。

## 实验数据和分析 {#results}

### 真实镜像磁盘库存 {#disk}

单位 MiB（2²⁰ bytes），N=3 个全新库存的中位数；末列是全部三次的范围，Docker 为扣除空库存后的镜像增量。

| 库存 | Docker MiB | pVisor V2 MiB | 磁盘节省 | Docker / V2 min–max MiB |
| --- | --- | --- | --- | --- |
| alpine-3.20 | 12.53 | 3.59 | 71.3% | 12.52–12.53 / 3.59–3.59 |
| ubuntu-24.04 | 110.18 | 31.64 | 71.3% | 110.18–110.18 / 31.64–31.64 |
| python-3.12-slim-bookworm | 176.68 | 47.15 | 73.3% | 176.68–176.68 / 47.15–47.15 |
| python-3.13-slim-bookworm | 172.34 | 46.18 | 73.2% | 172.34–172.34 / 46.18–46.18 |
| combined | 363.16 | 94.29 | 74.0% | 363.16–363.16 / 94.29–94.29 |


![真实镜像与共享库存的分配量](../assets/benchmarks/image-format-capacity.svg)

四镜像组合来自一个真正共享的 store，Docker 已正常复用公共层，不能把四个独立库存相加作为对照。下表拆出 Docker 原生 blob 和 snapshot 存储，中位数、MiB；主表另外包含数据库和其他元数据，因此各列不保证恰好加总。

| 库存 | Docker blobs MiB | Docker snapshots MiB | Docker empty MiB |
| --- | --- | --- | --- |
| alpine-3.20 | 3.47 | 9.05 | 0.24 |
| ubuntu-24.04 | 28.40 | 81.78 | 0.24 |
| python-3.12-slim-bookworm | 43.19 | 133.45 | 0.24 |
| python-3.13-slim-bookworm | 42.17 | 130.13 | 0.24 |
| combined | 90.29 | 272.75 | 0.24 |


V2 的容量收益来自保留压缩内容和索引、按目录打包小文件以及内容复用。该结果适用于所测 Docker 29/containerd overlayfs 与这组镜像，不代表所有镜像或所有 Docker 存储驱动；不包含导入峰值，也不证明 mmap／读吞吐或 GC 优势。

### 已准备镜像的原生启动 {#startup}

单位 ms，N=30 对的中位数；差值为逐对 pVisor−Docker 的中位数，95% CI 为 5,000 次配对 bootstrap。两项中位数之差不必等于中位配对差。

| 指标 | Docker ms | pVisor VM ms | 配对差 [95% CI] ms |
| --- | --- | --- | --- |
| 首条校验输出 Ready | 80.69 | 106.38 | +24.67 [+22.63, +26.21] |
| 启动到正常退出 | 109.98 | 146.64 | +34.23 [+30.66, +36.03] |


![首条输出与正常退出的全部配对样本](../assets/benchmarks/image-format-latency.svg)

每个点保留一个正式样本，连线表示同一轮；箱体给出分位范围，不隐藏慢样本。该轻负载中原生 Docker 更快。V2 的小索引和压缩池并未让完整 VM 创建成本消失；这组结果不能与带 SDK／服务适配层的历史对照混用，也不能把 Ready 的差异归为单独的索引算法成本。

### 完整部署内存与实例增量 {#memory}

单位 MiB，N=5 个新产品组的中位数，方括号为 min–max；差值为五对组的中位配对差及 bootstrap 95% CI。每实例增量先在各组计算 `(阶段总量−同组空基线)/实例数`，再取中位数，不用两项总体 P50 相减。

| 阶段 | Docker MiB | pVisor VM MiB | 配对差 [95% CI] MiB |
| --- | --- | --- | --- |
| 无实例，镜像已准备 | 211.47 [208.08, 212.85] | 35.36 [34.44, 35.57] | -176.27 [-178.41, -172.68] |
| 服务与 1 个空闲实例 | 219.44 [215.38, 220.75] | 84.14 [83.68, 84.37] | -135.07 [-137.00, -131.12] |
| 服务与 4 个空闲实例 | 245.33 [242.00, 247.25] | 230.67 [230.36, 231.05] | -14.66 [-16.19, -11.32] |
| 1 实例：扣基线后每实例增量 | 7.86 [7.30, 7.97] | 48.86 [48.32, 49.30] | +41.20 [+40.46, +41.56] |
| 4 实例：扣基线后每实例增量 | 8.46 [8.09, 8.93] | 48.83 [48.70, 49.15] | +40.42 [+39.90, +40.74] |


![部署总量、实例增量与内存分类](../assets/benchmarks/native-docker-memory.svg)

完整组计费包含 daemon、镜像缓存、匿名内存与内核开销；不是 `docker stats`、配置的 256 MiB 或单个进程 RSS。pVisor 的较小固定库存／管理开销可以降低整组读数，但原生容器的实例增量更低。若共享既有 Docker daemon，不能将本实验的全部固定成本归到每个新容器。四实例不能建立最大密度或盈亏平衡规模。

以下是同一批次 `memory.stat` 分类的五组中位数，MiB；分类和总量分别读取，可能略有统计偏差。

| 后端／阶段 | anon MiB | file MiB | kernel MiB |
| --- | --- | --- | --- |
| docker / empty | 90.47 | 109.70 | 8.91 |
| pvisor / empty | 0.11 | 31.64 | 1.90 |
| docker / four | 116.65 | 110.22 | 13.32 |
| pvisor / four | 190.46 | 31.83 | 6.93 |


镜像文件页缓存通常可回收，不能与 guest 匿名 RAM 当作等价的长期占用。容量规划应同时看 anon/file/kernel 和实例增量，不把本实验的 `memory.current` 差异解释为所有真实应用都更省物理内存。这里仅测空闲 shell/sleep，没有共享／独有大工作集、内存压力或长期稳定性结果。

### 来源与复现 {#evidence}

双方各 15 个容量库存与全部正式启动／内存组均通过身份、内容和生命周期校验，主对照均为原生 Docker/runc。完整 manifest、逐文件库存、CPU／内存见证、配对时钟、失败预检、冻结源码与制品保留在 `benchmark/storage/.data/native-docker-storage-formal-20261025/` 和 `native-docker-runtime-formal-20261025/`，不随站点发布。原始比较对象均为原生 Docker；内部格式回归另存本地证据。

固定镜像输入为下列平台 manifest；每个 config/layer 的完整收据在批次 inputs.json。

| Image | linux/amd64 manifest |
| --- | --- |
| alpine-3.20 | `sha256:c64c687cbea9300178b30c95835354e34c4e4febc4badfe27102879de0483b5e` |
| ubuntu-24.04 | `sha256:a2d43bffc46670d7108bb3c0468c67c5557c4c92e83e1e34cbff1322ad1b94dc` |
| python-3.12-slim-bookworm | `sha256:57d96f71de145a98e7ceca18ee4ade77bba4103a33a72dfaea026b23625c6bf0` |
| python-3.13-slim-bookworm | `sha256:4cea12ab7d78a3312c85f8cab19b6971d4fe9704da25443986c0bc20a6678850` |


| Artifact | SHA-256 |
| --- | --- |
| release CLI | 6f83d7e7e872761df9563137496bc5cb1286c842bd74b37c2ed28a0cd2c722fe |
| disk summary | eed2086cb76fa1d10ecdfa382cc7320583d9abd2593ba26aed822807068c2041 |
| startup summary | 3e16093d217c8a371d8bf9d171b7c2218edce9ca4e68e1396f731538fc7ba622 |
| memory summary | 70100c63051c078eaf5cb16f461ba7503b3ac3a3c875f6e74c9ec955a5f05376 |


实验分别登记为 B-IMAGE-STORAGE-ENG、B-NATIVE-DOCKER-ENG。复现入口与前提见[存储实验手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/storage/README.md)。新 runner 的库存链接／权限边界测试和 benchmark 注册／文档测试通过；未修改存储实现、语义审批 ledger 或快照。
