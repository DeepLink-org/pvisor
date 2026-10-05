# 共享工作集与惰性加载

本文把 Cluster 的资源利用方向明确为：**不可变环境与可共享基底只付一次成本，任务只为实际访问和私有修改付增量成本**。它连接已有环境缓存、Linux 原生恢复和调度机制，并提出下一轮实验与工程顺序；节点 owner 与 retained payload 总预算已按[服务整合设计](server-consolidation.md)接通；下面仍区分已实现机制、待扩展能力和未完成的性能验收。

## 分清三种收益 {#principles}

1. **内容去重**减少磁盘/对象存储中的重复字节，不自动减少 guest RAM。
2. **共享驻留页**要求可共享的同一 backing 身份与正确映射；从同一个镜像启动并不自动共享各 VM 的匿名 RAM。恢复到同一个只读 RAM inode 并使用 `MAP_PRIVATE`，才为未修改基底提供直接的共享路径。
3. **惰性加载**减少当前阶段没有访问的读入与解码，把成本留给后续访问。它可能改善 ready，却增加首次工具执行的 fault/I/O；因此要同时测首个有用结果和完整任务完成。

优先使用已经知道身份和所有权的不可变内容共享。任意匿名页扫描、冷页压缩和整 VM offload 是另外的策略，不能把它们的数字合成一种收益。

## 当前代码已有的基础 {#current}

| 路径 | 已有机制 | 当前边界 |
|---|---|---|
| 不可变环境 lower | 配置 node socket 后跨 Worker 共用同身份 mount，连接 pin 与强引用保温有界，任务各有 private upper | 未接入 Node 时保留 Worker 内 `Arc<MountedImage>` 复用；节点故障不支持 live 接管 |
| 镜像 lazy cache | 文件按需读取，客户端内容块上限 64 MiB/4096 条；分页索引默认 64 KiB 页、256 页 LRU；内容跨镜像 CAS 共享 | Node 统一热块、分页 metadata 与 decoded RAM 的 retained payload 额度；完整 metadata、scratch、外部 Arc 和 kernel pages 不在该计数内；磁盘容量回收仍需单独策略 |
| Linux native restore | Node 按封存 ID/compatibility 跨 Worker 与授权 store 复用只读 RAM inode；guest 使用 `MAP_PRIVATE` COW | 未接入 Node 时复用仅在 supervisor 内；普通新启动不经过 RAM restore，兼容性/无网络 profile 合同不变 |
| 快照 RAM lazy reader | RAM fault 按块校验/解码，小缓存保留四个 decoded 块，kernel page cache承担主要 decoded 复用 | 读取和解码的首次访问代价需测；旧 raw 格式没有块索引时仍可能需要完整校验 |
| 缓存亲和 | Controller 在有界候选窗口中按 `cache_keys` 与环境 layer handle 命中数排序 | Worker 当前上报来自静态 `--cache-key`；不是实测页驻留、完整块命中率或跨节点最短就绪时间调度 |
| 冷 RAM 压缩池 | 独立的实验性内容去重与冷页恢复机制 | `vm.memory_pool` 显式限制 macOS/Apple Silicon；不能作为当前 Linux Cluster 已具备后台冷页池的证据 |

代码位置：`bin/worker/environment.rs`、`image/cache/lazy.rs`、`image/cache/portable/binary.rs`、`executor/vm/restore_ram.rs`、`environment_snapshot/lazy.rs`、`pvisor-vm/src/memory.rs`、Controller `scheduler.rs`。

FS/S3 原生 cache 的已发布对象可分页按需读；普通 OCI cache 服务端在首次 `prepare` 返回前仍完整准备未缓存镜像。两种冷路径必须分开。`checksums.bin` 等控制对象仍会先读，分页不能被描述成“完全不读取未访问内容的任何元数据”。

## 目标成本模型 {#model}

对使用共同模板的一组任务，希望实际内存接近：

```text
M(N) = base services
     + shared resident working-set union
     + sum(private dirty RAM + private upper + per-VM runtime overhead)
     + bounded decoded caches and in-flight I/O
```

共享集合按实际 resident 页/对象计一次，私有部分随 N 增长。共享项也会随工作集、环境版本和访问模式增长，不能假设永远是固定常数。写入比例高或任务访问集合互不重合时，收益会下降。

读取成本目标是“必要控制对象＋访问元数据页＋所需数据块＋明确预算内的预取”，而非每个任务扫描/拉取整个环境。记录读放大、解码放大和 COW 放大；更大的传输块、页预读和 copy-up 可能使实际读取超过请求字节。

逻辑 RAM reservation、物理占用与可回收 cache分别记账。看到 PSS 或 RSS 降低，不能直接乐观削减准入 RAM 配额；容量策略需经过峰值、私有写入和并发 miss 检验。

## 应补上的工程连接 {#integration}

**验证已接入的跨 Worker 节点 owner。**同环境任务共用只读 mount；兼容恢复共用只读 RAM owner。环境身份包含 handle/digest，RAM 身份包含封存 ID/compatibility；每次申请校验授权 store 与有效发布，再用连接 pin 保护活动对象、有界强引用保温。正常 teardown 后释放，GC 仍遵守已有提交根与 pin 合同；不放开可写映射。部署方式见[统一服务指南](../../guides/cluster/service.md)。

**扩展现有 payload 预算的覆盖与观测。**Node 已统一 retained cache payload、owner 数和准备并发；完整进程内存仍由 delegated cgroup 封顶。统计 metadata、内容块、RAM decoded cache、kernel驻留、临时解码和在途 I/O。节点预算至少约束可管理的缓存、mount 数和在途峰值；kernel页缓存通过宿主内核限额与观察管理，不能承诺完全由用户态 cache 精确控制。保温 mount应有字节/数量/TTL或驱逐策略，而不是每个任务多保留一个缓存。

**按对象身份复用并合并相同 miss。**先测多个 task是否在读同一个有效对象时产生重复下载/解码，区分已有文件锁复用、per-mount热缓存和跨 revision共享。新增 single-flight需要明确 key、请求取消、错误重试、预算和 owner生命周期；错误数据仍须拒绝，热缓存不能复活已撤销的发布引用。

**小量预取启动关键路径。**根据冻结的访问 trace，预取启动必须的索引页、可执行文件/库或恢复页，并限制总字节和在途数。其余按需读取。预取必须与纯 lazy和完整 materialization对照，以首个有用结果与任务完成为准，不因 ready更早就认为获益。

**让缓存亲和反映实际观察。**可提出带新鲜度的“已发布/已挂载/本地块/驻留”分层 hint，并将排队和可用资源一起考虑。当前是 Worker主动 poll与候选排序，不是全局预测式节点选择；协议与调度扩展属于后续工作。hint通过内存视图与 Worker对账收敛，不为每个 cache命中新增持久化日志。

通用 warm-template Agent启动也不是现成能力：现有恢复保持源命令、输入、环境和策略合同，不能任意替换任务定义；动态工作需要单独设计受控的启动/输入交接。当前无网络 native restore不能直接当作网络模型 Agent完整恢复。先验证已有不可变环境 lazy路径和兼容 fork场景，扩展能力另行验收。

## 测量前先冻结的五个问题 {#experiments}

下面是待冻结实验草案，未新增测试结果。全部延续最多四个真实沙箱、每沙箱 RAM/vCPU/CPU时间限制及宿主内核硬上限；A/B顺序执行，不通过清理宿主全局 page cache干扰其他任务。

| ID / 问题 | 对照与变量 | 主指标与否定条件 |
|---|---|---|
| S1：共同基底能否降低每个 fork的边际物理 RAM？ | 相同封存字节与任务，1/2/4 分支；共享/独立 backing，以及 eager/lazy分别控制；固定非零工作集，改变私有写比例 | 对整组 task与 owner计内存、native RAM PSS/shared-clean/private-dirty、COW字节、首个结果/完成时延；私有写隔离不成立则失败，PSS仅因未访问减少不能算共享收益 |
| S2：大环境的小工作集能否按需加载？ | 同一已发布版本的 eager/lazy；增加未访问文件/字节，固定任务访问与输出；应用缓存冷、磁盘热、同mount热分别记录 | ready前/首个结果前/完整任务的读入、GET/range数、metadata与内容RAM、读放大；只是推迟全部成本或完整任务更慢则不支持整体收益 |
| S3：并发重复 miss会不会变成重复读入/解码？ | 至多四 task同步访问同一对象与各自不同对象；保持工作量、cache预算和后端条件一致 | 每个唯一对象的 origin请求/解码次数、在途峰值和受阻任务时延；区分预读/重试与重复工作，不能仅凭缓存hit数判定 |
| S4：有界关键页预取能否降低 cold路径尾延迟？ | 纯lazy、有界trace预取、完整materialization；同版本/输出/总预算 | 首个有用结果与任务完成、总读取/解码/内存峰值；预取增加无用工作或miss队列干扰，可能否定策略 |
| S5：保温与缓存亲和能否提高固定预算有效吞吐？ | 同任务到达轨迹与节点预算，现有策略对照有界保温/新鲜缓存hint；共用环境与不同环境均测试 | 正确完成吞吐、CPU与内存时间积分/任务、排队与尾延迟；更多cache占用、队列集中或冷租户饥饿可能抵消收益 |

S1 的 eager/shared等消融组合目前没有完整公开开关，必须先建保留同等校验与所有权的受控 harness；不能把不同 rootfs/输出/兼容性配置当成开关。源 VM完成封存并退出后再启动最多四个分支；若源必须继续存活，分支最多三个，**源也计入四台上限**。

缓存冷实验使用私有 endpoint/prefix/cache目录并计数；宿主文件页缓存若未控制，就标记未知/热，不称为物理磁盘冷。一次版本发布和模板封存的成本单独记录，再按真实复用次数摊销。S1固定共享数据内容与访问量，避免把零页优化或压缩率误当物理共享；S2固定数据内容与校验输出，避免减工作量制造lazy收益。

验收含块/页校验、COW私有写隔离、版本固定、owner/pin释放与GC正确性、慢/失败对象读取下的租约响应。成本计入实际cache/pager服务，而不是只求VM进程RSS之和。具体负载、样本数、最小有意义收益及容许延迟代价需在正式采样前冻结；现在不能给PASS或预报倍数。

## 应绘制的曲线 {#plots}

- **S1：整组物理内存 vs分支数**，每张图固定工作集和写比例，分别标明共享/独立与eager/lazy；另画COW私有页随写比例的变化，证明收益来源。
- **S2：首个有用结果前的读入与时延 vs环境总量**，任务访问集固定，再画完整任务对应值；应用冷/磁盘热/同mount热分组，识别是否只是推迟读取。
- **S3/S4：受阻时延与在途峰值 vs并发miss数 / 预取预算**，保持任务输入与输出校验不变，报告实际读取、工作量与读放大。
- **S5：正确完成吞吐、CPU秒/任务、内存时间积分/任务 vs复用次数**，总资源预算固定，把环境发布和模板封存成本加入摊销账目。

既报告稳态收益，也报告首次使用成本和达到摊销盈亏平衡所需的复用次数；达不到盈亏平衡就是有效反例。控制组无法安全实现或条件不能匹配时，曲线缺项并标记未测，不用其他配置补点。

## 优先级 {#priority}

先做 **S1 现有共享 RAM/owner复用**和 **S2 现有环境 lazy路径**，确定 pVisor实际已有机制的收益。随后用 S3解释并发瓶颈，再决定节点保温、预取与动态cache hint是否值得实现。最后在[固定预算有效工作实验 Q3](../cluster-benchmark-plan.md#q3)中验证整套机制的组合收益。

上一轮最小目录rootfs、全新VM启动、每VM独立Worker的探针没有指定不可变环境或恢复引用，绕开了 S1/S2 的核心路径；保留为基础成本参照，不能用它否定或证明共享与惰性加载收益。

相关合同见[镜像cache](../../reference/shared-image-cache.md)、[共享镜像存储](../shared-image-cache-storage.md)、[冷RAM池](../memory-sharing/index.md)、[生命周期](lifecycle.md)及[资源准入](scheduling.md)；这些不同路径的支持范围仍分别成立。

服务如何收敛见[Cache、Memory Pool 与 Cluster 服务整合](server-consolidation.md)：统一节点 owner、预算和部署入口，区分可重取 cache 与不可丢失的活动 RAM，并保留 Controller 独立重启边界。
