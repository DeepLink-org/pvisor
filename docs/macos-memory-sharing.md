# macOS 跨 pVisor RAM 共享与冷页压缩：实验设计

> 首版已收敛并接入 CLI / SDK：见[当前使用与合同](src/zh/design/memory-optimization/proof-of-concept.md#v1-integration)。停止扩展实验矩阵，本页继续保留历史记录，完整物理验收与恢复尾延迟限制未改写为已通过。

当前设计说明已整理到[文档系统：内存去重与冷页压缩概念验证](src/zh/design/memory-optimization/proof-of-concept.md)。本页保留逐轮实验、失败记录与历史取舍；当前实现与数据适用范围见新文档。

`tools/experiments/macos-memory/` 的历史脚本已移除。下列路径与实验命令仅记录当时的执行方式，不能在当前工作树运行；原始证据与实验结论保留。

状态：实验进行中，默认关闭。两个真实 pVisor VM 已通过自动冷页回收、
跨进程压缩对象共享、恢复内容校验，以及真实宿主 WARN 压力下的六次匹配运行。
新增双账目实验剔除逻辑换出后的 VM-object 驻留账目下降中位数为 61.5%，
pmap 驻留账目下降 14.2%，尚不能替代全机物理内存验收。旧扫描十轮网络匹配
六个 case 累计约 15.5 分钟通过，持续 RAM＋pool 代理降幅中位数 33.5%；
配额修正复验为 56.1%，仍低于初始冷态；新记录最大暂停
80.3 ms。更长同 VM 压力、更多负载、尾延迟与全机物理收益仍待验收。

最新结果见 [共享压缩与自动冷页回收报告](../review_project/03-modules/macos-memory-sharing.md)。
本文后续逐轮记录保留历史结论；当前状态以最新报告和对应原始证据为准。

## 目标与验收

1. 同宿主机、同信任域的两个 pVisor VM 复用相同 RAM 内容，保持 CPU 和设备写入隔离。
2. 驻留数据共享与压缩对象共享分别计量，不能用磁盘节约代替物理内存节约。
3. VMM 发现长时间不访问的页，保存压缩内容、回收原页，随后按需恢复；guest 无需 zram。
4. 使用 Linux guest 的两个真实 VM 验证，包含重复内容、不可压缩内容、写密集负载、设备 I/O。
5. 内存收益、吞吐与尾延迟共同决定可行方案；不能仅用合成全零负载宣称高性能。

暂定“大幅”验收为重复内容工作负载下，计入共享 backing / 解压缓存 / 压缩池的
宿主物理占用相比匹配基线至少减少 40%；阈值是工程验收目标，不是已测结果。
压缩不可压缩输入时须有原始存储回退和硬预算。测试至少重复三次，报告原始样本、
中位数与波动；完整 VM 路径需持续压力与失败注入后才可称稳定。

## 已有边界

- `ram_backing::SnapshotChain` 是不可变磁盘 generation，不是共享驻留页池。
- `MemoryGate` 排空设备引用，但不记录访问温度，也不执行写时复制。
- `ram::reclaim` 依赖共享可写文件映射；私有 COW 脏页不能按同一合同回收。
- 原 krun-hvf 将 data abort 解码为 MMIO，并安排推进 PC。现已增加可选 RAM
  fault resolver，在 MMIO 解码之前处理 data / instruction abort，恢复后重试原指令；
  未接管地址维持 MMIO 行为。resolver 默认禁用，已接入显式开启的实验冷页 pager。
- 宿主页大小按系统读取。本机为 16 KiB；存储块 64 KiB，不能把 4 KiB staging
  单位直接当作可重映射的 HVF / 宿主页。

## 路径选择

| 路径 | 实验目的 | 代价与退出条件 |
|---|---|---|
| A：共同只读文件基底 + 原生 MAP_PRIVATE COW | 验证 HVF guest 写入仍保留隔离及未写页共享 | 最少自定义异常代码；COW offload 需另写提交合同 |
| B：共享只读基底 + HVF 写保护 + 显式私有化 | 比较可控 COW 与原生 COW | CPU 写 fault 与设备写必须同一入口；异常开销可能不值得 |
| C：压缩对象池 + no-access 采样 + 冷页解映射 | 验证宿主侧压缩 RAM 与按需恢复 | 需改 HVF fault 解码及设备访问，优先正确再降低停顿 |
| D：现有 FUSE 压缩 backing + 闲置 VM 自动 offload | 对照整 VM 压缩路径的收益和复杂度 | 只能识别 VM 闲置；不能宣称实现页级访问温度 |

不先实现任意匿名页的后台 KSM 扫描。A/B/C 使用显式共有对象；有收益后再
考虑周期性去重私有页。不得通过放开共享对象写权限处理 COW。

## 所有权与布局

每个 VM 独立维护 `Guest region → host range → backing offset` 与页面状态；
共享池只负责不可变内容及引用，不持有 guest CPU / 设备对象。
内容身份为 SHA-256(域标识、未压缩长度、原始字节)；相同 hash 的候选共享
对象必须校验长度与内容。共享池的授权域先限定同一用户、同一信任域。

存储层沿用 PVZRAM 的校验与 generation；短期不把磁盘格式一起重写。
驻留压缩池保留唯一编码对象，可压缩输入以紧凑 slab/连续 arena 组织，不能
为每个 200 字节对象各分配一个宿主页。对象身份与虚拟地址分离，移动编码对象
不得使 VM 的 RAM 指针失效。首版采用单池锁和有限容量，测到竞争才拆锁。

跨进程首版由一个宿主所有者持有池，通过 Unix socket 注册对象引用并请求恢复；
runner 只持引用和私有可写页。恢复数据通过有界 RPC 传输，暂不传递 FD。
引用注册、发布和 GC 在同一所有者内序列化。断线释放连接引用；持久 pin
尚未实现。没有共享池时
保持原 backing 回退；池故障不得使已运行 VM 读取错误或零填充数据。
压缩池本身应是内存对象；磁盘 chunk cache 是独立的后备层，分别计量。

## 页状态与温度

```text
ResidentPrivate / ResidentShared
    → Sampling（guest no-access，记录采样 epoch）
    → 访问 fault：恢复权限，记录 last_access，回到 Resident
    → 冷阈值且预算允许：Evicting
    → CPU 静止 + 设备排空 + 再确认内容与引用
    → 生成/引用压缩对象 + 移除 guest 映射 + 回收宿主页
    → Compressed
    → CPU 或设备访问：Restoring
    → 校验/解压至私有页或共享只读缓存 + 安装 guest 映射
    → Resident
```

no-access 采样同时涵盖读、写和执行；只写保护不能发现只读热页。
每页保留单调时钟最后访问 epoch；不能用 `mincore` 的驻留状态判断冷页。
采样限制每轮页数、缺页率和 CPU 时间，热页设置冷却时间，连续热页退出采样。
只有页在完整观察窗口没有 CPU/设备访问，才可判为候选冷页。

设备读取在取得 guest slice 前恢复压缩页；设备写入额外私有化并标记访问。
DescriptorChain、Reader/Writer、保留 packet/TX buffer 必须涵盖这个入口。
无法拦截的内核异步访问或 passthrough 页固定驻留，不采样、不压缩。
首次实现采用静止 epoch 串行重映射；稳定宿主地址下的替换只能在所有访问者
排空且 HVF 映射解除后进行，不能沿用已有 raw pointer 的“地址没变”证明安全。

同页 fault 只允许一个恢复者，其余等待；先建好新页再发布，失败保留旧内容。
多页设备操作按固定顺序锁定或批量恢复，避免环路等待。压缩超时可终止等待，
但资源清理必须等待实际 worker 完成。任何校验、解压、配额失败都不返回伪造零页。

## Offload 和提交合同

原生 COW 不将私有修改写回基底。显式 offload 在 CPU/设备静止时捕获全部私有
修改，发布独立 generation，随后才能丢弃私有页。后台页级压缩是临时驻留策略，
不自动发布 checkpoint，也不改变 RunState。CPU/设备对象仍存活，不能由 RAM
共享推导跨节点恢复或独立 VM fork。

共享解压缓存与私有页分别回收；一个 VM offload 不应主动驱逐其他 VM 的热基底。
压缩池、解压缓存和临时双份页必须有预算及恢复预留，避免内存压力下解压死锁。

## 原生探针与当前证据

入口：`python3 tools/experiments/macos-memory/run.py`。四条路径各重复三次，共 12 组；
原始证据见 `review_project/06-evidence/macos-memory/hvf-cow-probe.json`。
C 探针直接调用系统 Hypervisor.framework，避免在证据未知时修改生产 VMM。
每轮两个独立进程各建一个 HVF VM；guest 读取每个宿主页，分别写入不同值，
检查自己的映射和共同基底字节。readonly 路径同时验证 stage-2 写 fault 与重试。
private 路径为独立匿名 backing 对照。cold 路径对数据 RAM 设置 no-access，
100 ms 观察窗口仅访问首块；其余 1023 个 64 KiB 块引用一个预编码、只读、
跨进程共享的 LZ4 内存对象，解除 HVF 映射并回收宿主页。访问末块时从这个
对象恢复内容，重新安装映射并重试原指令。此探针没有任意内容索引或设备写入。

本轮初次实验中，64 MiB 基底在两个映射者中报告相同 VM object ID；两个 guest
分别写入 100/101 后各自值正确且基底不变。writable 原生 COW 与 readonly 显式
路径均通过。这只是底层可行性证据，没有 Linux guest、virtio 或生产控制路径。

`phys_footprint` 不计入全部共享文件缓存；VM_REGION_TOP_INFO 的 shared 页统计
也可能包括 Hypervisor 引用。同 object ID 是对象共享证据，不是全机唯一物理
占用的精确测量。最终结果必须把共同驻留页计一次，并包括父进程 / 池 / 缓存；
RSS 或 footprint 相加不能单独证明“大幅降低”。

后续页级身份探针 `hvf-page-identity-probe.json` 已通过 12 组：首个共享页在
guest 写入后分离，未写尾页保持相同对象与偏移。查询使用本机 SDK 的
`mach_vm_page_info(VM_PAGE_INFO_BASIC)`。Apple XNU 源码的该查询沿 VM
object 阴影链定位页，再返回对象身份、对象内偏移和驻留 disposition；对象
身份为内核地址 hash，不是公开物理地址。[Apple XNU 实现](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/vm/vm_map.c)
这提供了页级共享识别的实测方法；仅查询首尾页，仍未覆盖全部 VM / 池
映射、系统压缩与内核开销，也不是全机原子快照。完整收益验收继续保持未通过。

## Rust 实验接口

`ram_backing::resident` 已提供 `CompressedPool`、`CompressedObject` 和
`ColdWindows`。池使用填充值、Zstd 或 raw 回退，限定 payload 与对象数量；
相同内容复用 Arc，释放最后外部引用后可收集。恢复校验长度与内容身份。
冷页候选绑定观察版本，访问或重新采样使旧候选失效；候选不授予映射回收权限。
`ram_backing::ipc` 将池放在独立宿主进程。每次 PUT 分配独立连接引用，重复
内容仍只有一份编码对象；RELEASE 或断线仅释放对应引用。输入限制为 64 KiB，
恢复验证长度与 SHA-256；协议或传输失败永久关闭会话，已完整读取的容量拒绝
保留已有引用。调用方负责端点授权、连接数与服务生命周期。

`memory_pool_case` 与 `tools/experiments/macos-memory/pool_check.py` 已验证真实
三进程路径：两个客户端各持有两份 64 KiB 引用，共享一个 271 字节编码对象；
一个客户端退出后，另一个仍能恢复；释放重复引用不影响另一引用，最后对象归零。
这是压缩 payload 与引用生命周期证据，不是完整 VM 或总物理占用的测量。
实验冷页 runner 已使用该池；单测也覆盖截断响应后的会话失效。协议更新为
PVMEMP2，STATS 增加跨连接共享对象数，不兼容旧实验二进制。

## 生产后端接口增量

现已 vendor `krun-hvf` 1.19.3，加入 `MemoryFault`、`MemoryFaultHandler`、
`Vcpus::handle_memory_fault` 和 `HvfVm::protect_memory`。默认 resolver 返回 false，
原 MMIO 路径保持；接管 RAM 后返回 `MemoryFaultHandled`，不设置 PC advance。
恢复错误进入 VMM 的失败退出路径，不将 RAM 错误当作可继续执行的 MMIO。

`VmmHandle::install_ram_fault_handler` 在运行 VM 中暂停 CPU、放开 VMM 锁排空
设备访问，再安装处理器并续跑；拒绝已暂停/offloaded VM。此 API 只注册回调，
不采样、不替换页面、不实现跨进程池。处理器必须验证 GPA 所有权，序列化同页
恢复并在返回成功前安装正确映射；更换处理器前必须移除旧采样映射。

`hvf_ram_fault_case` 使用同一 Rust backend 在真实 HVF 检查四条路径：原指令
重试、未接管的 MMIO、恢复错误、取指异常。四项均通过；不能据此声称设备
页恢复或完整 VM 实验已验收。

设备侧 `MemoryGate::set_prepare` 已提供可选 RAM 准备入口，安装要求 gate
已关闭且没有访问引用。每次 queue 访问（包括读取 descriptor table 前）取得
访问引用，再释放 gate 锁运行回调；保留的 descriptor / Reader / Writer 继续
持有引用，使采样或回收必须等待它们完成。准备回调只能在所有可能被设备
访问的页已经恢复且观察取消后返回成功。实验 pager 已连接该入口，设备访问
按 queue ring、descriptor header 和 payload 范围恢复并取消观察；空范围仍保守
恢复全部 RAM。准备 descriptor 后才解码它，再准备 payload 后才建立视图。

准备错误或 panic 会终止当前隔离 VMM 进程。现有 queue API 无法完整传播
恢复错误，继续返回普通空队列会掩盖故障并允许后续设备读缺失 RAM，因此
此实验采用 fail-stop。池失联后的可恢复降级尚未实现。默认没有准备回调。
七项 MemoryGate 定向检查已通过，覆盖回调先于 descriptor / payload 读取、
保留视图阻止回收、回调不持 gate 锁、跨 VM 隔离，以及忙设备跳过可选维护。

## 后续实施与实验矩阵

### 自动冷页与共享压缩已接入

设置宿主环境 `PVISOR_EXPERIMENTAL_MEMORY_POOL` 为同一用户私有目录下的
实验池 socket，runner 启用 `vm::pager`。服务仍由实验驱动管理，不是生产 daemon。
启动后等待 1 秒，每 250 ms 尝试暂停 CPU 并关闭空闲设备 gate。已有保留
设备引用时恢复 CPU 并跳过本轮，不等待五秒或将正常繁忙当作 VM 故障；
用户暂停的 VM 也跳过。成功关闭后按 64 KiB 块轮转采样。历史实验每轮最多
访问 256 块；当前修正为最多 256 次观察/回收操作，访问不超过一圈，
循环内采用 8 ms 软预算；单次 RPC / I/O 及统计工作仍可越过预算。观察至少
200 ms 且未被 CPU 或设备范围访问的块才保存到池。CPU 读/写/取指 fault
取消观察；设备只取消实际准备范围的观察。持续设备压力下的公平性尚需验证。

保存引用成功后才解除 GPA 映射，用 PROT_NONE 匿名映射替换原宿主页；
这两步仍在 CPU 静止、设备引用排空的事务内。恢复 CPU 后，pager 在不持
VMM / pager 锁的情况下对原文件范围执行 F_PUNCHHOLE。该范围此后不再
映射给 VM；故障恢复始终使用私有匿名 RAM，文件回收不修改恢复后的内容。
每块只安排一次原文件回收，后续匿名页回收不重复打洞。文件剩余大小不变，
但内容有洞；它不再代表可独立恢复的 RAM 快照。已有 FUSE RAM backing 和整机 offload 与此模式
互斥，避免旧提交合同读取被回收内容。恢复先在预留的 64 KiB buffer 中检查
长度和 hash，再分配私有页、复制、同步指令缓存、映射 GPA，最后释放池引用。
同块操作由 pager 锁串行化；另一个 vCPU 的过期 fault 即使发现块已驻留，也
重试原指令。池/映射恢复错误采用 fail-stop，尚无服务重启后的恢复合同。

控制回复区分完整的安全拒绝和状态不确定的失败：错误回复若带已知 Running /
Paused 状态且没有 RAM 报告，保留连接并向调用方返回拒绝；未知状态、畸形帧、
断线和超时仍取消 Attempt。冷 pager 的整机 offload 拒绝会返回已知状态。
原有 fail-stop 用例及新增拒绝后继续 pause 的用例均通过，控制模块共 12 项。

`cold_vm_check.py` 首次完整实验通过：两个 2 vCPU / 256 MiB Linux VM 分别
自动回收 266862592 / 266993664 逻辑字节，池峰值编码约 15.9 MiB；两个
连接分别观察到约 596 / 592 个被另一连接持有的对象。随后两 VM 全部冷块
恢复，64 MiB 非均匀数据和独立可变工作集校验通过，池最终归零。
每个 guest 有 2 秒静默阶段；这不是持续 I/O 或高压负载。逻辑回收包括未使用
零页，不能当成实际物理节省；全机唯一物理占用与恢复尾延迟仍未测量。
最新原始值和源码 hash 以 `cold-vm-check.json` 为准。

### 内存与延迟计量增量

新样本分开保存为 `cold-vm-check-debug.json` 和 `cold-vm-check-release.json`；
旧 `cold-vm-check.json` 保留修复前测量。runner 每次采样前后记录自身
`proc_pid_rusage` 和宿主 `host_statistics64` 原始计数，实验控制器每 50 ms
记录共享池及 SDK supervisor footprint。宿主计数是全机观测，包含其它程序，
两 VM 的宿主计数不能相加；footprint 仍不覆盖全部文件缓存。

以下为此前整机扫描、整批设备恢复的历史样本，不能代表当前实现。

| 单次双 VM 样本 | 池峰值 footprint | 每 VM 最大扫描耗时 | 最大设备整批恢复 |
|---|---:|---:|---:|
| debug，修复前 | 93.22 MiB | 4.29–4.31 s | 2.41–2.43 s |
| debug，修复后 | 22.33 MiB | 4.29–4.31 s | 2.40–2.43 s |
| release，修复后 | 21.58 MiB | 0.84–0.86 s | 0.52–0.57 s |

修复前 Zstd 返回的 Vec 长度很小但保留接近 64 KiB 的容量；编码 payload 预算
没有覆盖这部分保留空间。不可变对象改为 Box<[u8]>，丢弃多余容量，不改变
内容身份、解码校验或预算拒绝。相同 debug 对照池峰值下降约 76%，这是该
实验进程的 footprint 对照，不是全机或两 VM 总内存节省比例。

release 单块恢复最大记录为 225 / 278 μs，包含 GET、校验、分配、映射和
RELEASE；不包括等待 pager 锁，也不是 p99。扫描时间不包括 CPU 暂停与设备
排空前的等待。修复后 debug 重叠冷页阶段观测到宿主 free 页约增加 208 MiB，
release 两 VM 的冷区间不完全重叠，观测值不同。未提供匹配空闲期基线和
重复样本的噪声界限，仍不满足 40% 物理内存验收。

这些历史计量促使实现改为有界轮转采样和设备范围准备。新阶段的原始
证据及限制见下节；批量 RPC 的收益仍需由恢复尾延迟和暂停计量决定。

### 有界采样与设备范围准备

`review_project/06-evidence/macos-memory/cold-vm-bounded-range-release.json`
记录两个完整 Linux VM 的一次 release 实验，guest 静默期为 35 秒。每 VM
峰值冷页为 104.94 / 116.31 MiB；两 guest 完整内容、独立工作集和新增
32 MiB 零页写入校验通过，最终池引用归零。跨连接共享对象数峰值为 496。

| 计量 | VM 0 | VM 1 |
|---|---:|---:|
| 完整采样事务 P50 | 8.651 ms | 8.637 ms |
| 完整采样事务 P95（nearest rank） | 9.629 ms | 9.081 ms |
| 完整采样事务最大值 | 181.125 ms | 18.955 ms |
| 单块恢复最大值（不含锁等待） | 770 μs | 792 μs |
| 完成采样轮数 | 133 | 134 |

池峰值 footprint 为 17.25 MiB。完整事务计时涵盖暂停、设备排空、采样与
恢复 CPU；8 ms 是循环软预算，181 ms 最大停顿说明尚未满足严格尾延迟目标。
设备冷恢复计数为零，只能说明此负载没有迫使设备恢复冷 payload，不能
据此宣称设备恢复零开销。该样本早于忙设备跳过改进，且源码哈希清单遗漏
queue.rs；后续样本补齐此项，保留原始记录而不追补旧运行哈希。

忙设备跳过改进后的完整双 VM 复验见
`review_project/06-evidence/macos-memory/cold-vm-bounded-range-idle-release.json`。
该运行源码清单包含 queue.rs；内容与引用回收检查通过。每 VM 冷页峰值
112.44 / 113.50 MiB，完整事务 P95 为 9.304 / 10.053 ms，最大值为
21.008 / 23.448 ms，池峰值 footprint 为 17.39 MiB。此负载仍未发生
设备冷 payload 恢复；忙设备跳过语义由保留视图的定向检查证明，完整 VM
运行只证明普通负载兼容性。两次样本的最大停顿差异不能视为改动的因果收益。

此前有界采样但设备仍整批准备的失败记录保留在
`cold-vm-bounded-all-restore-release.json`：内容校验通过，但两个 VM 冷页
峰值不足 64 MiB，未通过原验收门槛。设备范围准备解决了该负载的反复
全量恢复问题。新实验静默期长于历史样本，不能把全部改善归因于代码。
全机物理内存减少 40%、匹配基线三次重复、随机数据及长期压力仍未验收。


### 匹配基线：记账指标与 RAM 驻留必须分开

`review_project/06-evidence/macos-memory/matched-vm-release.json` 保存三组
双 VM 配对实验，每组使用相同 35 秒静默负载、相同二进制与数据，交替
基线/压缩顺序。六次运行的内容与引用回收检查均通过。计量窗口统一为
启动后 25–30 秒，每 250 ms 读取两个 runner、SDK 及压缩池的进程统计。

| 配对 | 基线 footprint | 压缩 footprint（包含池） | 基线 RSS 合计 | 压缩 RSS 合计 |
|---|---:|---:|---:|---:|
| 0 | 13.58 MiB | 33.00 MiB | 347.73 MiB | 122.47 MiB |
| 1 | 13.58 MiB | 32.64 MiB | 348.34 MiB | 122.55 MiB |
| 2 | 13.67 MiB | 33.41 MiB | 349.48 MiB | 122.38 MiB |

footprint 的上升与 RSS 的下降同时存在。文件 RAM 并未全部计入 footprint，
所以池增加的记账不能当作总内存退化；RSS 包含共享库等重复驻留页，也不能
直接称为唯一物理占用。此对照明确否定使用单一进程指标验收的做法。

新增实验环境 `PVISOR_EXPERIMENTAL_MEMORY_METRICS` 使每个 runner 每秒
复用 `ram::residency`，读取自己文件 RAM 范围的 mincore 驻留标记；冷页
匿名替换仍位于这些范围内，因此热页与恢复后的页也纳入。该查询不读取
RAM 内容、不暂停 VM、不触发页恢复。读数是瞬时建议值，映射变化和访问
可能使样本波动。Apple 对其定义见 [mincore 文档](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/mincore.2.html)。

RAM 专项配对使用同一晚期窗口，并要求两个 VM 各至少三次有效读数。
报告分别保留 RAM 驻留、池 footprint 和全进程指标；RAM 驻留加池记账
仍不是全机唯一物理占用，不包含全部内核页表/文件缓存或压缩器变化。


RAM 专项三组完整复验见
`review_project/06-evidence/macos-memory/matched-ram-vm-release.json`。六次
运行全部通过 guest 内容、独立写入及引用回收检查；每个 VM 的晚期窗口
均有至少三次有效 RAM 驻留读数，池统计也在相同时间窗口取样。

| 配对 | 基线 RAM 驻留 | 压缩 RAM 驻留 | 池 footprint | 压缩 RAM + 池 | 指标降幅 |
|---|---:|---:|---:|---:|---:|
| 0 | 234.61 MiB | 44.98 MiB | 17.11 MiB | 62.09 MiB | 73.53% |
| 1 | 234.08 MiB | 44.81 MiB | 17.31 MiB | 62.13 MiB | 73.46% |
| 2 | 235.42 MiB | 51.86 MiB | 17.05 MiB | 68.91 MiB | 70.73% |

该指标降幅中位数 73.46%，三次范围 70.73–73.53%。它包含两个完整 VM
的所选 RAM 范围与中央池记账，支持此重复内容/静默负载下明显的回收净收益。
它仍不是全机唯一物理占用，不能直接替换原 40% 物理内存验收，也不支持
随机数据、持续活跃工作集或整个生产配置的收益结论。

性能计量覆盖同组两个 VM 的完整采样事务。三组 P95 分别为
9.023 / 9.099 / 9.248 ms，最大值为 245.170 / 383.917 / 17.459 ms。
低 P95 不能掩盖长停顿：每块同步 RPC、文件打洞及暂停等待均在事务路径，
当前证据尚未区分这些长尾来源。下一步分段计时定位长尾，再将已确认的
阻塞工作移出 CPU 静止区间；不能仅通过缩短循环预算宣称解决此问题。

### 长尾定位与分离文件回收

分段原始记录 `cold-vm-timing-release.json` 显示，两次 117–122 ms 的
事务长尾集中在单块 discard。进一步的 `cold-vm-discard-timing-release.json`
将最大值分解为 HVF unmap 18 μs、文件打洞 155.700 ms、宿主 remap 57 μs，
直接确认该运行的主要阻塞来自文件回收，而非暂停或压缩池。

当前实现将文件打洞移出 CPU 静止事务；块的原文件范围永久脱离 VM，
故 CPU / 设备可以同时访问恢复后的私有匿名页。尚未完成打洞的范围以
`pending_file_bytes` 保守计入专项计量，直到成功回收后才扣除。每轮完成
记录要求 pending 归零，后台下一轮采样在此轮回收结束后才开始；这里
没有额外 worker 队列或无限排队。文件回收失败仍采用明确的 fail-stop。

`cold-vm-deferred-reclaim-release.json` 的完整双 VM 复验通过原有内容、
零页写入、共享引用和回收门槛，并新增逐轮文件回收完成检查。每 VM
峰值冷块 177.31 / 178.63 MiB（包含原本未驻留的零块，不能当作物理
节约）；完整采样事务 P95 为 9.017 / 9.117 ms，最大值 15.915 / 11.778 ms。
映射 discard 最大值 108 / 87 μs；文件回收批次最大值 7.903 / 53.122 ms，
发生在恢复 CPU 后。该样本证明慢文件操作已离开暂停区间，不能据一次
运行保证任何宿主压力下的停顿上界。

三个 RAM 定向回归检查通过，新增检查用真实文件和匿名替换验证重复打洞
保持私有内容、相邻块和文件长度。standalone nextest 使用项目实际 vendor
设备/HVF patch，测试构建关闭 debug 信息以减少磁盘占用；未修改批准账本。
复验源码哈希与原始输出保留，性能/内存的三组匹配复验继续独立计量。


三组匹配复验保存于
`review_project/06-evidence/macos-memory/matched-deferred-ram-vm-release.json`。
相同 35 秒静默负载，六次运行均通过内容、独立写入和引用回收检查。
RAM 指标保守计入未完成回收的原文件范围；晚期窗口内该项的中位数为零。

| 配对 | 基线 RAM 驻留 | 压缩 RAM＋待回收文件上界＋池 | 指标降幅 | 完整事务 P95 | 完整事务最大值 |
|---|---:|---:|---:|---:|---:|
| 0 | 233.91 MiB | 53.39 MiB | 77.17% | 9.039 ms | 19.259 ms |
| 1 | 234.33 MiB | 55.72 MiB | 76.22% | 9.019 ms | 12.920 ms |
| 2 | 234.70 MiB | 55.21 MiB | 76.48% | 9.003 ms | 13.455 ms |

降幅中位数 76.48%。共 794 次完整事务，每次都找到对应文件回收完成记录，
pending 均归零。三个配对的文件回收批次最大值为 203.987 / 280.639 /
46.298 ms；它们在 CPU 恢复后执行。文件长尾仍存在，但已不再阻塞静止区间。
这些记录的源码哈希与当前实验实现一致，单独的原始 stdout / stderr 保留。

该结果证明所测重复内容/静默负载的回收收益和移出同步文件 I/O 的效果；
仍不能保证其它宿主压力下的停顿上界，或把 RAM 专项指标称为全机唯一物理
占用。尚需随机/写密集负载、设备冷 payload I/O、故障注入和长期重复运行。

### 随机数据、冷热切换与持续文件 I/O

复现时设置 `PVISOR_MEMORY_CASE=cold-stress`，其余沿用已有固件目录和
`cold_vm_check.py`。`cold-vm-stress-release.json` 保存一次完整双 VM 实验：
每 VM 两个 vCPU、64 MiB 可修改重复数据、16 MiB 独立确定性随机数据，
另有 16 MiB 验证缓冲区。三次静默窗口各 12 秒，每次完整校验上一轮内容
后重写随机/重复数据；并发线程每约 100 ms 写入、fsync、读回 64 KiB 文件。

两个 VM 分别完成 344 / 343 次文件往返，三轮 RAM 校验与最终零页写入
检查全部通过。它是持续小块 I/O，约 0.6 MiB/s，不能称为设备吞吐极限测试。

| 计量 | VM 0 | VM 1 |
|---|---:|---:|
| 峰值冷块（含未驻留零块） | 144.94 MiB | 150.25 MiB |
| 累计恢复块 | 11.94 MiB | 15.00 MiB |
| 池完整拒绝回复 | 59 | 61 |
| 完整事务 P95 | 8.832 ms | 10.036 ms |
| 完整事务最大值 | 19.826 ms | 22.216 ms |

池最大编码 payload 为 16,777,018 字节，接近 16 MiB 硬预算；峰值池
footprint 20.91 MiB，包括元数据、编码工作区和进程开销。拒绝后仍保留
原驻留内容，随后随机数据校验通过。此样本验证容量背压与内容隔离，
不证明不可压缩私有数据有净内存收益，也不等同宿主物理内存压力验收。
本次设备冷恢复计数为零，故另行验证真实设备冷 payload 路径。

### 真实 virtio-fs 冷 payload 恢复

复现设置 `PVISOR_MEMORY_CASE=cold-device`。guest 在 35 秒静默后通过
Linux `vmsplice/splice` 把用户页送入文件写入路径，选取分布在 64 MiB
数据中的 63 个 64 KiB 范围，随后逐字节读回比较。没有设置
SPLICE_F_GIFT，原页始终由 guest 缓冲区拥有；内核仍可能复制数据，
不能把该调用路径自动称为零拷贝。[Linux vmsplice 文档](https://man7.org/linux/man-pages/man2/vmsplice.2.html)

两个失败记录保留：`cold-vm-device-unaligned-release.json` 与
`cold-vm-device-direct-open-release.json`。后者通过分段错误确认，失败
发生在 O_DIRECT 文件打开，返回 EINVAL，并非已证明的页恢复错误。
正常文件模式的第一次成功记录为 `cold-vm-device-splice-first-release.json`。

增加独立设备恢复计量后的 `cold-vm-device-release.json` 再次通过：两个
VM 的设备路径各恢复一个 64 KiB 冷块，较长单范围 payload 准备计数也
各为 64 KiB。该计数由同一 pager 锁内的恢复字节差计算；queue ring 使用
三个范围，descriptor header 使用单个 16 字节范围，因此这里保守地只计
大于 16 字节的单范围恢复。它计量恢复块大小，不是精确 I/O 字节数或物理页数。

设备恢复最大耗时 150 / 143 μs；完整采样事务 P95 为 9.018 / 9.079 ms，
最大值 13.456 / 16.125 ms。两个 guest 均通过文件读回、完整 RAM 和零页
写入检查，池最终引用归零。脚本要求每 VM 设备恢复及 payload 恢复非零，
不会仅因文件内容正确就记为此路径通过。

设备 gate 的七项定向检查也通过。descriptor / payload 顺序检查现在先把
两个独立宿主页置为 PROT_NONE，再由准备回调分别恢复权限和内容；读取若
早于准备就会故障。它补强了原先仅使用零填充页的检查，保留 Reader 仍阻止回收。
这些证据覆盖真实 virtio-fs 的一次冷恢复与一般队列顺序，net / vsock 压力、
长期反复映射仍待验收。

### 共享池退出与故障隔离

`pool_loss_vm_check.py` 的真实 VM 故障注入已通过：两台连接池的 VM 冷块
峰值为 97.75 / 88.25 MiB，另外两台独立 VM 同时运行。强制结束实验池后，
受影响 VM 在约 0.369 秒内均报告 Failed / exit 1，日志明确记录 Broken pipe。
两个独立 VM 的内容、暂停/恢复检查通过，四个 runner 均收尾退出。
证据保存于 `pool-loss-vm-release.json`，包含原始输出与运行时源码哈希。

当前合同是池丢失后明确停止受影响 VM，不返回伪造内容。它尚不提供池重启
恢复或高可用；此注入由维护线程检测断线，也不覆盖全部 CPU / device fault
交错。将共享池视为临时 RAM 的唯一所有者时，这种故障会丢失运行状态。
正式启用前须明确接受此可用性边界，或实现可恢复的 backing 所有权。

网络设备的代码边界也已复核：net TX / RX 经 queue descriptor 入口准备
header / payload，vsock packet 保留 descriptor 的访问 lease。vsock 的
packet-retention 定向检查通过，descriptor 释放后 packet 仍阻止回收，
写入与最终读回正确。该检查不覆盖网络压力或 socket 生命周期的全部交错。

后续真实网络实验复用 pVisor 的 virtio-net / smoltcp 数据面，TSI 保持关闭。
宿主 fixture `network_fixture.py` 仅绑定 `127.0.0.1` 随机端口，接受两个
独立 TCP 流，以 64 KiB 有界缓冲回显；连接有超时，结束后关闭 listener。
`--self-check` 的两个本机客户端各校验 1 MiB 数据已通过，尚未经过 VM。
VM 侧须配置仅授权该 hostname / port 的规则，并通过合成 DNS 到宿主
fixture；不能使用 guest 的 `127.0.0.1` loopback 作为设备路径证据。
后续验收要求传输内容、独立 VM 状态、页恢复、容量 / 引用和收尾共同通过，
host fixture PASS 本身不计入 net / vsock 的 VM 验收。

首次真实双 VM TCP 实验 `cold-vm-net-single-burst-release.json` 已通过。
SDK 显式选择 Auto / smoltcp，仅授权 `localhost` 的 fixture 端口及 TCP，
允许该 hostname 解析到宿主 loopback。guest 经合成 DNS 获取路由地址，
没有使用 guest loopback 或启用 TSI。两台 VM 在 35 秒静默后各发送并
逐字节校验 32 MiB，packet 内包含独立 VM 身份和序号；宿主两个连接的
计量均恰为 32 MiB，fixture 正常退出。完整 RAM、零页写入与引用回收
检查通过，驱动也明确拒绝 stdout / stderr 被截断的证据。

运行共 37.06 秒；两个 guest 的传输段为 672.353 / 537.608 ms，包含
用户拷贝、TCP 往返及冷恢复，不含初始连接和静默，不是独立网络基准。
设备路径累计恢复 2.69 / 2.94 MiB，最大准备耗时 307 / 222 μs，事务
P95 9.076 / 9.132 ms，最大 21.072 / 19.434 ms。设备统计汇总所有 queue
准备，不能把这些恢复字节全部归因于 net；virtio-fs 和其它队列也在运行。
后续十轮实验每个 burst 32 MiB、间隔静默 12 秒，另存独立证据，不把
一次快速传输称为持续网络压力验收。

十轮真实网络复验 `cold-vm-net-10-rounds-release.json` 已通过，运行
151.23 秒。两个 VM 的全部 20 个 burst 校验正确，每台累计 320 MiB，
宿主两个连接均记录恰好 335,544,320 字节并正常退出。完整 RAM、最终
零页写入、共享引用回收和逐轮文件回收均通过，源码哈希已核验。

| 十轮网络负载 | VM 100 | VM 200 |
|---|---:|---:|
| 单 burst 传输段中位数 | 725.757 ms | 626.547 ms |
| 单 burst 传输段最大值 | 840.554 ms | 823.194 ms |
| 累计恢复逻辑 RAM | 515.63 MiB | 495.06 MiB |
| 累计设备恢复块大小 | 17.19 MiB | 16.63 MiB |
| 设备准备最大耗时 | 899 μs | 593 μs |
| 采样事务 P95 | 9.183 ms | 9.144 ms |
| 采样事务最大值 | 19.287 ms | 17.407 ms |

此负载未遇到容量拒绝，池 footprint 峰值 23.30 MiB。恢复累计量包含
重复恢复和所有设备队列，不能算物理节约或仅网卡恢复量。传输段包含
用户拷贝、TCP 往返与冷恢复，间隔静默不在 burst 计时内；它是数据
一致性与反复冷恢复检查，尚无同负载关闭 pager 的性能基线，不能由此
宣称吞吐无回归或极限网络压力通过。原生 vsock 与其它设备仍需覆盖。

### 网络同负载基线

`matched_vm_check.py` 设置 `PVISOR_MEMORY_CASE=cold-net`，开启与关闭 pager
各运行三次，每次两台 VM、每 VM 三个 32 MiB burst，初始静默 35 秒，
burst 间静默 12 秒。按 baseline/cold、cold/baseline、baseline/cold 顺序
交替，授权与 guest / fixture 程序相同，关闭模式也保留 RAM 计量。
证据为 `matched-net-3-rounds-vm-release.json`。六次运行的全部 36 个 burst
内容、宿主累计字节、RAM / 零页、引用与 runner 退出检查通过；冷模式
采样 / 完整事务 / 文件回收记录数量一致，pending 均归零，运行源码哈希
已核验。没有修改任何 semspec 人工批准账本。

| 配对 | baseline 静默 RAM 加池 | cold 静默 RAM 加池 | 代理降幅 | baseline burst 中位数 | cold burst 中位数 |
|---|---:|---:|---:|---:|---:|
| 0 | 234.47 MiB | 55.16 MiB | 76.48% | 1,192.425 ms | 760.410 ms |
| 1 | 234.97 MiB | 54.59 MiB | 76.77% | 1,229.718 ms | 659.669 ms |
| 2 | 233.45 MiB | 54.67 MiB | 76.58% | 1,299.480 ms | 511.850 ms |

静默窗口仍为启动后 25–30 秒，按各 VM RAM 驻留与待回收文件上界的
中位数相加，再加池 footprint 中位数；代理降幅中位数 76.58%，不能
改称全机唯一物理节约。整个 fixture 是共同的工作负载端点，不计入 RAM
代理；全进程 footprint 结果也保留，不能用它替代文件 RAM 计量。

本配对中 cold / baseline burst 中位耗时比为 0.638 / 0.536 / 0.394，
未出现更慢的中位数，但这不是普遍加速承诺。传输段包含用户拷贝、冷
恢复、双 VM 竞争与 TCP 往返，fixture 使用默认 TCP socket 行为，且宿主
为活跃桌面；尚未隔离 TCP 行为、文件 backing 或其它影响时间差的因素。
首次连接和静默不在 burst 计时内，不能按此推导启动延迟、极限吞吐或
其它工作负载的性能。正式性能验收仍需处理这些边界。

### 五十轮冷热切换

设置 `PVISOR_MEMORY_CASE=cold-stress PVISOR_MEMORY_STRESS_ROUNDS=50`，
同一双 VM 工作负载运行 611.28 秒，通过全部 100 次重复 / 独立随机 RAM
检查，以及两台 VM 的 5,713 / 5,715 次 64 KiB 文件写入、fsync、读回与
最终零页写入检查。证据为 `cold-vm-stress-50-rounds-release.json`，原始
stderr 约 5.96 MB，运行时源码哈希已核验。池最终无引用，文件回收 pending
逐轮归零。

完整事务 P95 为 9.288 / 9.313 ms，最大值 34.292 / 29.071 ms；单块恢复
累计最大值为 1.055 / 1.045 ms。首 200 次与末 200 次事务 P95 分别为
8.960 / 9.087 ms 和 9.352 / 9.424 ms，没有表现出明显的逐轮延迟增长。
池 footprint 峰值 22.25 MiB，20–80 秒窗口中位数 18.31 MiB，末 60 秒
15.73 MiB。该样本没有持续内存增长迹象，不是所有泄漏或稳定性问题的排除证明。

容量完整拒绝累计 9,442 / 8,916 次，内容仍正确，但高频重试会消耗编码与
RPC 成本；需继续评估拒绝后的采样退避。约 0.6 MiB/s 文件 I/O 与十分钟
运行不代表吞吐极限、宿主内存压力或长期生产稳定性，不能据此补齐全机物理
收益验收。

### 容量拒绝后的有界退避

已增加 `Deferred(deadline)` 页状态。PUT 完整拒绝且连接仍健康时，恢复
guest 访问权限并保留原 backing，30 秒内不再对此块做观察或压缩提交。
CPU / 设备访问直接使用原内容，不依赖共享池。到期后重新启用 no-access
观察并记录新的时间窗口；只有新的冷窗口满足条件才再次提交，不沿用拒绝
之前的冷判断。固定 30 秒策略可能延迟新释放容量的利用；先验证收益，再
考虑容量代际或动态退避。

首次复验 `cold-vm-stress-backoff-enospc-release.json` 失败，guest 在文件 I/O
阶段返回 StorageFull；它不能算退避成功。随后只清理本次实验生成的
standalone vendor 测试构建产物约 257 MB，保留原有项目缓存和失败证据。

十轮复验 `cold-vm-stress-10-rounds-backoff-release.json` 通过，运行 123.21 秒。
两个 VM 完成 20 次 RAM 检查，各完成 1,144 次文件 I/O，最终内容、零页
写入与引用回收检查通过。容量拒绝为 1,517 / 1,398 次，到期重新观察计数
为 730 / 728；该记录中字段 `deferred_retries` 实际计量重新观察次数，后续
已更名为 `deferred_rearms`，不应解释为 PUT 成功次数。

事务 P95 为 9.079 / 9.130 ms，最大值 12.989 / 20.632 ms，单块恢复最大
0.797 / 0.798 ms；池 footprint 峰值 25.66 MiB。十轮与先前五十轮的负载
时长不同，不能用这些值直接宣称性能或内存改善。

五十轮退避复验 `cold-vm-stress-50-rounds-backoff-release.json` 已通过，运行
611.80 秒，两个 VM 合计完成 100 次 RAM 校验，分别完成 5,738 / 5,736 次
文件 I/O；最终零页写入、引用回收和逐轮文件回收检查通过。运行时源码
哈希已核验，guest 与驱动源码和上一轮五十轮记录一致。

| 同样五十轮负载 | 无退避 | 固定 30 秒退避 |
|---|---:|---:|
| 两 VM 容量拒绝合计 | 18,358 | 13,371 |
| 两 VM 采样事务耗时合计 | 29,027.92 ms | 28,060.41 ms |
| 事务 P95（两个 VM） | 9.288 / 9.313 ms | 9.265 / 9.204 ms |
| 事务最大值（两个 VM） | 34.292 / 29.071 ms | 21.506 / 17.323 ms |
| 20–600 秒 RAM 加池代理指标 | 227.87 MiB | 225.88 MiB |
| 池 footprint 峰值 | 22.25 MiB | 25.28 MiB |

RAM 代理按每 PID 的 `resident_bytes + pending_file_bytes` 窗口中位数相加，
再加同窗口池 footprint 中位数；它不是同步快照或全机唯一物理计量。
退避到期重新观察计数为 6,039 / 6,533，说明没有永久停留在 Deferred。

本配对拒绝次数减少约 27.2%，采样事务耗时合计减少约 3.3%，RAM 代理
基本持平；池峰值却增加约 13.6%。末段一次 runner footprint 较高的现场
采样未代表整段趋势。延迟运行于活跃桌面环境，仅一个配对，不能把最大
延迟下降全归因于退避。保留此简单策略是为限制重复提交成本，不宣称它
改善物理内存收益；进一步变化需经过相同负载复验。

### 完整 VM 对照已建立

`vm_memory_pair_case` 使用两个独立 SDK / runner、每 VM 两个 vCPU 与 256 MiB
RAM。静态 musl Rust guest 保留 64 MiB 非均匀重复数据和独立可变工作集；
暂停一个 VM 时另一个持续写 heartbeat，恢复后双方校验全部数据。
另一路对两个 VM 交替执行三轮现有整机文件 RAM offload / resume，共六次。
两条路径均通过；没有启用 macFUSE、共享压缩或自动冷页策略。

入口 `tools/experiments/macos-memory/full_vm_check.py` 构建并签名驱动，编译
静态 guest，保存日志及源码 hash。需要显式 `PVISOR_CASE_VM_LIBRARY_DIR` 指向
已有固件。首轮 offload 前两 VM 的 `mincore` 驻留量均约 117 MiB，
回收后均为零；这只描述各映射的驻留采样，不表示全机物理占用归零。
运行耗时和驻留原始数值保存在实验 JSON；耗时包含启动和退出，不能解读为
单页恢复延迟或吞吐成绩。

该工作负载作为 pager 的内容一致性对照。冷页实验已经验证保存、回收与恢复，
随机数据、多轮观察、小块 I/O 和池退出已有定向证据；密集 I/O 与长期压力
尚未验收。暂停接口本身仍不提供按页保证。

### 剩余实施

1. 原生 C / Rust 探针和 Rust 池已实现，读/写/执行 fault、重试、容量与引用
   已有定向证据；继续扩充边界和故障交错覆盖。
2. 页面状态、no-access 温度观察、宿主冷压缩与按需恢复已连接真实 VM；
   扩展反复重映射的运行时长，检查延迟与内存是否随轮数增长。
3. CPU / 设备静止事务已接入；文件打洞移出 CPU 停顿，真实池退出已有
   失败隔离证据。补齐恢复时故障与池可用性合同。
4. 设备范围准备与真实 virtio-fs 冷 payload 恢复已验证；继续审计 net / vsock
   等全部可启用设备，并增加压力与异常生命周期覆盖。
5. 实验环境开关和双 VM SDK 驱动已提供，生产默认不开启；评估正式配置合同。
6. 对比普通文件、现有 FUSE 压缩、共享 COW、共享冷压缩四条路径，包含
   相同/不同 rootfs、相同/随机 guest RAM、工作集大小与设备负载。
7. 根据真实内存与延迟数据保留有效路径，删掉没有净收益的复杂方案；更新评审。

完整目标仍未完成，不能把原生探针 PASS 记为 pVisor 共享压缩 / 自动冷页 PASS。

## 定向压缩池回收

IPC `RELEASE` 原先在丢弃一条引用后扫描全部压缩对象。改为按内容身份查询
并回收：只有池自身持有最后一个 `Arc` 时才删除对象，其他 session 或同 session
的重复引用继续保留。单次正常释放由 O(N) 扫描降为 BTreeMap 的 O(log N)
查询/删除；断线批量清理和容量不足时仍保留原来的全池回收。

[三次微基准](../review_project/06-evidence/macos-memory/targeted-pool-collection.json) 同时检查 pinned 对象、重复引用、
未知身份、对象计数与编码字节归零。使用不同的 16 字节内容保留对象，
每轮先测定向回收，再测全池扫描；debug 模式每组 256 次回收的总耗时中位数：

| 池内保留对象 | 原全池扫描 | 定向回收 |
|---|---:|---:|
| 1 | 72 µs | 61 µs |
| 256 | 2,605 µs | 124 µs |
| 8,192 | 78,142 µs | 179 µs |

这是进程内回收算法的微基准；不包含 socket、解压、映射、vCPU 调度，
不能把比值直接当成真实 VM 加速。resident / IPC 四项 nextest 检查通过
（run ID `793e2722-8fa0-4576-ad2f-e8c42f782487`）。

[定向回收后的双 VM 复验](../review_project/06-evidence/macos-memory/cold-vm-net-3-rounds-targeted-collection-release.json)
在 63.60 秒内完成各三次、每次 32 MiB 的 TCP 内容往返；每个 VM 共 96 MiB，
宿主 fixture 字节数一致。最终 RAM/零页、跨 session 共享、设备触发恢复、
引用清空、采样与文件回收记录匹配及 pending 归零检查通过，两个 runner
已退出，源码哈希核验一致。完整事务最大值为 31.685 / 32.052 ms；这次运行
验证改动后的内容与生命周期，没有配对基线，不能证明 VM 尾延迟改善。
全机唯一物理计量与宿主内存压力仍未验收。

## 完整目标映射的页身份查询

[完整页查询结果](../review_project/06-evidence/macos-memory/hvf-full-page-inventory-probe.json) 覆盖四条原生路径各三次，
并再次通过四个 Rust HVF decoder 检查。每个 checkpoint 都遍历两个进程各自
64 MiB RAM 的全部 4,096 页和 64 KiB 压缩池映射的全部 4 页；保留每页的
object ID、对象内 offset 和 disposition。两个 guest 在 checkpoint 停止执行，
查询没有读取 RAM 内容来强制 fault-in。

| 路径/阶段 | 每个进程 RAM 驻留页 | 两进程 RAM 页键交集 | 两进程 RAM 页键并集 |
|---|---:|---:|---:|
| 独立匿名 RAM，guest 读取后 | 4,096 / 4,096 | 0 | 8,192 |
| 共享基底，guest 读取后（原生 COW / 显式写保护） | 4,096 / 4,096 | 4,096 | 4,096 |
| 共享基底，各自写入第一页后 | 4,096 / 4,096 | 4,095 | 4,097 |
| 冷页回收后，保留每 VM 一个 64 KiB 热块 | 4 / 4 | 0 | 8 |
| 每 VM 再恢复一个 64 KiB 冷块后 | 8 / 8 | 0 | 16 |

宿主页为 16 KiB；各阶段压缩池驻留页键并集与交集均为 1。
所有被计入的页带 PRESENT 且没有 FICTITIOUS；本组 RAM PAGED_OUT 计数为零。
标志定义来自 [Apple XNU](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/mach/vm_statistics.h)。
脚本保留完整遍历、共享/私有身份、COW 分离、回收/恢复页数的断言，
证据记录 C、脚本与 Rust decoder 源码哈希。旧首尾页探针证据仍保留。

这补强了共享覆盖范围与回收行为的证据。页键是内核报告的 VM 对象身份和
offset，不是物理页号；它们不能证明全机唯一物理占用。结果仅覆盖原生
探针的目标映射，不含未映射文件 backing、进程堆、其它共享区、内核或
系统压缩开销，跨进程也不是全机原子快照。完整 pVisor VM 的页查询与
计入压缩池/缓存的物理收益验收仍未完成；原生冷路径的合成内容也不能
代替真实 VM 的匹配基线、压力和尾延迟检查。

## 真实 VM 的完整 RAM 查询接口

新增默认关闭的 `Vmm::experimental_ram_page_inventory()`：要求健康暂停的
VM 且设备 MemoryGate 已关闭并空闲，沿 RAM region 查询每个宿主页。
仅包括带 RAM file offset 的 region（含冷页恢复后的原地址），排除 DAX。
返回宿主页大小及 `[guest_address, object_id, object_offset, disposition]`；
采用本机 SDK `VM_PAGE_INFO_BASIC` 的 32 字节/8-word ABI，查询不读取 RAM
来强制驻留。调用方仍须处理查询期间宿主内核的驻留变化。

runner 环境变量 `PVISOR_EXPERIMENTAL_MEMORY_PAGE_INVENTORY=<私有目录>`
开启一次性诊断。启动后 25 秒尝试暂停 CPU、关闭设备 gate、查询 RAM；
设备忙则每 250 ms 重试，最多 20 次。恢复运行后写出 `<pid>.json`，
文件必须新建且权限为 0600，目录必须属于当前用户且没有组/其他用户权限。
查询错误通过嵌套结果返回，不因只读诊断失败把正常 VM 标为映射失败；
暂停/恢复失败仍遵守已有控制失败策略。环境变量未设置时没有查询线程。

完整遍历会延长诊断停顿，临时结果也占用 runner 堆内存。开启诊断的运行
用于确认映射覆盖与内容一致性，不能混入正常 pager 吞吐/尾延迟基准。
私有目录记录仅含目标 guest 地址和内核对象身份，没有读取其他进程内容。
`matched_vm_check.py` 的 `PVISOR_MEMORY_PAGE_INVENTORY=1` 会为每次双 VM
运行创建独立目录，检查每个 VM 的全部 256 MiB RAM、页地址唯一性/对齐、
时间窗口、文件回收 pending 归零，并保存所有原始页记录。

## 完整 pVisor VM 的页查询对照

[三组完整 RAM 查询对照](../review_project/06-evidence/macos-memory/matched-deferred-ram-vm-release-page-inventory.json)
通过六次双 VM 运行。每个 VM 查询全部 256 MiB RAM（16,384 个 16 KiB 页），
共保留 196,608 条原始页记录。每次查询均在 CPU/设备暂停期间进行；
查询 pid 与驻留采样的 runner pid 相符，源码哈希已核验。最终内容/零页、
跨 session 共享、引用归零、采样/事务/文件回收记录匹配、pending 归零及
runner 退出检查全部通过。基线/冷模式顺序交替，guest 程序相同。

| 配对 | 基线 RAM 页键并集折算 | 冷模式 RAM 页键并集折算 | 池 footprint 中位数 | 冷 RAM 页键＋池代理 | 代理降幅 |
|---|---:|---:|---:|---:|---:|
| 0 | 234.594 MiB | 36.828 MiB | 17.704 MiB | 54.532 MiB | 76.75% |
| 1 | 235.547 MiB | 36.172 MiB | 17.422 MiB | 53.594 MiB | 77.25% |
| 2 | 234.484 MiB | 38.172 MiB | 16.922 MiB | 55.094 MiB | 76.50% |

代理降幅中位数 76.75%。计入的 RAM 页为 PRESENT 且非 FICTITIOUS；
全部查询的 RAM PAGED_OUT 计数为零，查询时待回收文件字节上界为零。
实际 pVisor 的两个 RAM 映射没有共同驻留页键，符合目前各 VM 保留私有
热页/恢复页、仅将不可变压缩对象集中共享的设计。共享压缩证据来自同轮
pool 的跨 session 引用，而不是把 RAM 直接映射成可写共享页。

十二次查询事务耗时 10.048–28.874 ms。这是刻意开启的完整遍历诊断，
临时 Vec/JSON 也增加了 runner 堆开销；这些运行不用于正常路径的性能验收。
页键按内核 VM object/offset 去重，仍不是 PFN；池采用另一种 footprint
计量且时间窗口不原子同步。其它 runner/SDK 内存、未映射 backing、内核
以及系统共享/压缩开销没有全部纳入，所以表格仍明确标为代理，
`global_physical_memory_reduction_verified` 保持 false。
完整映射覆盖已有证据，全机唯一物理计量和宿主压力仍待验收。

## 共享池停止响应：失败复现与绝对期限修复

[修复前故障记录](../review_project/06-evidence/macos-memory/pool-stall-unbounded-io-vm-release.json) 保留真实失败：
通过 SIGSTOP 停止本次创建的共享池，两个已持有冷页的 VM 未能在驱动的
10 秒窗口内明确失败，最后被取消；两个不使用该池的 VM 内容/暂停/恢复
正常。`checks_passed=false`，没有把 Cancelled 当成预期 Failed。
逐次 socket 读写超时不能限定整笔请求的等待，部分 I/O 和零星回复可扩展总时长。

`PoolClient` 现使用独占非阻塞 socket 和 `poll`。握手与每次 exchange 各自
获得一个绝对截止时刻；部分读写、EINTR、WouldBlock 重试沿用原截止时刻，
不会刷新预算。解析/校验结束后再次检查期限；超时或破损回复关闭会话，
完整且及时的容量拒绝仍保留已有引用。没有修改协议帧或给静默 VM 添加
服务端引用过期。期限不是硬实时保证，唤醒与调度仍可产生少量超出。

新增 200 ms 期限、每 40 ms 才发一个字节的 STATS 回复检查，约 201.371 ms
后得到 TimedOut，且客户端不能再使用该会话。它与引用/容量/断线回收及
破损回复检查共同通过（三项 nextest，run ID
`235722a7-3ba3-479c-a1d9-736b7d5a872a`）。

[修复后真实 VM 故障复验](../review_project/06-evidence/macos-memory/pool-stall-vm-release.json) 通过：池保持停止且
存活直到两个依赖 VM 退出；两个 VM 均明确 Failed/exit 1，stderr 报告
`pool exchange deadline exceeded`。从停止池到驱动完成检测为 5.338 秒。
另两个独立 VM 内容与暂停/恢复通过，全部 runner 无残留。随后恢复池，
池因已断开的请求退出并释放进程资源。本结果证明有限等待与故障隔离，
不等同共享池高可用或 VM 在池故障后的继续执行。

本轮宿主预检：24 GiB 内存，已有约 4.2 GiB 交换数据，磁盘可用约
457 MiB。没有在该余量下施加会显著增加交换的全机内存压力；停止池实验
也不冒充宿主压力验收。最终唯一物理收益与真实宿主压力检查仍待完成。

[绝对期限修复后的正常负载复验](../review_project/06-evidence/macos-memory/cold-vm-net-3-rounds-absolute-deadline-release.json)
在 61.98 秒内通过两个 VM 各三次 32 MiB TCP 往返（各 96 MiB）。
fixture 字节数一致，完整 RAM/零页、设备触发恢复、共享引用及采样/文件
回收检查通过；源码哈希核验一致，两个 runner 已退出。该检查没有配对
性能基线，不据此宣称吞吐或尾延迟改善。

## TCP_NODELAY 与绝对期限后的网络配对对照

[三组网络对照](../review_project/06-evidence/macos-memory/matched-net-3-rounds-vm-release-nodelay-absolute-deadline.json)
通过六次双 VM 运行，共 36 个 32 MiB burst（1,152 MiB）。host echo 明确
启用 TCP_NODELAY，guest 程序相同、运行顺序交替，使用绝对 exchange 期限
修复后的同一 runner/pool 构建。全部内容、fixture 字节数、RAM/零页、引用
清空及 runner 退出检查通过；冷模式采样/事务/文件回收记录匹配，pending
归零。运行源码哈希和各 fixture 的 TCP_NODELAY 标记已核验。

| 配对 | 静默 RAM＋池代理降幅 | baseline burst 中位数 | cold burst 中位数 | cold/base 时间比 | 完整事务 P95 | 完整事务最大值 |
|---|---:|---:|---:|---:|---:|---:|
| 0 | 76.20% | 932.531 ms | 740.357 ms | 0.794 | 9.050 ms | 24.505 ms |
| 1 | 76.33% | 1,141.895 ms | 703.492 ms | 0.616 | 9.047 ms | 34.242 ms |
| 2 | 76.49% | 1,398.535 ms | 671.750 ms | 0.480 | 9.048 ms | 11.649 ms |

RAM＋池代理降幅中位数为 76.33%，burst 时间比中位数为 0.616。本组没有
观察到 burst 中位耗时退化，关闭 host echo 的延迟合并后结论仍如此；
但这不能把耗时差异全部归因于去重/压缩。宿主上游 TCP、复制/代理路径、
文件 backing 与私有匿名恢复页的区别、两 VM 竞争与活动桌面仍影响结果。
burst 计时包括 guest 复制与同步往返，排除首次连接和轮间 12 秒静默。
每种模式每个配对只有六个 burst 样本，不能证明极限吞吐或网络尾延迟上界。

完整事务 P95 按每个配对两个 cold VM 的全部事务使用 nearest-rank 计算，
样本数分别 464 / 461 / 463；文件 punch 在事务之外，不能把此数当作物理
回收完成时延。静默内存代理仍是 RAM 驻留＋待回收文件上界＋pool footprint，
没有计入共同 fixture，也不是全机唯一物理计量。没有打开页查询诊断线程。
旧 TCP 默认行为的证据保留原路径与源码哈希，没有被本次结果覆盖。

真实宿主内存压力验证仍需足够的交换磁盘余量；已请求先腾出空间或明确
可清理的构建缓存目录，尚未自行删除现有项目构建缓存。

## 宿主原生压力实验入口与当前预检

新增 `tools/experiments/macos-memory/pressure_vm_check.py`，复用本机
`memory_pressure -l warn -s 1` 施加真实压力，不使用 `-S` 模拟通知。
使用 `kern.memorystatus_vm_pressure_level` 的 dispatch 级别（本机 NORMAL=1、
WARN=2、CRITICAL=4），而不是把 free percentage 当作物理可分配预算。
导出与内部级别的转换可核对 [Apple XNU 实现](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_memorystatus_notify.c)。
该 sysctl 是私有接口；不存在、无法读取或返回未知级别均不能获得验收 PASS。

入口采用以下实验边界，数值是保守的工程限制，尚未在真实压力运行中校准：

| 边界 | 行为 |
|---|---|
| 启动时交换卷空闲不足 8 GiB，或宿主非 NORMAL | 不启动压力工具，也不启动 VM 实验 |
| 交换卷空闲降至 4 GiB 以下 | 结束本次创建的压力工具 |
| 相比预检新增交换达到 2 GiB | 结束压力工具，验收失败 |
| 压力工具 RSS/footprint 较大者达到宿主 RAM 的 1/3 或 8 GiB 中较小者 | 结束压力工具，验收失败 |
| 进入 CRITICAL、统计接口错误、压力工具异常退出 | 结束压力工具，保留失败记录 |
| 45 秒未观察到 WARN | 不启动 VM 实验，释放压力工具 |
| 压力持有超过 480 秒 | 结束压力工具，验收失败 |

watchdog 每次统计后约等待 250 ms，结束时只释放自己的原生压力进程，
让既有 VM 验证和清理完成。限制是采样边界，不是硬分配上限或硬实时保护；
原生工具批次分配、系统调用和调度可能使采样滞后。需要严格分配上限时
应换用容量受限的 allocator；本入口尚未进行大规模分配验证。
原生工具的输出、采样、退出原因及子实验文件链接均保留。

达到 WARN 后复用三组双 VM 网络基线/冷模式检查。每次运行保存宿主
`started_ns`/`finished_ns`；从启动后 25 秒至结束，要求至少 10 个 WARN
样本且 WARN 样本占比不少于 80%。不足则不计压力验收通过，即使内容检查
通过。这是 VM 活跃时间段的采样覆盖，不证明每个网络包或每次 RAM 访问
都在 WARN 下执行，也不是极限压力或全机物理计量。

运行方式（使用既有 firmware 环境变量）：

```sh
python3 tools/experiments/macos-memory/pressure_vm_check.py --self-check
python3 tools/experiments/macos-memory/pressure_vm_check.py --preflight
# 达到准入条件后才会启动真实压力与六次双 VM 运行：
PVISOR_CASE_VM_LIBRARY_DIR=/path/to/existing/firmware \
  python3 tools/experiments/macos-memory/pressure_vm_check.py
```

边界判断及原生 **仅等待、不分配、不模拟通知** 进程的结束/回收自检通过。
[当前预检](../review_project/06-evidence/macos-memory/host-pressure-preflight.json) 的磁盘余量为
438.21 MiB，交换已使用
4.098 GiB，级别 NORMAL；准入被拒绝。
证据明确 `native_pressure_started=false`、`vm_experiment_started=false`、
`pressure_validation_passed=false`，源码哈希已核验。
因此目前只证明自检与准入拒绝，不证明大规模压力下的 watchdog、VM 稳定性
或性能。真实压力与全机唯一物理验收仍未完成，等待磁盘余量或明确的缓存
清理授权后继续。

## CPU 校验/修改阶段的配对对照

[三组 CPU/RAM 对照](../review_project/06-evidence/macos-memory/matched-stress-3-rounds-vm-release.json) 通过六次双 VM
运行。每个 guest 三轮，每轮静默 12 秒后校验并修改 64 MiB 确定性数据、
重新生成/校验 16 MiB 随机数据，另保留 16 MiB expected，并同时持续
64 KiB 文件写入/fsync/读回。新增 cold-stress-baseline 使用相同 guest
和 driver，无共享池/pager；运行顺序交替。每个配对每种模式六个 body 样本。

body 计时从静默结束至校验/修改完成，排除 12 秒 sleep，包含冷页恢复、
guest 调度和同时进行的小块 I/O 竞争，并非纯 CPU 使用时间。共 36 轮
内容校验、最终 RAM/零页、文件 I/O、引用清空和 runner 退出检查通过；
冷模式采样/完整事务/文件回收记录匹配，pending 归零，源码哈希已核验。

| 配对 | baseline body 中位数 | cold body 中位数 | cold/base 时间比 | 静默 RAM＋池代理降幅 | 完整事务 P95 | 完整事务最大值 |
|---|---:|---:|---:|---:|---:|---:|
| 0 | 139.874 ms | 150.110 ms | 1.073 | 17.31% | 8.867 ms | 15.476 ms |
| 1 | 137.876 ms | 150.465 ms | 1.091 | 16.76% | 8.842 ms | 13.636 ms |
| 2 | 136.378 ms | 141.713 ms | 1.039 | 16.84% | 8.890 ms | 13.538 ms |

时间比中位数 1.073，即 body 中位耗时增加 7.32%；内存代理降幅中位数
16.84%。该负载同时保留随机副本且周期性全量写入，不能像重复内容静默
负载一样取得约 76% 的代理降幅，也不能从网络结果推断 CPU 恢复零成本。
这些结果支持对所测负载的代价/收益判断；不证明全机物理收益、极端 CPU
尾延迟、宿主压力或任意 workload 的高性能。是否采用这条路径应同时看
状态重复度、静默时间、恢复频率与可接受的读写延迟。

本次新增时间字段前的长周期 stress 证据保留原 hash。新数据的通用 scope
文案原先误写 35 秒，已按实际 stress 参数修正为每轮 12 秒；原文与执行
源码哈希在 JSON 中保留，并记录 metadata correction。runner 后续也已
修正该文案；没有改变本次执行的 guest、计时、内容检查或任何验收条件。

## 页身份计量的内核语义与合成身份防护

核对 [Apple XNU 的页查询实现](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/vm/vm_map.c)
发现：普通路径查找 VM 页并沿 shadow chain 查找实际页对象，返回该对象的
内核地址 hash 与对象内 offset；另有 footprint 路径返回合成对象身份、
固定 offset 和账本派生状态。因而 PRESENT 且非 FICTITIOUS 本身不足以
证明返回的是可逐页去重的普通对象身份，返回值也不是 PFN。

当前 RAM 合同为每个 VM 独立且不在自身 guest 页之间建立物理 alias：
文件 offset 各不相同，冷页恢复为独立可写匿名页。因此完整 RAM 检查新增
`ram_page_keys()`，拒绝同一 VM 中 PRESENT/非 FICTITIOUS 页的重复对象/offset
键，防止将合成身份折叠误算成极低内存占用。若未来允许 VM 内部合法 alias，
必须按显式 alias 合同更新校验，而不能直接删去检查以获得 PASS。
此检查不是通用的合成模式识别器；只有单个驻留页等退化情况下，仅凭
重复键无法区分模式，其他查询上下文与覆盖检查仍然必要。

[页键复核证据](../review_project/06-evidence/macos-memory/ram-page-key-validation.json) 通过有效/非驻留/虚构页的
分类与合成重复键拒绝检查；重验三组保存的完整 VM 查询共 12 份记录、
196,608 条页记录，同一 VM 内驻留页键均无重复。保存数据及验证脚本的
hash 已记录。这是对已有测量的加强验证，不是重新启动 VM 的实验。

RAM 范围的共享、私有化与回收证据继续有效；但 SDK/runner 的其他映射、
共享库、未映射 backing、pool 和内核开销仍没有统一计量，不能因此把
76% 左右代理降幅升级成完整物理收益 PASS。真实宿主压力也仍受磁盘准入
限制，整体目标继续未完成。

## 全进程遍历的可行性检查：覆盖预算拒绝

新增可独立运行的 `tools/experiments/macos-memory/process_inventory_check.py`，
编译小型原生 self-process 原型，重复三次检查：两个可写匿名页具有不同
对象/offset 身份；同一文件 offset 的两个共享映射具有相同身份；未触碰的
PROT_NONE 匿名页没有 PRESENT 标记。三次 fixture 检查均通过。

遍历使用 `mach_vm_region(VM_REGION_TOP_INFO)`，按 4096 页分批执行
`mach_vm_page_range_query`，对驻留或换出页补查对象身份。查询不读取页面
内容；但它自身的栈、输出和内核临时状态会变化，尚非原子快照。
程序不按零驻留区域统计值跳过映射，也不把提前停止的结果记作完整覆盖。

[原始结果](../review_project/06-evidence/macos-memory/process-inventory-feasibility.json)
记录三次扫描均遇到 384 GiB 虚拟区域，区域统计报告 private/shared resident
均为零。该区域超过当前 4,194,304 页（本机 64 GiB）的扫描预算，程序明确
返回覆盖拒绝，`full_process_coverage_verified=false`。当前有界遍历尚不能
覆盖完整进程；下一步需要验证能够跳过空保留区且不会漏掉驻留页的稀疏
路径，或有足够时间预算的完整扫描，再接入 SDK、runner 与 pool 的计量。

这是原型可行性与失败证据，不是生产接口，也不是两个真实 VM 的整体内存
验收。宿主压力实验仍需满足既定磁盘准入；本次检查时剩余约 370 MiB，
未启动压力分配。整体物理收益验证继续未完成。

## 扩大扫描预算后的完整遍历与账本差异

没有跳过零驻留区域，也没有删去覆盖检查。原生原型新增 `--wide`，将查询
预算扩大到 67,108,864 页（本机 1 TiB），并保留之前的覆盖拒绝记录。
[首次完整遍历](../review_project/06-evidence/macos-memory/process-inventory-wide.json)
三次均通过，61 个区域、约 30,548,800 页，耗时 0.32–0.76 秒；因此此前的
64 GiB 预算拒绝不能解释为完整遍历在本机不可行。

[带进程账本的复验](../review_project/06-evidence/macos-memory/process-inventory-wide-ledgers.json)
再次三次通过，耗时 0.321 / 0.330 / 0.812 秒。除 fixture 外，runner 检查
虚拟地址无重复、PRESENT/PAGED_OUT 行数一致，并对非虚构驻留对象身份
去重；每次识别一个合法共享别名。重现入口为：

```sh
python3 tools/experiments/macos-memory/process_inventory_check.py --wide
```

重要差异：所映射 backing 的非虚构驻留对象/offset 联集约 902.66 MiB，
而原型扫描前的 `TASK_VM_INFO.resident_size` 仅 1.63–1.64 MiB，
`phys_footprint` 仅 1.17–1.19 MiB。扫描后两项各增加约一个 16 KiB 页。
对象页查询包含共享 backing 的驻留页，不能解释为该进程独占或因启动该
进程才新增的物理内存。这也意味着直接给两个 VM 的所有映射求联集，仍
需要区分共同 backing、已有缓存与新增私有状态，不能直接替代归因计量。

本轮证明原生完整页查询可运行，并给出了三次实测诊断成本；不是实际
pVisor runner、SDK 或 pool 的集成检查。将该扫描放进 VMM 的全暂停区会
带来至少所测量级的额外诊断延迟，不能算作生产 pager 的尾延迟结果。
下一步仍需接入真实进程组、校验映射稳定性并统一记录页身份和进程账本。
全局物理收益标志继续为 false，真实宿主压力验证仍未完成。

## Rust 诊断接口与真实 VM 实验接线

`ram_backing::inventory::process_inventory()` 使用原生 Mach 接口完成全部
映射区域的有界扫描。它分配两个可写匿名 canary 页，验证 PRESENT 且非
FICTITIOUS、对象/offset 不同，并确认完整扫描包含这两个页；身份模式不符、
页状态在分批查询之间改变、返回数量异常或预算超限均返回错误。
预算为 67,108,864 个虚拟页、1,048,576 条驻留/换出记录；没有按区域的
零驻留统计跳过查询。返回页记录最大占 32 MiB，不包括 Vec 容量及其他
诊断开销。canary 和查询缓冲本身也属于采样期间的诊断开销。

新环境变量 `PVISOR_EXPERIMENTAL_MEMORY_PROCESS_INVENTORY` 指向私有输出
目录。实验 SDK 和 pool 在 25 秒后执行一次查询；pool 查询期间持有现有
pool 锁。runner 与既有 RAM inventory 一起，在 CPU/设备 quiescence 内查询；
只读查询错误不会将健康 VM 转为控制失败。文件在恢复后写入，输出目录必须
属于当前用户且拒绝组/其他用户访问，文件使用 create_new 与 0600 权限。
数值页记录直接序列化到缓冲 writer，避免为每条记录构建额外 JSON Value。
这些诊断默认关闭；它们的查询与暂停成本不纳入生产 pager 性能结论。

匹配实验脚本新增 `PVISOR_MEMORY_PROCESS_INVENTORY=1`，自动同时启用 RAM
inventory，并将 process inventory 放在另一个目录。解析要求 SDK、两个
runner、启用 offload 时的 pool PID 全部有记录；检查页尺寸、唯一地址、预算、
时间窗口，并要求每个 runner 的完整 RAM 驻留键属于对应进程查询的键集合。
它报告全部映射 backing 的键联集，不将联集等同于私有 RSS 或新增物理占用。
不同进程的采样没有全局原子性，SDK 分配器/其他线程也没有被整体暂停。

[接口验证证据](../review_project/06-evidence/macos-memory/rust-process-inventory-integration-check.json)
记录实际执行的命令、输出和源码 hash：pvisor lib 与两个实验 examples 定向
检查通过；同一 Rust 查询源码使用独立标准库 runner 编译（拒绝 warnings），
真实 Mach 完整扫描检查通过；解析器对合法 alias 只计一次，拒绝缺失进程、
重复地址、RAM 键遗漏与不可能的扫描行数。解析器输入是合成数据，不是
真实 VM 采样。

本轮没有运行新的真实 VM 进程组：检查后可用磁盘约 134 MiB，不足以承载
已有基线实验的两份文件 RAM 和新增诊断输出。接线目前只有编译/原生接口
及解析检查，`real_vm_process_group_inventory_verified=false`。需要在空间
满足准入后执行新匹配实验；不能用本轮接口检查替代该验收。宿主压力与
整体物理收益验证也继续未完成。

## 历史磁盘阻塞与实验恢复条件

匹配实验新增磁盘准入：编译前检查 build、temporary、evidence 所在卷，
每个 case 前重查 temporary/evidence；一般匹配实验要求至少 1 GiB，完整
进程诊断要求至少 4 GiB。它是余量检查而非空间预留，构建大小和其他应用的
并发写入仍会改变可用空间。拒绝时退出 2，在单独的 `*-preflight.json`
保留原因，不覆盖既有完整匹配数据，也不启动编译或 VM。解析检查增加
零预算放行、极高预算拒绝、未创建路径查找已有父目录的检查。

[实际诊断实验准入](../review_project/06-evidence/macos-memory/matched-deferred-ram-vm-release-process-inventory-disk-admission-preflight.json)
在当前环境返回 admission_rejected，构建/临时/证据目录都只有约 126 MiB。
宿主压力 [重新准入记录](../review_project/06-evidence/macos-memory/host-pressure-preflight.json)
同样拒绝。读取宿主状态需要脱离执行沙箱；复核命令只读取状态，不分配压力
内存、不启动 VM。压力实验继续要求至少 8 GiB 余量以及正常压力级别。
压力 wrapper 同时移除两种 inventory 开关，避免环境继承改变它依赖的
数据路径或额外暂停成本。

代码接线、原生接口和解析验证已完成当前阶段；最终验收仍缺真实进程组
采样、整体内存归因、真实宿主压力下的稳定性与性能。这些证据不能由已有
单进程原型或 RAM/pool 代理数据替代。连续多轮磁盘不足后，当前剩余实验
无法继续安全执行；目标以磁盘资源阻塞，尚未完成。恢复需腾出至少 8 GiB，
或由用户指定允许清理的缓存目录；未删除项目原有 target 或用户缓存。


## 恢复后的真实进程组与 WARN 实验

磁盘余量恢复至约 40 GiB 后，完成完整进程组诊断、原生 footprint 归因和实际
宿主 WARN 观察。原磁盘阻塞已解除；先前的未验收声明描述当轮状态。

完整进程诊断首次因 SDK 页状态在扫描中改变而失败；接口改为仅对 WouldBlock
最多整轮重试三次，不能拼接不一致的部分结果。下一轮因一个 runner 尚有
64 KiB 文件 punch 待完成而失败；诊断现在等待 pending 上界为零后才发布。
两次失败原始记录保留，未放宽覆盖或回收完成条件。

v3 六个双 VM case 全部通过，覆盖 SDK、两个 runner 与启用时的 pool；RAM
键均属于对应进程页查询。v4 同时完成原生 footprint 多进程采样，目标 PID
与进程组诊断精确一致，使用 summary.total 避免再次累加共享归因。stderr 明确
提示 VM-object 模式忽略 --unmapped；该结果不覆盖未映射的 owned VM objects。

| 指标 | 三对匹配结果的中位数 | 范围与解释 |
|---|---:|---|
| v4 RAM＋pool 代理占用下降 | 74.89% | 局部计量，不是完整进程组物理占用 |
| v4 全映射 backing 键联集下降 | 10.17% | 包含共享缓存对象，不能归因成实例新增内存 |
| v4 原生 dirty＋clean 进程组归因下降 | 63.37% | 包含逻辑换出/压缩账目，不是全机 PFN 审计 |
| 真实 WARN 网络组 RAM＋pool 代理下降 | 76.60% | 六次运行的后段样本均处于真实 WARN |

真实 WARN 组没有分配压力内存，也没有模拟压力；六次后段窗口共 821 次采样，
全部为 WARN，无 critical 或守卫错误。三次 cold 的完整 quiesce 事务 P95 为
9.092 / 9.065 / 9.104 ms，最大为 19.476 / 12.667 / 27.119 ms。
网络 burst 耗时比 cold/base 为 1.022 / 0.635 / 0.852，不能声称普遍加速。

原主动分配压力 wrapper 仍要求初始 normal；最新预检因初始 WARN 拒绝，
没有启动分配器或额外 VM。已有 WARN 下的验证不能替代主动压力升级和停止
分配器的完整验收。全机物理收益标志仍为 false，原至少 40% 物理收益门槛保留。

[派生汇总与原始数据哈希](../review_project/06-evidence/macos-memory/group-validation-summary.json)
由 group_validation_summary.py 校验生成；各历史运行保留执行时的源码 hash。


### 原生系统计量探针的限制

[native-system-footprint-capability.json](../review_project/06-evidence/macos-memory/native-system-footprint-capability.json) 保留只针对自建短时进程的
原生探针。`--vmObjectDirty` 与 `--unmapped` 不兼容，工具在 stderr 提示
忽略后者；v4 历史运行也有同一提示。派生汇总现在显式保留该诊断并标记
未映射 owned VM objects 未验证，后续采样移除不支持的参数并拒绝 stderr
诊断。历史原始记录不改写。

探针的 sys_footprint 约 39.0 GB，超过本机 24 GiB RAM，说明该字段不能
直接解释为当前物理驻留字节；JSON 也没有结构化系统字段。仅凭这个接口
不能补齐全机物理验收。不得将 logical footprint 降幅写成物理页降幅。


## 双账目与宿主物理压缩器采样增量

[最新报告](../review_project/03-modules/macos-memory-sharing.md#v7-实际双-vm-验收结果)
和派生汇总保留 v5/v6 失败及 v7 六 case 通过结果。诊断仍要求完整页状态
相等，仅对 WouldBlock 在五秒/20 次的重试准入预算内丢弃整次结果后重试；
单次原生查询不可中断，该预算不是全暂停硬上限。默认 pmap JSON 可省略
false 模式字段，现在由完整捕获命令与缺省语义确认模式。

VM-object 与 pmap 模式独立采集，不相加；按 footprint(1) 定义从 Dirty 扣除
Swapped，加 Clean/Reclaimable 得到各自驻留账目，Wired 不再重复相加。
vm_stat 保留 Pages occupied by compressor 的物理页数和 Pages stored in
compressor 的逻辑原始页数。原生未映射 owned-object 查询需要 root，本机
无交互 sudo 不可用；默认 pmap 查询不宣称包含它。

v7 三对 VM-object 驻留账目下降中位数 61.50%，pmap 驻留账目下降 14.16%，
RAM＋pool 代理下降 76.74%。后段 234 次样本均真实 WARN；宿主压缩器物理
cold-minus-baseline 差值为 −419.88 / −205.86 / ＋333.36 MiB，包含其他应用，
不能作为 pVisor 全机节约比例。global_physical_memory_reduction_verified
仍为 false，原物理验收门槛未改变。


## 十轮网络与及时取消增量

[最新报告](../review_project/03-modules/macos-memory-sharing.md#十轮网络匹配持续工作集与尾延迟)
记录六个真实双 VM case、120 次 burst、约 932.5 秒累计运行。持续 RAM＋pool
代理下降中位数 33.52%，采样最大值下降中位数 16.31%，明显低于初始 quiet
代理的 76.42%。关闭页诊断后的 quiesce P95 约 9.1 ms，最大 29.810 ms。
2690 次后段样本全部为真实 WARN，无守卫错误或终止；不算主动压力分配器验收。

实验 SDK 新增 SIGTERM 到现有 RunCancellation 的接线；守卫、超时与 finally
共用终止入口。自建模拟进程组验证正常和强制清理，对照进程保留；真实两 VM
在第五秒故障注入后约 0.34 秒完成收尾，均为 Cancelled、目标进程全部退出。
注入只改测试元数据，不模拟 OS 压力、不分配压力内存。

原物理占用至少减少 40% 的门槛保持；持续代理约 33.5% 不能充当该证明。
需要继续补全物理覆盖与应用/压力证据，不以短时匹配或累计运行时间宣布全面稳定。

## 扫描配额修正复验

[完整分析](../review_project/03-modules/macos-memory-sharing.md#扫描配额修正三对匹配通过尾延迟未闭合)
记录 Cold 等无操作状态消耗旧扫描配额的问题。当前保持 256 次状态操作和
8 ms 软预算，允许跳过更多无需操作的块；新增 worked 与 observation_restores
遥测，验证更快回收是否同时增加热页观察开销。定向 Clippy、Release 构建和
三对十轮网络匹配通过，累计 936.2 秒。持续代理下降中位数 56.11%，采样
最大值下降 16.71%，网络耗时比 0.786，quiesce P95 约 9.2 ms；最大暂停
80.281 ms，尾延迟尚未闭合。2752 次后段采样全部为真实 WARN。少量容量
拒绝正常退避，运行源码哈希和持续样本重算核验通过。保留配额修正继续验证，
不把代理降幅写成全机物理收益；原物理验收门槛保持。
