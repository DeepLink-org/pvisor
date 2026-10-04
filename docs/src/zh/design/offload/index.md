# VM RAM offload

## 1. Motivation {#motivation}

Agent 执行环境经常需要在两次交互之间保留进程状态。停止 VM 可以释放资源，却也丢失了尚未持久化的内存状态；只暂停 vCPU，又会继续占用 RAM。offload 用于这段等待期：保持 VMM 和设备状态，把 RAM 同步到文件，并请求操作系统回收驻留页，下一次交互仍由原 VM 继续执行。

文件 backing 可以保存原始 RAM，但其磁盘成本会随着写入增长。压缩模式进一步利用零页、重复字节和可压缩块减少存储量，以额外的 CPU、读写和暂停时间为代价。设计因此需要同时解决三个问题：运行中的访存如何保持正常语义，何时可以提交稳定的 RAM 数据，以及恢复时怎样读取少量需要的块。

当前范围是同一 VMM 的 offload/resume。CPU、设备、连接和时钟状态仍留在进程中；RAM 文件不能独立重启 VM，也不提供跨节点迁移。

## 2. 核心设计 {#core-design}

### 文件映射作为运行中的 RAM

VMM 以 `MAP_SHARED` 映射文件，vCPU 和 virtio 设备继续通过原有地址访问 RAM。内核处理缺页和写回。offload 同步文件并请求回收页；resume 继续使用同一文件身份。这里没有在每次 offload 时复制一份 RAM，也没有因为改变路径而重新建立宿主映射。

普通模式的逻辑 FD 和物理 FD 指向同一个原始 RAM 文件。压缩模式把两者分开：物理 FD 指向 `vm.ram` manifest，逻辑 FD 指向 FUSE 的 `mount-*/ram`。FUSE 读取时解码已提交块，写回时把页存入 staging。

| 对象 | 职责 |
|---|---|
| `VmControl` / `RamBacking` | 控制交换，持有 FD、选定路径和存储生命周期 |
| VMM / device memory gate | 暂停 CPU，阻止并排空设备访存，回收和恢复映射 |
| `CompressedMount` / `RamFs` | 将压缩存储适配为可 mmap 的逻辑文件 |
| `CompressedRam` | 管理 manifest、页暂存、dirty mask 和提交 |
| `SnapshotChain` | 编码与校验 generation，解析继承，合并和 GC |

### manifest、generation 和 staging

`vm.ram` 在压缩模式下只保存目录和 head，不保存 RAM payload。已提交内容保存在不可变 `.pvdelta` 中；第一次生成完整 base，后续 generation 只保存变化块并引用 parent。运行中尚未提交的数据保存在内核脏页或 staging。

不可变 generation 避免覆盖仍被读取的旧数据。提交先持久化 generation，再追加并同步 manifest head。这样 head 发布之后引用的数据已经存在。staging 保持可写，适合连续的小页更新；它不承担历史保留。

每个非均匀的 64 KiB 块独立使用 zstd level 1 压缩，并通过 seek table 定位。零块和其他均匀字节块只保存 fill。读取一个块不需要解压整个 RAM，但当前解码粒度仍是完整块。

磁盘目录、逐文件大小、schema 和字节图见[文件格式](disk-layout-and-schema.md)。[SVG 全集](assets/overview.svg)展开 manifest、generation、原始 RAM、staging 和 pin 的内部结构。

## 3. 关键数据和核心机制详细设计 {#detailed-design}

### 文件身份与目录 {#files-and-references}

```text
/data/
├── vm.ram                    压缩模式为 manifest；普通模式为原始 RAM
└── vm.ram.layers/            仅压缩模式，权限 0700
    ├── .tmp<staging>         可写原始页
    ├── <id>.pvdelta          不可变 base 或 delta
    ├── <id>.pvpin            可选历史保留根，空文件
    └── .tmp<capture>         新 generation 写入期间的临时文件
<用户cache>/pvisor/ram/
└── mount-<随机名>/ram        FUSE 逻辑 RAM，仅压缩模式
/exports/
└── idle.ram                  可选硬链接，指向原 backing inode
```

显式 backing 文件以 0600 创建，目标必须不存在；未指定路径时使用 `dirs::cache_dir()/pvisor/ram` 下的临时文件。压缩 sidecar 位于显式路径追加 `.layers` 的目录，或 cache 下的临时 `layers-*` 目录。

`offload(Some(path))` 使用硬链接发布原 backing。host 验证源路径的 dev/ino 与所持 FD 一致，拒绝覆盖和跨文件系统目标，再更新 selected path。runner FD 和 mmap 保持不变。压缩 alias 只链接 manifest，descriptor 继续引用原 sidecar；不会创建与新 alias 同名的 layers 目录。

临时 compressed backing 发布后，原 layers 目录通过 `keep()` 转为持久保留。原临时 manifest 名称在退出时删除，alias 继续存在。Run Bundle、OverlayFS upper 和 OCI cache 不属于这组 RAM 文件。

### 页暂存与块读取

staging 是稀疏原始文件，byte offset 与紧凑逻辑 RAM offset 相同。每个 64 KiB 块用一个 `u16` 标记其中 16 个 4 KiB 页是否有效。第一次部分覆盖某页时，writer 从旧 chain 补齐其余字节；完整页覆盖直接写入。

读取先取得 committed block，再覆盖 staging 中的有效页；完整 staged block 可以跳过祖先读取。未写的初始 RAM 返回零。dirty mask 只存在内存中，因此 staging 文件本身不足以重建未提交 epoch。

4 KiB 暂存页、64 KiB 压缩块与主机页是三个不同粒度。VMM 按主机页对齐 file offset，mincore 也以主机页采样；Apple Silicon 主机页常见为 16 KiB。设备匿名共享窗口不进入 RAM backing。

### 从运行到 offload，再恢复 {#lifecycle}

启动时，executor 创建 backing 和可选 FUSE mount，安装初始 guest lower 排除规则，复制 RAM FD 给内部 runner。VMM 设置文件长度、建立共享映射；host Connection 持有 backing，runner 持有 VMM 和控制线程。

普通 pause 只停止 vCPU，不关闭设备 gate，也不提交 generation。要取得稳定 RAM epoch，offload 必须继续排空设备访问和内核写回。

| 阶段 | 实际动作 |
|---|---|
| 路径准备 | host 可选创建 alias；发生在控制请求发送前 |
| CPU 静默 | runner 持 transition 锁，暂停所有 vCPU |
| 设备静默 | 释放 VMM mutex 后关闭、排空 memory gate，避免等待 I/O 时阻塞 VMM worker |
| 映射回收 | macOS 解除 HVF RAM 映射；对宿主 file-backed RAM 同步并请求回收 |
| runner 回复 | 返回 Offloaded 和驻留采样；CPU、gate 仍保持暂停/关闭 |
| host 完成 | 同步逻辑 FD，提交 compressed store，等待 worker 并校验 outcome |
| resume | macOS 恢复 HVF 映射，打开 gate，再恢复 vCPU；缺页按需读取 backing |

macOS 使用 `msync(MS_SYNC | MS_INVALIDATE)` 和 `madvise(MADV_DONTNEED)`；Linux 使用 `MS_SYNC` 并额外请求 `posix_fadvise(DONTNEED)`。宿主 MAP_SHARED 地址仍保留。vCPU 转换共享 3 秒截止预算，设备排空为 5 秒；host offload 交换预算 300 秒，pause/resume 为 10 秒。

runner ack 后 host 仍有存储工作，所以调用者应以完整 operation outcome 为完成点。控制 stream 的 frame 是 4 字节大端长度加 JSON，最大 16 KiB。调用者丢弃 future 不会让交换任务放弃读 ack，以免后续请求读到旧回复。

### 提交与文件校验 {#format-and-publication}

manifest 长度为 `44 + N + 80k`：8 字节 magic、4 字节 descriptor 长度、N 字节 JSON、32 字节校验，再加 k 条 80 字节 head。generation 由 Zstd frames、seek table、metadata JSON 和 64 字节 footer 构成。

ID 是 metadata 原始 JSON 的 SHA-256；metadata 中还有各解码块的 checksum。读端同时检查格式、继承关系和块内容。JSON 中的 ID 是 32 个整数的数组，文件名使用 64 位十六进制表示。完整字段与 schema 见[格式定义](disk-layout-and-schema.md#detailed-design)。

| 顺序 | 动作 |
|---|---|
| 1 | 逻辑 FD `sync_all`，将内核脏页写回 staging |
| 2 | 从 staging 和 parent 合成块，写临时 generation |
| 3 | generation fsync，no-clobber 发布，layers 目录 fsync |
| 4 | 追加 manifest head，再 fsync manifest |
| 5 | 更新内存 chain，清 dirty mask，truncate staging |
| 6 | 解析 current 与 pins 的祖先，删除不可达 generation |

FUSE fsync 只执行 `flush_writes`，不会发布 head；稳定 epoch 的 generation commit 由 offload 协调。已提交 head 之后的 GC 失败记录警告，不撤销提交。generation 或 head 写入失败会使 store 标为 failed，不能把它当可继续重试的正常 writer。

### 增量继承、合并与 GC {#worked-example}

```text
H1(base)  = A1 B1 C1
H2(delta) = B2，parent H1
H3(delta) = A2，parent H2
当前 RAM  = A2 B2 C1
```

`resolved` 索引记录每个块最后的覆盖来源。dirty 块最终 checksum 与 parent 相同时可以省略；无变化 delta 可以复用 head。

parent depth 达到 8 后，下一次 capture 生成完整 base，parent=null。合并需要读取全部逻辑 RAM、解压旧块并重新编码，发生在 VM 暂停的 offload 路径中。这一轮的成本接近全量保存，即使最近修改很少。

GC 保留 current head 及所有 `.pvpin` 所指 head 的祖先。旧 manifest 记录不是保留根；合并后的旧链若未被 pin，可以删除。标准 offload 不自动 pin，也不提供从旧 head 重建 VM 的接口。

### 退出与失败 {#ownership-and-cleanup}

正常退出等待 child，取消或截止先终止进程树，再 detach control。backing 释放时尝试 unmount，删除 staging 和临时 mount 目录。未发布的临时 backing/layers 清理；显式文件、alias 和已 keep 的 layers 保留。

退出没有额外 generation commit。最后一次 offload 后 resume 所产生的新写入可能随 staging 清理而丢弃；持久 manifest 只描述最近一次 committed head。检查应在 writer 停止后通过只读 `CompressedRam::open` 进行。

已知问题集中在路径、状态和清理时序：新 alias 还没有持续安装初始 backing 的 guest 可见性约束；Offloaded 后 pause 可以报告 Paused，但资源仍保持 offloaded 条件；300 秒 Tokio 超时不能停止 blocking commit，worker 完成与卸载/删除之间还需明确同步。manifest 撕裂尾部当前直接报错。64 GiB layout 许可与 64 MiB JSON index 预算也尚未完全协调。

### 源码位置 {#source-map}

| 文件 | 内容 |
|---|---|
| [control.rs](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor/src/executor/vm/control.rs) | backing 所有权、路径发布、控制交换和 host 提交 |
| [supported.rs](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor/src/executor/vm/supported.rs) | 启动、FD 传递、guest 排除、runner 控制线程 |
| [ram_file.rs](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor-vm/src/ram_file.rs) | FUSE 文件与回调 |
| [ram_backing.rs](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor/src/ram_backing.rs) | manifest、staging、dirty mask、提交 |
| [image.rs](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor/src/ram_backing/image.rs) | generation 编码、继承、pin、合并和 GC |
| [VMM ram.rs](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor-vm/src/vmm/ram.rs) | RAM 映射、同步、回收与驻留采样 |
| [libkrun](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor-vm/src/handle.rs) / [memory gate](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor-vm/src/devices/virtio/memory_gate.rs) | CPU 与设备静默顺序 |

## 4. 实验数据支撑 {#experiments}

下面的数据来自 2026-10-03 的定向测试和对已有磁盘产物的检查，基线为 `e5359307` 的产品路径及 `1b97a9ef` 的文档 helper。它们不覆盖随后出现的未提交冷页池实验代码。

| 检查范围 | 结果 | 说明 |
|---|---:|---|
| backing、控制协议、RAM 存储与 Core 合同 | 31 通过 | 包含随机部分写、重复提交/合并、损坏拒绝、超时与 ack 校验 |
| device memory gate | 4 通过 | Reader/Writer 凭证、排空、重新开放和多 VM 独立性 |
| 宿主 RAM 映射/回收 | 2 通过 | 回收后共享数据保持、设备窗口排除、无效 backing 拒绝 |
| offload 拒绝后的路径副作用 | 1 通过 | 复现先发布的硬链接在转换失败后仍存在 |

这 38 项检查支持存储与控制层的若干不变量，尚不能证明真实 guest 的端到端组合。六个 VM 文档用例在准备 socket 时遇到 EPERM；直接 SDK 驱动也在 AgentCtl 初始化时受阻，未进入 guest。记录保存在 `review_project/06-evidence/offload-20261003/`，不能把这些阻断计为 VM 通过。

已有 256 MiB compressed RAM 样本中，manifest 逻辑长度 999 B，base 约 21.45 MiB，delta 约 1.85 MiB；两代文件实际分配总量为 23.30 MiB，另需计 manifest 和目录。样本有 10 条 head 记录，GC 后只保留一个 base 和一个 delta。[逐字段大小核对](disk-layout-and-schema.md#experiments)给出了原始整数与文件位置。

这组数据说明格式与目录大小可以按公式核对，也展示了一次具体工作负载的存储结果。它没有测量物理内存节约、首次缺页恢复延迟、合并暂停时间或多 VM 密度。目前也没有足够数据判断 level 1 或固定 8 层合并是否最合适。

`resident_before_bytes / resident_after_bytes` 是 mincore 对 file-backed 映射的采样；`backed_bytes` 是覆盖范围。host 压缩提交发生在 runner 采样之后，还会读取和分配内存，因此这些字段不能代替 RSS、physical footprint 或完整 offload 后的系统内存测量。

## 5. 使用建议 {#usage}

需要在同一节点保留 VM、等待一段时间再继续执行时，可以考虑 offload。若只需短暂停止 CPU，pause 更直接。当前文件不适合承担关机后恢复、跨节点迁移或最终退出状态归档。

先用普通 backing 验证控制与恢复，再评估压缩模式。压缩要求 Linux FUSE 或 macFUSE kernel backend；backing 和 alias 应放在 guest 不可见的宿主私有目录，同一文件系统，并保留 manifest 所引用的 sidecar。调用者应等待 operation 成功后再使用发布路径。

测量时分别记录普通 delta 轮次和合并轮次的暂停时间、CPU、磁盘峰值及恢复延迟。合并会同时占用旧链和新 base，pin 会进一步延长历史保留；大 RAM、不可压缩数据和频繁重写尤其需要测量。现阶段不应把较小的样本文件当作容量规划依据。

需要保留最后一轮 RAM 数据时，显式完成 offload，再结束 writer；不要依赖正常退出自动提交。跨进程只读检查应避开活动 writer，目录清理应按整个 backing 的所有权处理，不能独立删除 parent generation。

### 与实验冷页池的关系 {#experimental-integration-boundary}

未提交工作树中的 `PVISOR_EXPERIMENTAL_MEMORY_POOL` 路径使用 Unix socket 和内存压缩对象池恢复部分冷块，与磁盘 generation 的 whole-VM offload 不同。当前接入拒绝与 FUSE compression 同开，也拒绝在实验 preparation/pager 开启时做 whole-VM offload。inventory JSON 仅用于诊断。这些实验不在上述测试与使用建议的验证范围内。其当前机制和实验结果见[内存去重与冷页压缩](../memory-sharing/index.md)。
