# 文件系统缓存与异步写：系统曲线评估与复现手册

本手册保留已执行曲线的合同与复现方法。后续替代七工具系统总分的三层负载协议见
[WORKLOADS.md](WORKLOADS.md)；其离线计划已可生成，新执行器与结果尚未提供。

## 结论范围

这里回答的问题是：**在相同的显式文件与目录 fsync 边界下，host 接管写入、
后台推进和合批分别能改善哪些请求形态的完成吞吐，代价落在哪个阶段和资源上？**

本手册登记可执行协议和后续实验设计，不登记尚未产生的性能结果。现有入口可以
得到粒度、文件并发、同步频率、工作量及有限参数敏感性曲线，并用独立诊断批次
核对实际 backing 工作。N5 批次用于探索；独立 N30 批次用于确认已选定的主点。
两者都不等于完整系统论文验收，也不构成 OSDI 或 artifact 认证。

本实验属于 [B-FS-ENG](../../benchmark/README.md#b-fs-eng)，诊断属于
[B-FS-DIAG](../../benchmark/README.md#b-fs-diag)。结果只进入工程分析。
不更新跨运行时用户表，不与普通 CLI 七工具、历史 hyperfine、DAX 或 guest
writeback 批次拼接。

## 动机与主假设

小块连续写的成本可能包含 guest/host 往返、输入复制、资格检查锁、排队和
backing 调用。把 WRITE 提前回复只移动等待位置；若随后 flush、fsync 或退出
承担同样甚至更大的成本，不能据此宣称任务加速。因此主指标是完整显式同步
边界的完成吞吐，WRITE 返回阶段和实际 backing 调用数用于解释机制。

预先检查以下假设，不以结果反向改写假设：

1. 连续、保持打开的同一文件上，合批减少 backing 调用，并改善包含最终同步的
   吞吐；收益随请求粒度变大而减弱。
2. `unmerged` 与 `owned-sync` 能区分后台接管、复制/worker 交接及合批的作用。
   `async-old` 用于识别前台/后台资格检查锁耦合的影响。
3. 高频 fsync、write-close 小文件和只读阶段限制了可隐藏的等待，可能不受益或
   退化。同步、关闭和目录持久化成本必须保留。
4. 有界预算只允许有限的提前接收。工作量跨过可用预算后，需观察完成吞吐和
   背压，不能只测队列尚未填满的短暂峰值。

## 实验设计

### 被测对象与固定条件

[curves_driver.rs](curves_driver.rs) 经 `pvisor_vm::api` 构造一个 rootfs
Overlay 连接；输出位于 `/curve-work`。这是 SDK 机制探针，不经过普通
`pvisor run --safe` 的完整 Job、Gateway、stage journal、Bundle 和 apply 流程。
不把它的绝对耗时解释为 CLI Ready 或整个 Agent 的完成时间。

[curves_guest.rs](curves_guest.rs) 编译为同一个静态 Linux x86_64 payload。
VM 和 `native` 条件执行逐字节相同的 `bin/payload`，读取同一对内的计划文件。
`native` 在宿主文件系统运行，作为参考；它与 VM 的内核、执行边界不同，不能
单独用其差值归因某一个 FUSE 组件。

当前协议固定 Linux x86_64/KVM、guest 2 vCPU / 512 MiB、guest writeback=0、
DAX=0、guest cache=1。rootfs lower 保留 Mutable 声明，不通过实验擅自承诺
物理不可变。所有条件使用独立 stage；native 不启动 VM。

完整 campaign 使用同一个父 cgroup：CPU 0,1、两核 quota、2 GiB、swap=0，
文件描述符限制 65,536。观察者和内存采样线程也在这个组内。启动前校验实际
quota、memory/swap 上限和进程 affinity；原始 cgroup、CPU 和 backing 文件系统
信息写入证据。不得把“配置了预算”改写为“没有宿主竞争”。当前没有安静期、
外部构建/测试干扰或逐轮全量输入清单门禁。

### 当前三个计划

下表是完整默认计划的**预期库存**，不是已测样本数。每个独立环境内执行三轮，
每轮执行该 suite 的全部 cells。`--conditions` 会缩减条件集合，实际库存以冻结
的 `protocol.json` 和 `plan-*.tsv` 为准。

| Suite | Cells / 条件数 | 独立环境数 | 预期 `CURVES_RESULT` 行数 | 用途 |
| --- | --- | --- | --- | --- |
| `shape --samples 5` | 18 / 6 | 30，其中 25 VM、5 native | 1,620 | 机制与形状探索 |
| `parameters --samples 5` | 4 / 10 | 50 VM | 600 | 逐项参数探索 |
| `confirm --samples 30` | 4 / 2 | 60 VM | 720 | 独立确认固定主点 |

`shape` 的 18 个 cells 如下。除注明外，stream 总工作量为每轮 4 MiB，
单 worker，末尾同步，不在循环中周期 fsync。

| Family | 横轴 | 固定工作量与操作 |
| --- | --- | --- |
| `block` | 1、4、16、64、256、1024 KiB | 保持打开的单文件连续 `pwrite`；每轮 4 MiB |
| `workers` | 1、2、4 个 worker / 文件 | 4 KiB 请求；总量固定 4 MiB，均分给 worker |
| `barrier` | 每 1、16、256 次写 fsync；0 表示仅最终 fsync | 4 KiB 请求；总量 1 MiB |
| `readset` | 1、8、32 MiB | 64 KiB `pread`；准备好的 upper 文件，三轮复用 |
| `smallfiles` | `tiny-1k` | 128 个 1 KiB 文件，逐文件 create/write/checked-close |
| `sync` | `sync-each` | 4 KiB 请求、256 KiB 总量，每次写后 fsync |

`confirm` 预先固定为 `block-1k`、`block-64k`、`tiny-1k`、`sync-each`，
只比较 `sync` 和 `async`，与 shape 中对应 cell 的输入相同。当前命令不会把
任意探索赢家自动加入确认集；要确认另一个参数或主点，需先登记新协议并准备
新批次。不能据确认集替探索中所有 18 个点作 N30 保证。

`parameters` 的 4 个 cells 是 64 KiB 请求、每轮 8/64/128 MiB 的三个
`pressure-*`，以及 4 KiB 请求、每轮 8 MiB 的 `stream-4k`。10 个条件包括
默认 `async`，容量 4/16/256 MiB，batch 64/256 KiB，以及 delay
0/100/1000/50000 μs；每臂只改一个字段，其余保留默认值。

### 消融与参数合同

| 条件 | 相对默认异步实现的改变 | 可回答的问题 |
| --- | --- | --- |
| `sync` | host writeback=0 | 原有直接同步宿主路径 |
| `async` | 默认配置 | 完整候选 |
| `unmerged` | `chunks=1, delay_us=0` | 保留接管/后台队列，禁止合并 |
| `owned-sync` | `synchronous=true` | 保留 owned copy 和 worker 交接，WRITE 回复前排空 |
| `async-old` | `qualification_lock=true` | 恢复前台/后台资格检查锁耦合 |
| `native` | 相同 payload 直接在宿主运行 | 原生参考，不作为产品条件或单组件消融 |

宿主私有开关为 `PVISOR_VM_FS_HOST_WRITEBACK=0|1`，未设置默认关闭。
配置对象为 `PVISOR_VM_FS_HOST_WRITEBACK_CONFIG`。worker 严格校验 JSON 对象、
字段类型、未知字段和范围，非法值不静默回退；这些控制不是公开 VM API 或
snapshot schema。

| JSON 字段 | 默认值 | 支持范围 / 含义 |
| --- | --- | --- |
| `capacity_mib` | 64 | 整数 4–256；payload 预算，含预留、排队和执行中的字节 |
| `jobs` | 1024 | 整数 1–4096；上述三种状态共同占用的任务数 |
| `batch_kib` | 1024 | 整数 4–4096；不得超过 payload 预算 |
| `chunks` | 64 | 整数 1–64；一次合批最多多少个原始请求 |
| `delay_us` | 10000 | 整数 0–50000；最老待处理项的合批等待目标 |
| `qualification_lock` | false | 布尔值；资格检查锁消融 |
| `synchronous` | false | 布尔值；提交后在 WRITE 回复前等待排空 |

单项写入不为满足 batch 大小而拆分；超过 batch 目标的单项单独执行。合并还
要求同一打开对象/句柄身份、相同 killpriv 语义和连续 offset。这里的容量不是
整个文件系统、guest 页缓存或 runner RSS 的硬上限，句柄、任务元数据和分配器
另有成本。

字节和任务数可能先后成为瓶颈。若每个接收请求恰为 4 KiB，1024 项最多承载
4 MiB；若恰为 64 KiB，则最多承载 64 MiB。实际请求形状必须由独立诊断核对。
因此现有 `capacity-256m` 不自动证明测试填满了 256 MiB；`peak_jobs` 先到顶时，
曲线可能由任务数限制。要隔离更大字节预算的作用，需另登记 jobs 敏感性或更大
请求的对照，不能运行后补改解释。

runner 清理继承的 `PVISOR_*`，再安装冻结条件。临时在 shell 中设置配置不会
覆盖已准备批次的条件；要扩展矩阵，应先修改并登记生成器，再创建新批次。

### 配对、重复与输入

每个 pair 用同一个 seeded RNG 随机排列条件和 cell 顺序。该 pair 的各条件
共享 `plan-NNN.tsv`，三轮沿用同一行顺序。随机顺序在测量前写入协议；这不是
强制平衡顺序或拉丁方。不同 pair 有独立环境，内部三轮、多个 cell 和多个
worker 都不是额外独立 trial。

payload 按 cell ID、worker 和 round 生成确定的高熵字节：FNV1a64 生成 ID
种子，再以 SplitMix64 式混合产生 little-endian 字节。写阶段每轮使用不同种子；
readset 使用固定 round=0，复用同一文件。它避免全零或短重复块输入，作为近似
不可压缩的负载选择；未测压缩率时不宣称已量化排除压缩/去重影响。

数据生成、目录/stream 文件预建、worker 创建和就绪握手位于 cell 计时之外，
记录在 `preparation_ns`。小文件的 create 和 close 属于被测操作，保留在计时内。
readset 的首次准备包含写入和 fsync，因此是经过准备的宿主热数据；fresh guest
不等于冷宿主页缓存或冷磁盘。guest 页缓存、协议缓存和 backing 页缓存的命中
情况另用诊断解释，不能用第几轮直接替代缓存状态证明。

## 时钟、持久化与正确性

### Cell 时钟

每个 cell 的 worker 预先启动；计时包含阶段命令分发、等待 worker 完成和实际
文件调用。并发 worker 的 wall time 由同一协调者测量，不能把 worker 时长相加
当作 cell 时间。

| 字段 | 起止与包含内容 |
| --- | --- |
| `accepted_ns` | 执行循环至全部 worker 返回；stream 包含写及指定的周期 fsync，smallfiles 还包含 create/checked-close，read 包含读循环 |
| `applied_ns` | stream 在仍保留原 fd 时 dup，再 checked-close duplicate，触发 FUSE FLUSH 的已接收前缀屏障；不是 fsync。smallfiles 已在循环内关闭，read 无此 I/O |
| `fsync_ns` | stream 对原打开文件最终 fsync；smallfiles 逐文件 reopen/fsync/checked-close；read 无此 I/O |
| `close_ns` | stream/read 最后 checked-close 原 fd；阶段协调成本也保留 |
| `dirsync_ns` | 写 workload 对 worker、round、cell、输出根和 workspace 目录逐层 fsync/checked-close；read 无目录同步 |
| `total_ns` | 从循环开始至目录同步结束；严格等于上述五段之和 |

`accepted_ns` 不是所有 cell 的纯 WRITE ACK 延迟，read 的对应指标是读循环吞吐。
`periodic_fsync_worker_ns_sum` 是已经包含在 `accepted_ns` 中的各 worker 同步
时间之和，可能并发，不能再加到 `total_ns`。

所有写条件，包括 `sync` 和 `native`，都有相同显式文件及目录 fsync 边界。
这里不使用“关闭缓存时 ENOSYS 的 SYNCFS 成功”对照“启用缓存时真实 SYNCFS
成功”来构造持久化差异。fsync 成功是本实验采用的完成边界，不等于已经做过
掉电、设备缓存或文件系统崩溃恢复实验。

每个 cell 在停表后由 guest/native payload 逐字节读回，核对长度和文件成员。
进程正常结束后，观察者再用同一个静态 payload 的 `--verify` 模式独立检查
宿主 stage 的完整输出、round/worker/文件库存和全部字节。后一次验证不计入
cell 时钟；仅收到 WRITE 回复或 `CURVES_DONE` 不足以判定有效。

### 环境与资源时钟

`ready_ms` 从观察者启动 subject 到 guest payload 的 `CURVES_READY`，不是
公开 VM ready callback，也不是普通 CLI Ready。`job_ms` 覆盖整个 subject/
payload 进程，包括启动、cell 准备、guest 验证和退出，结束于独立宿主验证之前。

资源采样范围比 `job_ms` 更宽：当前 10 ms `memory.current` 采样、byte-seconds
积分和最终 cgroup 快照还包含独立宿主验证及观察者开销，覆盖该环境的全部
cells，不能分摊成某个 cell 的局部内存。采样峰值可能漏掉短峰，字段名保留
`sampled_`。父 cgroup 跨整个 campaign 复用；内核 `memory.peak` 是该组累计
高水位，后续 trial 会继承此前峰值，**不是每个条件的独立峰值**。`cpu.stat`、
`io.stat` 等累计记录也要先取明确边界的差分，不能直接比较原始累计值。

队列 profile 的 `*_peak_bytes_sum` / `*_peak_jobs_sum` 是每个停止 session
峰值的和；只有确认该 owner 只有一个 session 时才能当作它的队列峰值。该计数
只覆盖 payload/任务预算，不替代整个 cgroup 内存，也不证明 VM 已能 offload。

### 有效性与失败

观察者要求正常退出、唯一 Ready/DONE、精确 cell×round 行库存、计划参数和
字节数一致、各阶段和等于 total、全部 correctness 成功，以及唯一正面的完整
宿主验证收据。正式计时发现 profile 输出会失败。每个 trial 的日志和
`result.json` 都保留；成功 stage 在验证后清理，失败 stage 保留。

超时、非零退出、错误字节、缺行/重复行、错误参数和不完整验证不是零耗时样本。
不按运行速度、干扰判断或“看起来异常”删除有效慢值。分析器显式保留失败记录
和不完整配对；有失败或缺失环境的输出不能作为完整确认批次发布，也不能补跑
几个样本后悄悄拼满 N。

## 不可变 lower：跨 RAM 的读缓存曲线

独立 `read_cache_curves.py` 批次在相同物理、调用方拥有的 Immutable fixture 上，
比较 native、`PVISOR_VM_CACHE_EXPERIMENT=off` 与 `root-data`。guest WB=0、
host WB=0、DAX=0，只有 guest lower 数据缓存开关变化。fixture 作为 rootfs
的最高优先级只读层；external rootfs 保持原 Mutable 声明，不把全部 rootfs
错误声明为不可变。

工作集为 1/16/64/256/384/448/512/768 MiB，guest RAM 为 512 MiB；中间点用于
展示容量边界附近的历史差异。每个 cell 连续五次 reopen/read/完整 memcmp/
checked-close，每环境三轮，N5 新环境。`first` 是每个 cell 每轮的第一遍，
`repeat` 是第 2–5 遍；跨三轮先分别累加字节和耗时再求吞吐，不等同冷盘首次访问。
逐轮图保留相同环境内的关联，不能把这些点当成更多独立 trial。

fixture 在测量外创建，使用确定性 1 MiB 高熵 pattern，在文件内重复；每个完整
文件有 SHA-256 和库存，测前测后完整验证，只读权限固定。guest 仅需一个期望
pattern 和一个 1 MiB 读缓冲；全字节比较在读时钟内。因此此吞吐包含校验，不能
与 write shape 的停表后校验或无校验工具直接比较。未请求 KSM，未测物理压缩
或去重率；共享宿主输入页可能在组外预热或计费，不以此推断整机内存节省。

独立 N1 B-FS-DIAG 复用相同 fixture、subject/payload/firmware，用每次 pass 的
marker 核对 READ、OPEN、LOOKUP 和其他库存。marker 范围内的异步预读可能跨过
边界；完整 final records 和逐 pass 差分同时保留。正式计时批次不启用 profile。

```sh
read_batch="$PWD/benchmark.new/filesystem/.data/read-cache-UNIQUE"
python3 benchmark.new/filesystem/read_cache_curves.py --prepare "$read_batch" \
  --rootfs "$curves_rootfs" --firmware "$curves_firmware" \
  --source-receipt /absolute/path/to/product-build-receipt.json \
  --samples 5 --seed 20261017
"$SEMSPEC_BIN" bench --cwd "$read_batch" "$read_batch/in.json"
python3 benchmark.new/filesystem/read_cache_curves.py --analyze "$read_batch"

read_diag="$PWD/benchmark.new/filesystem/.data/read-cache-diag-UNIQUE"
python3 benchmark.new/filesystem/read_cache_curves.py --prepare "$read_diag" \
  --rootfs "$curves_rootfs" --firmware "$curves_firmware" \
  --source-receipt /absolute/path/to/product-build-receipt.json \
  --reuse-build "$read_batch" --fixture-batch "$read_batch" \
  --samples 1 --seed 20261017 --diagnostic
"$SEMSPEC_BIN" bench --cwd "$read_diag" "$read_diag/in.json"
python3 benchmark.new/filesystem/read_cache_curves.py --analyze "$read_diag"
```

`--source-receipt` 必须列出与实际构建匹配的完整 product `source_hashes`；当前
准备器复用普通 CLI 的构建收据格式，逐项校验当前源码。`--reuse-build` 复用经过
摘要验证的相同制品，不复用计时结果。外部 rootfs 的完整性范围与上文写实验相同。
采集冻结在 `harness/`，后续分析使用的脚本及 helper 摘要另存 `analysis/summary.json`。

## Guest 内核写合并与流水线：独立后续实验

`guest_writeback_curves.py` / `guest_writeback_driver.rs` / `guest_writeback_guest.rs`
属于新的 B-FS-ENG 四臂实验，不重新解释上述 host 缓存样本。四臂为新固件 WB=0、
新固件 WB=1、旧 0047 固件 WB=1、native；host 写缓存与 DAX 始终关闭。
两种固件的 kernel/config/build 工具及 0041–0047 相同，0048 是内核代码差异。
冻结每臂实际加载固件的回执，分别比较 WB 开启收益和 0048 增量。

负载覆盖 1/4/64/1024 KiB、1/2/4 workers、每 1/16/256 次及仅最终 fsync、
128 个 1 KiB 文件和 8/64/128 MiB 压力。每条件五个 fresh 环境、内部三轮；
三轮先在环境内相加，配对 bootstrap 的独立单位仍是五个环境块。保留全部慢样本。
这一 payload 取消额外 dup/close 排空；同一文件的最终 fsync 承担 guest 缓存排空，
再 checked close 与目录 fsync。`applied_ns=0` 且标明未单独测量，不能把它当作零成本。
这些阶段与旧 host 缓存曲线不同，不拼接吞吐或阶段耗时。

正式时钟关闭 profiling；独立 N1/臂诊断核对全部注册/final、阶段库存和实际协商的
guest writeback/批量写回开关。统计 FUSE WRITE 的字节、后台标志及互斥大小桶；
最大桶仅给出粒度界限，不能冒充精确最大请求大小。提交与异步完成可能跨阶段边界，
保留最终总计，不从窗口内零请求推断整个任务无请求。

`guest_writeback_pipeline.py` 属 B-FS-DIAG：预先打开 A/B，A close 后 200ms 写 B，
用宿主 LD_PRELOAD 把 A 首次 backing pwrite 延迟 1.5s。以同一宿主时钟核对 B
是否在 A 完成前写入 backing，检查真实拦截、正常退出与完整字节。该实验验证等待顺序，
不报告生产吞吐。最初按需创建 B 的版本和修正前固件的失败诊断全部保留；不能据此
宣称 host namespace admission 已完全并行。固件完成线程先等已提交 WRITE，再同步
metadata，最后再次等待新提交 WRITE；双屏障都保留。

0049 调优独立使用 `--baseline-patch 48`：同 host/payload 的 0048 WB=1 与
0049 WB=1 隔离调优增量，另保留新固件 WB=0/native。提交提示改为压力目标的
1/4（1–16 MiB），锁外近似统计只影响提前 kick；逐 folio 压力与近阈值精确统计、
10ms、256 leases、双屏障不变。旧批次冻结源码和数据保留，新的时间分析明确使用
`incremental_0049`，不把它改写成0048的收益或拼接五环境。新增真实同inode多FD
回归跨共享/跨页写入、114次close/reopen，最后fsync+syncfs和双端全字节验证。

0049 初版的正式20环境批次有一个环境在第二轮128MiB同步失败，整批不用于性能
结论。保留完整失败计划后，`guest_writeback_memory.py` 在同一512MiB guest、
2GiB父预算下采集 `/proc/meminfo`、buddyinfo、vmstat 和内核日志。内核栈与
对应 vmlinux 反汇编定位到 `virtio_fs_enqueue_req+0x159` 的 order-2 原子 SG
数组分配；失败后仍有约360MiB可用内存，不能解释为工作集超过全部可回收RAM。
修复版在每个 request virtqueue 启动时预分配516项 SG/指针暂存区，原队列锁内
构造并同步交给 virtqueue。逐请求 argbuf 仍独占，数据页生命周期、1MiB上限和
脏页预算保持不变；该修复没有消除所有其他原子分配点。原失败证据保留，新版使用
独立新目录先重复完整历史序列，再测新的N5，绝不把旧批次19个成功环境补齐使用。

普通 CLI 的独立入口是 `guest_writeback_cli.py`：同一最终固件、同一普通CLI
制品，仅切换 WB=0/1，N5新环境、内部三轮。Ready 从创建进程到 ready token，
排除退出。主要指标为三轮七工具 worker 时间之和；小文件 write 包含 write/close/stat，
没有显式 fsync，stream_write 则含 fsync/close，二者边界按原负载保留。这个合计
不能称为全部写入持久化完成时间。完整stage字节、正常退出和Bundle校验在时钟外；
独立N1诊断检查两个设备的实际协商状态和全部13个注册/final实例。

准备与构建发生在时钟外；三个入口生成 `in.json` 后统一通过 `semspec bench` 执行。
四臂时钟由 `guest_writeback_analysis.py` 单独加工，输出 completed/accepted 时间比及
每个独立环境原值；候选/基线时间比小于 1 表示更快。加工后用
`publish_guest_writeback.py --check` 核验，`--publish` 发布到独立 guest-writeback
命名空间，不覆盖已有 host 缓存数据。
0048→0049 使用独立 `publish_guest_writeback_tuning.py` 和
`filesystem-guest-writeback-tuning` 命名空间；普通CLI使用
`publish_guest_writeback_cli.py` 和 `filesystem-guest-writeback-cli`。

## 复现步骤

### 准备条件与来源

需要可用 `/dev/kvm` 的 Linux x86_64 主机、CPU 0,1、可安装上述 cgroup 限制
的 user systemd、Python 3.11+、Rust 和 `x86_64-unknown-linux-musl` target、
离线可用的 Cargo 依赖、预先准备且受调用方控制的兼容 rootfs/固件，以及已构建
的独立 semspec CLI。绘图另需 matplotlib；`--no-plots` 只生成 CSV/JSON。

`--prepare` 会构建机制 subject 和静态 payload，并复制固件、harness、源码和
输入计划；构建不计入样本。它不供应或下载 rootfs。当前 rootfs 在协议中记录
绝对路径，不复制或完整哈希；调用方应保存其准备收据并保持内容不变，不能声称
runner 已独立证明 rootfs 不变。backing 挂载选项和缓存状态也须随报告说明。

源码副本及逐文件哈希保存在 `source/` / `protocol.json`。准备器检查构建期间
对应源码未变；固件、payload、subject、harness 和计划均有摘要，campaign 前后
核对。`source_head` 只是一项来源记录，不能代替可能含未提交改动的完整源码
哈希。生成的 `driver/Cargo.toml` 保留原仓库绝对依赖路径，源码归档不是无需
本机工具链/依赖就能运行的便携镜像；异机复现应恢复依赖并准备一个新批次。

下面的 `UNIQUE`、rootfs、固件路径需要替换。每个输出目录必须不存在；失败批次
也不复用。命令从仓库根目录执行。选择较少条件时，工程分析仍须包含相应基线：
shape/confirm 为 `sync`，parameters 为 `async`。

```sh
export SEMSPEC_BIN="$PWD/target/semspec/debug/semspec"
curves_rootfs=/absolute/path/to/prepared-rootfs
curves_firmware=/absolute/path/to/libkrunfw.so
shape_batch="$PWD/benchmark.new/filesystem/.data/curves-shape-UNIQUE"

python3 benchmark.new/filesystem/curves.py --prepare "$shape_batch" \
  --rootfs "$curves_rootfs" --firmware "$curves_firmware" \
  --suite shape --samples 5 --seed 20261017
"$SEMSPEC_BIN" bench --cwd "$shape_batch" "$shape_batch/in.json"
python3 "$shape_batch/harness/curves_analysis.py" "$shape_batch"
```

`in.json` 的一行执行完整预先登记 campaign；semspec 外层 wall time 不是曲线
样本，也不产生额外独立重复。真正数据来自 payload 的 event clocks。

参数探索与确认分别建新目录，不与 shape 合并：

```sh
parameter_batch="$PWD/benchmark.new/filesystem/.data/curves-parameters-UNIQUE"
python3 benchmark.new/filesystem/curves.py --prepare "$parameter_batch" \
  --rootfs "$curves_rootfs" --firmware "$curves_firmware" \
  --suite parameters --samples 5 --seed 20261017
"$SEMSPEC_BIN" bench --cwd "$parameter_batch" "$parameter_batch/in.json"
python3 "$parameter_batch/harness/curves_analysis.py" "$parameter_batch"

confirm_batch="$PWD/benchmark.new/filesystem/.data/curves-confirm-UNIQUE"
python3 benchmark.new/filesystem/curves.py --prepare "$confirm_batch" \
  --rootfs "$curves_rootfs" --firmware "$curves_firmware" \
  --suite confirm --samples 30 --seed 20261018
"$SEMSPEC_BIN" bench --cwd "$confirm_batch" "$confirm_batch/in.json"
python3 "$confirm_batch/harness/curves_analysis.py" "$confirm_batch"
```

独立诊断示例复用同样 suite，但另开 N1 批次、开启 profile；不是给正式计时
批次加日志后继续拼接。native 没有 VM filesystem counters，明确记为不可用。

```sh
diagnostic_batch="$PWD/benchmark.new/filesystem/.data/curves-diagnostic-UNIQUE"
python3 benchmark.new/filesystem/curves.py --prepare "$diagnostic_batch" \
  --rootfs "$curves_rootfs" --firmware "$curves_firmware" \
  --suite shape --samples 1 --seed 20261017 --diagnostic
"$SEMSPEC_BIN" bench --cwd "$diagnostic_batch" "$diagnostic_batch/in.json"
python3 "$diagnostic_batch/harness/curves_diagnostics.py" "$diagnostic_batch"
```

### Guest 0048→0049 的独立批次

下面的旧固件必须含0048但不含0049；新固件只新增0049，SOURCE必须与实际字节
一致。已有两代固件的构建在计时外完成；不能把重新准备当作旧批次的续采。

```sh
guest_pre="$PWD/benchmark.new/filesystem/.data/guest-wb-pre-UNIQUE"
guest_timing="$PWD/benchmark.new/filesystem/.data/guest-wb-timing-UNIQUE"
guest_diag="$PWD/benchmark.new/filesystem/.data/guest-wb-diag-UNIQUE"

python3 benchmark.new/filesystem/guest_writeback_curves.py --prepare "$guest_pre" \
  --rootfs "$curves_rootfs" --baseline-patch 48 \
  --firmware "$new_firmware" --firmware-source "$new_source_receipt" \
  --old-firmware "$old_firmware" --old-firmware-source "$old_source_receipt" \
  --samples 1 --preflight
"$SEMSPEC_BIN" bench --cwd "$guest_pre" "$guest_pre/in.json"

python3 benchmark.new/filesystem/guest_writeback_curves.py --prepare "$guest_timing" \
  --rootfs "$curves_rootfs" --baseline-patch 48 \
  --firmware "$new_firmware" --firmware-source "$new_source_receipt" \
  --old-firmware "$old_firmware" --old-firmware-source "$old_source_receipt" \
  --samples 5 --reuse-build "$guest_pre"
python3 benchmark.new/filesystem/guest_writeback_curves.py --prepare "$guest_diag" \
  --rootfs "$curves_rootfs" --baseline-patch 48 \
  --firmware "$new_firmware" --firmware-source "$new_source_receipt" \
  --old-firmware "$old_firmware" --old-firmware-source "$old_source_receipt" \
  --samples 1 --diagnostic --reuse-build "$guest_pre"
"$SEMSPEC_BIN" bench --cwd "$guest_diag" "$guest_diag/in.json"
python3 "$guest_diag/harness/guest_writeback_curves.py" --diagnose "$guest_diag"
"$SEMSPEC_BIN" bench --cwd "$guest_timing" "$guest_timing/in.json"
python3 "$guest_timing/harness/guest_writeback_curves.py" --analyze "$guest_timing"
python3 benchmark.new/filesystem/guest_writeback_analysis.py "$guest_timing" \
  --diagnostic "$guest_diag"
```

绘图命令在装有matplotlib的环境中执行，本轮保留的依赖版本为3.11.2；缺依赖时
可先用 `--no-plots` 生成数值，再在依赖齐全后重新加工同一原始批次。内存定位
入口的 `--source-batch` 指向有完整 `plan-001.tsv` 的N5准备目录，生成独立诊断
`in.json`，同样通过semspec执行，不能把插桩计时并入正式数据。

普通CLI入口的 `--build`、`--prepare` 和 `--setup` 分别记录GNU制品构建、冻结
输入及委托cgroup；它需要既有owned输入generation的准备收据。PRE、profiles、
timing分别由冻结config引用实际semspec入口，具体参数保存在该批次的
config、manifest和build-receipt中。SDK路径不替代这条普通CLI执行链。

### 证据清单与分析

| 文件 | 用途 |
| --- | --- |
| `protocol.json`, `plan-*.tsv` | 固定问题、suite、全部 cell/条件、配对随机顺序、预算、来源和摘要 |
| `source/`, `driver/`, `build.log`, `bin/`, `harness/` | 源码归档、构建参数、实际执行制品和冻结入口 |
| `in.json` 及 semspec 导出/日志 | 实际外层入口与执行收据，不能替代内部样本 |
| `campaign-command.json`, `campaign.*.log`, `budget-*.json` | systemd/taskset 命令、campaign 输出和父组预算 |
| `trials/NNN-CONDITION/{stdout.log,stderr.log,result.json,host-verification.log}` | 每个独立环境的完整行输出、错误、资源和宿主验证收据 |
| `records.partial.json`, `records.json`, `status.json` | 中间证据、最终 trial 库存和失败总数 |
| `analysis/raw_rows.csv`, `environment_points.csv`, `environment_resources.csv` | 所有原始行、逐环境聚合点和整环境资源；不丢失配对 ID |
| `analysis/curve_summary.csv`, `paired_ratios.csv`, `paired_ratio_summary.csv`, `failures.csv`, `summary.json` | 派生分布、逐对比值、区间、失败及统计口径 |
| `analysis/*.svg`, `analysis/*.pdf` | 由同一批数据生成的曲线；缺 matplotlib 时 CSV/JSON 仍保留 |
| `curves-diagnostics.json` | 独立诊断的最终计数和逐 cell 区间，不能作为性能分布 |

每个 cell 先在**同一环境**合并三轮：完成吞吐为总字节 / 三轮 `total_ns` 之和，
阶段时间为三轮均值。不能先平均三个吞吐再当作同一个量。随后按环境计算
中位数；比较项先对同一 pair 求 candidate/baseline 完成吞吐比，再统计这些
配对比值。比值大于 1 表示更快，不能与“耗时减少百分比”混用。

当前分析做 5,000 次 bootstrap，重采样独立环境或独立 pair，不拆开内部三轮。
它保留全部逐环境点和逐对比值；N5 仍只有 5 个独立重复，不会因三轮或 bootstrap
变成 15 个 trial。多点区间未作多重比较校正，不能选择最优点再用原批次声称确认。
95% 比值区间跨 1 时写“未检出差异”。分离簇、时间漂移或明显顺序依赖应展示原值
并解释，不用一个整体排名遮盖。多层重复和不确定性设计可参考
[Kalibera 与 Jones 的原始论文](https://kar.kent.ac.uk/33611/)。

诊断要求启动注册的 `(pid, component, instance)` 与唯一 final 完全匹配；当前
subject 恰有 rootfs 的 protocol、dispatch、overlay、Core 四个实例。只读取周期
checkpoint 或忽略缺少 final 的实例均不算完整。每个 cell/round 要求恰好一对
token，并按预先保存的 cell 顺序映射；不按日志时间猜归属。

逐区间保留原始累计差分，再仅对计数扣除一次 marker 自身：一个 IOCTL、80
请求字节和一个 direct request。inclusive span 的时间保留诊断含义，不能相加
为总时间或据此替代正式性能样本。最终计数包含准备/验证，phase 差分排除它们。
启用缓存时，accepted bytes、backing bytes、accepted requests/queue jobs 和
backing/queue batches 必须闭合且 backing errors=0；缺少显式计数不是零。
condition/total 汇总是不同诊断 trial 的工作量之和，不是平均工作量。

## 发布加工数据与图

`publish_io_curves.py` 接受完整 shape、parameters、独立 N30 confirm 和普通 CLI 批次，
可选同一问题的读缓存与独立读诊断。它检查完整样本、最终校验、CSV/summary 投影和
冻结制品摘要，复制全精度派生 CSV、SVG/PDF，生成不含本机绝对路径的共享来源摘要。
逐环境点和逐轮聚合保留；逐操作原始行、完整清单和日志仍在 `.data/`。

```sh
python3 benchmark.new/filesystem/publish_io_curves.py \
  --shape "$shape_batch" --parameters "$parameter_batch" \
  --confirm "$confirm_batch" --cli /absolute/path/to/cli-batch \
  --read-cache "$read_batch" --read-diagnostic "$read_diag" \
  --check --output /tmp/io-publication-check.json
```

用 `--publish` 替换 `--check` 才写入双语工程 CSV 和共享图；不更新历史跨运行时表。
发布器拒绝在同名目标中替换不同数据。新批次或重新分析应审查命名与来源，明确更换
对应文档，而非悄悄覆盖旧结论。`benchmark.new/docs.json` 注册两篇 locale 工程页
及其全部下载，文档翻译复核、`docs.py check` 和严格 docs build 分开执行。

## 形成完整系统结论仍需哪些实验

下表把现有可执行行与后续设计分开。后续参数是待登记的最小实验设计，不是当前
runner 已有开关，也不是已完成或已有结果。

| 问题 | 轴与对照 | 主要指标 | 当前覆盖 / 后续动作 |
| --- | --- | --- | --- |
| 改善来自哪一环？ | sync、owned-sync、unmerged、async、async-old | 完成/循环吞吐、阶段时间、实际 backing batches/bytes | 已有 shape 消融；需先核对 DIAG 实际走了哪些路径 |
| 粒度和局部文件并发如何影响收益？ | 1–1024 KiB；1/2/4 worker，总字节固定 | 完成吞吐、同步/关闭占比、逐对比值 | 已有闭环有限工作量曲线；不是多 VM 扩展性 |
| 高频同步和小文件是否退化？ | 每 1/16/256 次及最终 fsync；create/write/close | total、accepted、applied、fsync、close、dirsync | 已有；另有普通 CLI 七工具 N5，Cargo/npm 等应用待测 |
| 队列参数是否稳健？ | Q=4/16/64/256 MiB；batch=64/256/1024 KiB；delay=0/100/1000/10000/50000 μs | 完成吞吐、两类高水位、reserve waits、整环境资源 | 已有部分逐项参数臂；需识别 jobs 瓶颈，再登记 jobs/更大请求敏感性 |
| 是否只是短暂填队列？ | 长运行，累计字节覆盖 Q 的 0.25/1/4/16 倍；慢 backing | accepted 与 completed/durable 时间线、队列增长、最终 drain | 当前只有有限 8/64/128 MiB 工作量，实际队列未饱和；持续稳态和慢存储待测 |
| 饱和前后吞吐与尾延迟如何变化？ | 开放到达负载约为同步基线容量的 25/50/75/90/100/110/125% | offered/completed rate、端到端 P50/P95/P99、积压/拒绝/超时 | 未测；需独立负载发生器与逐请求时间戳，当前 cell 均值不能替代 |
| 工作集跨过缓存容量会怎样？ | read WSS / 有效 guest RAM=0.25/1/4；host/guest/protocol/tool 缓存状态分层 | 首次/重复 read、fault、I/O、整组内存、读写混合干扰 | 已有独立 Immutable lower 1–768 MiB 五遍 reopen 曲线及逐轮历史；不同 RAM 和冷盘待测 |
| 多 VM 是否增加密度且公平？ | VM=1/2/4/8，固定整父组 CPU/memory/swap，混合前台读写 | 总吞吐、逐 VM 延迟/公平性、CPU、byte-seconds、OOM | 未测；不能用当前单 VM 1/2/4 worker 推断 |
| 资源成本归谁？ | 同一 durable 字节数；区分运行阶段与独立验证阶段 | 父组 CPU 时间、memory.current/采样峰值、stat、fault、I/O/pressure | 已记录整环境原始资源；局部 workload 资源和严密短峰捕获待补 |
| 错误和生命周期是否安全？ | ENOSPC/EIO、短写、worker 异常、满队列、取消、unlink/rename、capture、DESTROY/INIT | 接收/完成边界、sticky error、所有权释放、无越界晚写 | 497 项 VM 测试通过，真实 VM 正常及注入错误探针通过；完整生命周期故障矩阵待补，正常曲线不能替代 |
| fsync 后能否从崩溃恢复？ | 可丢弃 backing 上定义故障模型；文件与目录同步前后 crash points | 恢复后完整性、已承诺状态、错误报告 | 未测；普通进程 kill 不等于掉电，采用独立 crash-consistency 设计 |
| 结论能否推广？ | 不同 CPU/存储/文件系统；真实 CLI 工具；不同内存预算 | 相同边界的曲线、资源与效应区间 | 当前固定平台 SDK 与普通 CLI 七工具 N5；跨硬件、跨文件系统和其他应用待测 |

开放负载的到达时间必须独立于前一个请求的完成，延迟从计划到达时刻开始，
包含发生器侧排队、背压和完成，保留失败/超时；否则系统变慢时会少发请求，
遗漏本应观察到的长等待。当前 worker 按完成推进，是闭环探针，不能报告开放
负载尾延迟。开放/闭环模型差异见
[Schroeder 等的原始论文](https://www.usenix.org/legacy/events/nsdi06/tech/full_papers/schroeder/schroeder.pdf)。

P99 需要大量带时间戳的操作和逐 trial 分布，不能从 5 或 30 个 cell 均值制造。
未来大样本操作也仍嵌套在独立 trial 内，不能把一次长运行中的每个操作当作
独立系统重复。重现性的系统曲线应先预定主指标、稳定区间、故障处理、独立样本
数和确认集，再测量；没有这些边界的更多点不能替代实验设计。

崩溃一致性需明确 fsync/目录 fsync、故障点和恢复模型，并在可丢弃输入上执行。
检查正常退出后的正确字节只覆盖正常完成，不覆盖所有故障后的承诺。可参考
[CrashMonkey/B3 原始工作](https://www.usenix.org/conference/osdi18/presentation/mohan)
设计独立验证；不能把本文的正常 fsync 曲线改名为 crash-durability 结果。

## 发布工程结论前的核对

报告应直接给出主结论、适用形态和退化点，再列协议、原始来源和失败。每个图
注明实际独立 N、三轮聚合方式、基线、profile 是否关闭、完成边界和缓存状态。
核对实际 trial/cell 库存、全部收据、制品摘要和预算；展示原始点以及确认批次
的独立来源，分开呈现诊断、资源与性能。

若仅完成 N5，就报告探索结果和未测项；若完成固定四点 N30，也只确认对应
单机机制条件。开放负载尾延迟、多 VM、崩溃耐久及跨硬件
范围需要各自证据。用户页、生产默认开关和 Agent 调度能力不会因这份手册或
某一条曲线自动改变。
