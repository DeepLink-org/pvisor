# TurboOCI 与 pVisor 按需镜像启动对比协议

**状态：实验协议，尚无本协议的测量结果。** 独立 OverlayBD/TCMU 实验环境已
部署，TurboOCI shell 已通过功能验证；见[部署记录](OVERLAYBD_LAB.md)。
四组对照的采样与审计 runner 尚未接通。此文件不构成已经完成的
TurboOCI 复现、pVisor 性能结论或用户 benchmark 成绩页。

归属 [B-LAZY-STARTUP](../../benchmark/README.md#b-lazy-startup)，遵守
[benchmark 规则](../../benchmark/README.md)和
[新入口的证据与发布规则](../README.md)。
[registry.csv](../registry.csv) 已登记 `startup/lazy_startup.json`，但
[该模板](lazy_startup.json)目前仍为未准备的占位表。四组设计已登记于
B-LAZY-STARTUP；正式测量前，仍须接通、预检实际的单条件命令；不能将
占位表或历史 campaign 当作可执行的新对照。

## 要回答的问题

同一台机器、相同源 OCI 镜像及应用命令下，TurboOCI 容器和 pVisor VM 的
客户端冷／热缓存 ready、completion、请求数与实际传输量分别是多少？
哪些准备成本发生在客户端启动之前，如何随重复使用摊销？

**隔离边界是选型结论的前提。** 容器与 VM 的跨运行时数据仅保留为端到端参考，
不能据此评价 OverlayBD 块路径和 pVisor 文件路径谁更快；相同 CPU 数量和
内存参数不等于相同隔离或整机资源预算。现有 cold/warm 容器适配器的单次预检
不升级为存储路线排名，也不继续以跨隔离的正式批次代替下述同 VM 对照。

存储路线的主对照应使用同一 VMM、同一 guest 内核、相同 OCI 用户态、应用、
可写层语义、资源预算和缓存策略：一组从 OverlayBD 虚拟块设备经 virtio-blk
及 guest ext4 读取，另一组经 pVisor 文件服务及 virtio-fs 读取。再分别记录
准备到 Ready、正常完成、实际传输与宿主服务开销；只有应用和输入均就绪的
guest 内阶段计时才能排除引导时间。该对照仍包含块/文件接口的设计差异，
不能解释为纯数据格式差异。

当前仅完成可行性检查：`pvisor-vm` 内部存在 virtio-blk 实现，但公开
`VmConfiguration` 尚无磁盘接入合同；普通 x86_64 固件配置也未启用 ext4。
因此同 VM 实验需要专用接线和两组共用的 ext4-capable 测试内核，目前尚无
此条件下的实测数据。不能为图方便将一组换成另一种 VMM 或另一套内核后仍
声称仅改变了存储路径。

先回答两个运行时内部的问题，再并列完整产品路径：

- 相同 containerd/runc 下，普通 OCI eager 与 TurboOCI lazy 的差异。
- 相同 pVisor VM 下，普通 OCI eager 与文件索引 lazy 的差异。

TurboOCI 容器与 pVisor lazy VM 的横向结果包含隔离、内核、文件接口及运行时
差异，只能解释为完整路径比较。不能把差值归因为镜像格式或 VM 隔离的单一
成本，也不能据此评价在途 I/O 卸载、并发容量或 Agent 吞吐。

## 与官方性能表的关系

参考上游 [TurboOCI 协议和性能表](https://github.com/containerd/accelerated-container-image/blob/main/docs/TURBO_OCI.md)。
上游四项为 `ai-cat-or-dog`、`php-laravel-nginx`、`python-small-import` 和
`python-large-import`，指标为镜像拉取到服务可用。

目前没有冻结这四项的完整镜像 digest、应用字节、入口、运行参数及实验环境，
不能精确复现其四个条件。首批采用现有 shell 与固定 NumPy 同类负载，二者
分别成批，不合并成一个分数。上游秒数仅作带来源和口径的独立背景；不得与
本机数字相除、计算加速倍数，或把 NumPy 结果标成官方 Flask 服务复现。
如后续取得原始输入，另建独立批次，不能补入首批样本。

## 四组条件

### 已准备环境的独立补充批次

首个性能批次使用 `image_compare.py`，比较普通 OCI 容器、TurboOCI 容器和
pVisor 本地预准备 rootfs VM。三者从同一源镜像派生，均运行相同 shell 字节，
每次创建新执行实例和可写状态。镜像及宿主页缓存保留，显式拉取、格式转换、
服务准备不计入 Ready/Completion；这是 prepared 场景，不能称为本节下面的
`oci-vm` 冷条件或完整四组复现。

3 次预热后，按冻结计划随机交替执行 30 轮。应用使用 CPU 12,13；容器任务为
2 CPU 配额/2 GiB，VM 为 2 vCPU/2 GiB guest RAM。宿主辅助进程和后台负载
单列，已有 CPU 0,1 测试保持原状；不声称宿主完全空闲。记录每轮运行时负载，
遇到采样 CPU 上的并发构建或其他已识别 benchmark，整批标记受干扰，不挑除
慢样本。容器时钟包含 Docker exec 客户端，VM 时钟从 CLI Popen 开始；两边
观察者解释器及未计时输入/清理均不计时。这些完整调用路径不能解释为纯后端成本。

准备生成固定 Python 配对行 `in.json`，由 `semspec bench` 采样；专用审计从
逐次证据重算统计后生成 Markdown/CSV。未通过专用审计不更新用户 benchmark 页，
也不使用通用 `update.py` 白名单以外的格式冒充已审核结果。

### 包含客户端镜像准备的完整条件

| 条件 ID | 客户端与后端 | 冷客户端内必须完成的工作 |
| --- | --- | --- |
| `oci-container` | 私有 containerd、同一 runc、普通 OCI/overlayfs snapshotter | 从私有 registry 获取源 OCI，完整准备容器 rootfs，再执行负载 |
| `turbo-container` | 同一版本与配置的 containerd/runc、OverlayBD/TurboOCI snapshotter、明确冻结的块设备后端 | 获取派生元数据，按实际访问取得原 OCI 内容，再执行相同负载 |
| `oci-vm` | 同一 pVisor 二进制、固件和 VM 参数、普通 OCI 完整加载 | 从源 OCI 完整准备所需 rootfs，再启动 VM 和负载 |
| `lazy-vm` | 同一 pVisor VM、预准备的文件索引服务 | 获取客户端索引与实际访问的数据，再启动 VM 和负载 |

这里的容器 `overlayfs` 是 containerd 的 Linux 内核 snapshotter，不能与
pVisor 自有 OverlayCore 文件服务混为一谈。两个 VM 条件保留相同的
virtio-fs、stage、网络开关及执行参数，仅改变镜像加载路径。
`oci-vm` 的冷计时包含客户端完整拉取／解包，不能用预先物化的 rootfs 绕过；
热条件可以复用已准备输入，但仍创建新的 VM 和可写状态。

四组共用相同的源平台 manifest digest、config 和原始压缩 layer blobs。
TurboOCI 另记派生 index／manifest／元数据 layer digest、转换工具版本和
源到派生的映射；pVisor 另记镜像句柄、revision/COMMIT 与文件索引摘要。
派生 digest 不要求与源 digest 相同，必须能追溯到完全相同的源字节。
禁止通过 rootfs 重新打包生成“等价”源 OCI，也不允许给某一组单独升级软件包。

## 首批负载与正确性

负载复用 [现有 lazy runner](../../benchmark/pvisor/lazy_startup.py)的固定输入，
实现新 runner 时冻结该源码，不引用会随工作区改变的常量。

| 负载 | 源镜像与校验 | Ready 输出 |
| --- | --- | --- |
| `ubuntu-shell` | `docker.io/library/ubuntu@sha256:88a381d5b5eeb2b35d3ad70925a362c37ce569daf43ede89ff818ec20e4d3794`；固定 `linux/amd64` manifest，校验 `/etc/os-release` 的 Ubuntu 26.04 身份 | 唯一精确行 `LAZY_READY ubuntu 26.04` |
| `numpy-script` | `docker.io/amancevice/pandas@sha256:9a3a94039175ac799ad33c1a207997994ff9b24814e06508dddfa259b1ed9159`；校验 Python 3.13.14、NumPy 2.5.2、整数平方和 1240、矩阵运算总和 3680 | 唯一精确行 `LAZY_READY python 3.13.14 numpy 2.5.2 sum 1240 dot 3680` |

准备时核对平台 manifest；若上述 digest 是多架构 index，同时保留 index 和
本次 `linux/amd64` 子 manifest，不在每次 trial 重新解析移动 tag。
版本校验不通过即拒绝运行，不能为使样本通过而修改预期值。

四组使用相同的 workload 脚本字节和解释器参数。NumPy 禁用 bytecode 写入，
固定 BLAS/OMP 等线程数为 1；每次运行使用新的私有 HOME、临时目录和可写层。
shell 与 NumPy 均须零退出，镜像基线与宿主 canary 未被改动；VM 还需核对
Run Bundle 中的实际 VM 执行模式、stage 与已安装控制。
正常输出之前的异常退出、重复或乱序 marker、错误结果均为失败，不进入有效统计。

## 前置条件与资源配置

首批限定 Linux x86_64/KVM。预检记录宿主内核、CPU、内存、存储文件系统、
可用磁盘空间、cgroup v2、namespace、虚拟化权限及所选四个逻辑 CPU。
应用／VMM 使用固定两个 CPU，镜像服务、snapshotter、代理等使用另外两个；
记录所有实际亲和性与未能纳入限制的辅助进程。

- 容器应用配置两核和 2 GiB 限制；VM 配置 2 vCPU、2 GiB guest RAM。
  容器硬限制与 guest RAM 不是等价的整机内存限制。宿主服务、VMM、缓存及
  kernel 开销单独记录，结果不得称为完全等预算的隔离机制比较。
- 冻结 containerd、runc、OverlayBD、snapshotter、convertor、pVisor、
  `pvisor-daemon` 和固件的版本、源码及二进制摘要。服务进程必须匹配被测制品。
- 当前部署官方包为 OverlayBD v1.0.18 的 azurelinux3 x86_64 RPM，以及
  accelerated-container-image 的 snapshotter/convertor v1.4.5 x86_64 RPM。
  前者只带 TCMU 程序，没有 UBLK 可执行文件；后者程序位于
  `/opt/overlaybd/snapshotter`。部署目录保存包摘要、实际程序路径与动态依赖；
  正式测量仍须为本批次冻结制品，不能把部署 smoke 当作采样通过。
- 官方预构建包路线先核验 TCMU：`target_core_user`、configfs、所需设备以及
  创建 target／挂载所需的宿主权限必须可用。本机 KVM 可用，标准 TCMU 与
  loopback 模块已加载；实验通过 privileged Docker 操作所需内核接口，UBLK
  未启用。这些是部署状态，不是性能结果。
  实验只创建并清理具有本批次明确所有权的 target、设备和挂载。
- 如选择 UBLK，除了启用 `ublk_drv` 和具备 `/dev/ublk-control` 权限，还须
  核验支持该后端的 OverlayBD 源码构建产物、依赖及 snapshotter 接线，并完成
  本批次设备的创建、读写与删除验证。不能认为加载模块即可运行候选 RPM。
  TCMU 与 UBLK 使用不同配置与批次，后端不可用时保留预检失败，不静默切换。
- containerd root/state/socket、snapshotter 状态、OverlayBD cache、挂载点、
  设备 ID、镜像服务和私有 registry 均有本批次独立所有权及清理清单。
  不直接运行上游会停止共享 daemon、删除系统 containerd 数据或清理全局缓存的
  测试／清理脚本。清理只作用于清单内已确认属于本批次的资源。
- 首批 registry、计数代理及 pVisor 镜像服务均使用本机 loopback，无人工带宽
  或延迟注入，只能称为本机模拟远程协议。TLS、认证、重定向和实际端点在
  manifest 固定；若不同路径无法采用相同传输方式，明确披露差异。

驱动启用和宿主配置不属于计时工作。按部署记录准备后，正式批次仍须验证私有
设备、挂载、资源约束和清理闭环，再进入预热；采样期间不改变宿主全局配置。

## 准备成本和客户端时钟

准备阶段独立记录，不混入客户端 Ready，也不忽略其使用成本：

| 阶段 | 至少记录 |
| --- | --- |
| 原 OCI 获取与私有 registry 发布 | 源 digest 校验、获取／复制／发布时间、字节和缓存来源 |
| TurboOCI 元数据构建与发布 | 构建时间、工具参数、元数据与原始 blob 的关系、上传量和存储量 |
| pVisor 镜像服务准备 | 首次拉取／解包／建索引／发布的各段时间、字节、存储量及 revision |
| 共享服务启动和预检 | 服务启动时间、配置、基础驻留及预检结果 |

若复用已经验证的原 OCI blobs 或 pVisor store，只能报告“缓存输入准备”，
不能沿用另一批次的首次获取耗时。准备前后都保留输入摘要；服务端不允许在
正式样本中悄悄回退到公网 registry 或重建另一版本镜像。

客户端 `t0` 为观察者使用 `perf_counter_ns`、在发起本次显式拉取／准备／运行
链路之前的时间。它包含客户端命令创建、拉取、挂载、VM/container 创建和
负载执行；不包含事先完成的服务端派生格式准备、观察者解释器启动和后续审计。
冷 OCI 的完整准备不能放到 `t0` 之外。

- `Ready = t_ready - t0`：观察者收到唯一正确 marker；marker 必须在全部负载
  校验之后输出。它表示该脚本的首次有效输出，不是完整 OS 启动或 HTTP 服务可用。
- `Completion = t_exit - t0`：同一次运行正常零退出；不能用 Ready、受控终止或
  内部某一阶段代替。两项来自同一 trial，不分别重新运行。
- 固定所有条件相同的显式总超时，在正式批次前写入 manifest。超时保留输出和
  阶段记录，计为失败；不得按某个条件单独放宽后合并统计。

未来如接入官方 Web 负载，使用同一外部 HTTP 成功响应及内容校验定义
Service Ready，另记探测频率和网络路径；不与当前脚本 Ready 合并。

## 冷／热客户端及局部清理

每轮每条件执行一次冷客户端，紧接一次热客户端。两次均创建新的执行实例、
工作区、stage 和可写层；热条件仅保留该条件冷样本获得的镜像输入及客户端
只读数据缓存，不能复用正在运行的 VM、进程或工具输出。

冷条件使用新的私有客户端目录：普通 OCI 清空本条件的镜像内容／rootfs 缓存，
TurboOCI 清空本条件的 metadata、块及 gzip 缓存，pVisor lazy 清空本条件的
客户端索引与内容缓存。先结束使用者并解除本批次挂载，再删除目录；不能只删
索引而保留打开的设备或 daemon 内存缓存。每次记录清理范围、空目录检查和
服务重建／重置情况。

服务端输入、registry 存储和宿主页缓存按预先声明的共同策略保留。不执行
全机 `drop_caches`、Docker/containerd prune 或删除无关缓存。由于宿主页缓存
未被全局清理，名称固定为“冷客户端／热客户端”，不能写成冷磁盘或整机冷启动。

预检确认普通 OCI 冷条件实际取得全部所需源 blobs；lazy 冷条件从声明源取得
所需内容，不能由未登记的本地 rootfs 满足。热普通 OCI 不得发生远端 blob
传输，热 lazy 不得重新传输文件内容／数据块，允许元数据请求并单独计数。
零传输结论必须来自服务／代理日志及缓存检查，不能只看 TUI。

## 请求与实际传输量

所有路径记录请求时间、endpoint、对象 digest/句柄、方法、Range、状态码、
`Content-Range`、声明长度、实际发出的响应 body 字节、重试及中途断开。
计数日志与 trial 的开始、Ready、退出相互关联，分别保留截至 Ready 及截至
Completion 的累计量。准备阶段流量另列。

对于 OCI/TurboOCI 的 HTTP Range 请求，预检必须验证私有 registry／计数代理
能正确处理 `206 Partial Content`：返回的区间、body 长度和对应原 blob 切片
逐字节一致。正式计数遵循以下规则：

- `206` 只累计实际响应 body，不把 `Content-Range` 中的完整对象长度计成
  已下载字节；不能因响应不是 `200` 而漏计。
- 重复区间、重试及断开前已发送的字节均计入实际传输；唯一覆盖区间另列，
  不能用去重后的覆盖量替代实际流量。
- Range 被忽略并返回 `200` 时按实际完整 body 计数，并保留这一事实；不能按
  请求区间估算下载量或把有效的慢样本剔除。
- manifest、config、TurboOCI 元数据和原始 layer 数据分类记录。pVisor 同样
  分列索引／元数据与内容数据，并记录缓存服务实际返回量。TUI 的逻辑 read
  字节不能替代抓取整个内容块产生的传输量。

首批统一报告应用协议的实际响应 body 字节，不称链路总字节；HTTP 头、TLS、
TCP/IP 及请求方向开销不在此口径内。若另记录 wire bytes，保留单独列和范围，
不能与协议 body 或解压后的逻辑文件字节相加。gzip 解压缓存占用另记。

## 预检、样本和统计

每负载先完成四条件各一次冷／热预检，验证：固定输入与派生映射、相同负载
字节、版本和结果、真实 lazy 请求、Range `206` 计数、热缓存不重新取内容、
正确退出、Run Bundle、宿主 canary、以及本批次设备和进程的完整清理。
任何一项未满足则保留预检材料，正式样本数为零。

正式协议为每个独立负载 **3 轮预热＋30 个配对轮次**。每轮使用预先冻结的
随机种子打乱四条件顺序，每个条件保持“冷后紧接热”。因此每个负载每格
`N = 30`、共 240 次正式启动；24 次预热和预检另记，不进入统计。
四条件不同时运行，避免相互争抢 CPU、存储和服务端队列。

随机交替必须由新 runner 明确实现并保留逐轮计划；不能用现有逐行、按条件
顺序执行的 hyperfine 输出冒充配对实验。复用事件时钟和审计接口，不把旧的
多样本 campaign 包成一次 hyperfine 命令。

报告每格 N、失败数、P50、均值、SD、Min/Max；P95 仅供参考，不报告 P99。
对照差值使用同轮配对和轮次级 bootstrap 95% CI，冻结统计方法及种子。
CI 包含零时写“未检出差异”。分离／双峰分布报告各簇比例和中位数，不用单一
中位数掩盖模式。不同负载、缓存状态、批次、制品及网络条件不合并。

预先记录超时、错误输出、输入摘要变化、资源／服务错误等失败判据，保留失败
样本和原因。任何正式正确性或资源隔离失败阻止本批次性能发布；失败不能当作
零耗时，慢但正确的有效样本不剔除。外部干扰及诊断记录不能成为事后挑样依据。

## Runner 接线与完整证据

实现优先复用现有代码中的固定负载、私有 registry、缓存状态检查与传输记录，
以及 [startup runner](README.md)的 Ready/Exit 事件时钟、输入冻结和审计逻辑。
本协议没有新增一个已能运行的命令；`startup/lazy_startup.json` 仍须接通实际
条件，统一走 prepare → semspec bench → audit → render。统计工具和是否使用
Python 事件时钟须明确，不能把 Python 时钟标成 hyperfine 数据。

证据存入新的 `benchmark.new/startup/.data/` 批次目录，不复用已有输出。
至少保留：

- `manifest.json`：benchmark ID、协议版本、环境／权限、时钟范围、CPU／内存
  配置、网络、源及派生 digest、软件／源码／固件摘要、cache scope、超时、种子。
- 冻结的 `harness/`、输入脚本、原 OCI manifests/config/blobs 清单、TurboOCI
  派生关系和 pVisor revision；有无 source-build receipt 分别声明。
- 实际 `in.json`、原始 `in.bench.json`、逐次事件、stdout/stderr、退出与正确性
  记录、Run Bundle、配对计划、缓存检查、设备／挂载／进程所有权与 teardown 收据。
- 准备阶段时间／字节／存储、逐请求与 `206` 计数、元数据／内容统计、宿主辅助
  服务 CPU／内存、磁盘用量、失败和诊断材料，以及统计生成参数。

发布前从原始事件复算 Ready、Completion、流量与配对统计，核对首尾制品和
输入摘要，确认所有正式样本及清理通过。随后核对对应注册设计，补齐文档绑定和
审计支持；没有 live binding 或审计证据不发布用户表。
用户页仍遵守结论、动机、设计、数据与分析的结构；本协议、未接通状态及诊断
记录不冒充性能结果。
