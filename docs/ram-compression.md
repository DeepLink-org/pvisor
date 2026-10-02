# 压缩 RAM backing：Seekable base/delta（PVZRAM v2）

> 本次为源码重构。未编译、未执行测试、未启动 KVM/HVF。
> 存储核心已采用不可变增量文件；VM 接入仍使用 FUSE 兼容适配器。
> 当前 generation 是文件写入 epoch，不是跨 vCPU/设备的一致性 VM 快照。

## 启用与文件所有权

```toml
[vm]
ram_backing = "/data/session.pvram"
ram_compression = true
```

CLI：`--vm-ram-compression --vm-ram-backing /data/session.pvram`。
显式路径必须不存在；同时创建权限 0700 的 `/data/session.pvram.layers/`。
manifest、generation 和临时 staging 文件均为私有文件；manifest/generation 0600。
省略路径时由 attempt 持有临时 manifest 和 layers 目录，正常释放时清理。
通过 offload 发布新路径时，manifest 使用同文件系统硬链接，临时 layers 转为保留。
**保留文件不代表重新启动 VM 的能力。** 当前只读检查器可以读取已提交的 RAM。

manifest 内记录 layers 的绝对路径。它与 sidecar 构成一个 bundle，不能只复制
manifest、移动目录或删除父层。不同路径的 manifest 硬链接仍引用原 sidecar。
显式 bundle 和已发布 bundle 的保留、归档与删除由调用方负责；没有隐式 GC。

## 三层职责

```text
地址 / 生命周期层
  ├── RamLayout：Guest region、逻辑页大小、紧凑 RAM 布局
  └── 当前 VM 兼容适配器：逻辑文件偏移、写回、offload 确认
          ↓
Block 层
  ├── 稳定 block ID；64 KiB，region 末尾允许短块，不跨 region
  ├── ZERO / SAME；其余每块一个独立 Zstd frame
  └── SHA-256、变更块集合、继承解析
          ↓
Generation 存储层
  ├── 不可变 base / delta 文件，紧凑追加 payload
  ├── 标准 Zstd Seekable 数据区与 seek table
  └── 元数据、footer、内容身份、发布与同步
```

`SnapshotChain::capture` 接收真实 `RamLayout`、父链、完整 dirty block 集合和
稳定 epoch 的块读取函数。布局变化拒绝增量，需要单独创建新的 base。
`restore_to` 支持完整恢复紧凑 RAM 字节，调用方必须静止所有目标访问者。

VM supervisor 当前不知道 runner 内的完整 Guest region 表，因此 FUSE 适配器
使用一个从 0 开始的**逻辑 backing region**；这个地址不是 Guest 物理地址。
不能将其当作跨进程迁移所需的真实 Guest 布局。直接存储 API 支持多 region，
之后接入 runner 地址层时需要显式传递真实布局。

## 参考设计与取舍

- [Zstd Seekable](https://github.com/facebook/zstd/blob/dev/contrib/seekable_format/zstd_seekable_compression_format.md)：独立 frame、尾部 seek table。
- [QEMU mapped-ram](https://www.qemu.org/docs/master/devel/migration/mapped-ram.html)：稳定逻辑页位置、RAMBlock 和写入 bitmap；这里不复制固定物理偏移。
- [QEMU fast snapshot load](https://www.qemu.org/docs/master/devel/migration/fast-snapshot-load.html)：缺页与后台恢复必须协调页面归属；当前尚未实现这个 pager。
- [Firecracker memory](https://github.com/firecracker-microvm/firecracker/blob/main/src/vmm/src/vstate/memory.rs)：region 布局，以及 vCPU/用户态写入都参与增量追踪。
- [zram](https://www.kernel.org/doc/html/latest/admin-guide/blockdev/zram.html) / [zswap](https://www.kernel.org/doc/html/latest/admin-guide/mm/zswap.html)：零/同值内容省略与压缩对象组织，不复制其内核分配器。

首版不建立 storage-page 分配位图、可变 extent、hole punching 或在线碎片整理。
压缩对象紧凑连续排列，允许跨磁盘页；不能每个小对象都额外占一个 4 KiB extent。
原 v1 可变块容器已替换，v2 不向后读取 v1 文件。

## 增量语义

```text
S0（base）: 所有有效逻辑块
S1（delta）: 相对 S0 的变更块新内容
S2（delta）: 相对 S1 的变更块新内容
```

delta 缺项继承父层。ZERO/SAME 是显式替换，不能与缺项混淆。
一个块只要有局部修改，就保存该块完整新内容，不做 XOR/字节差分。
SHA-256 与父层解析内容相同时可省略这个候选 dirty block。
没有实际变化时复用旧 head，不生成空 delta。

打开链时构建 `block_id → layer + entry` 的解析索引，读取不逐层搜索。
每个 generation 的 layout 必须相同，base 必须完整覆盖所有逻辑块。
父层缺失、循环、重复 block、非法 frame 和布局不一致均报错，不默认为零。

链达到 8 层后，下一次有写入的提交读取所有最新逻辑块，生成独立新 base。
当前合并会解码并重新编码，没有实现直接复制编码对象。VM 适配器在新 head
持久化后回收不可达 generation；保留当前链及所有 `.pvpin` 指向的历史链。
`SnapshotChain::pin/unpin/collect_unreachable` 必须由目录所有者串行调用；直接
`capture` 不隐式删除历史层。未 pin 的历史 ID 不是持久保留承诺。pin、当前链、
合并期间的新 base 与 staging 仍会占空间，回收不等于容量配额。
读取拒绝超过 32 层的外部链。逻辑 RAM 限制 64 GiB、最多 1024 regions；每层
JSON 元数据预算 64 MiB，实际可容纳 RAM 还受这个预算约束。

## Generation 文件格式

```text
0
├── 独立 Zstd frames（ZERO/SAME 不占 payload）
├── 标准 Seekable skippable frame / seek table
├── JSON 元数据（UTF-8）
└── 固定 64 字节 footer
```

文件开头到 `metadata_offset` 是完整的 **Zstd Seekable 数据区**。
整个外层文件不是直接可交给通用 Seekable reader 的文件；读取器应限制到这个
数据区，或先提取它。外层元数据和 footer 位于 Seekable 数据区之后。
不使用自定义 RAW payload：不可压缩内容也由 Zstd frame 承载，以保持该区兼容。
Seekable 的每 frame checksum flag 为 0，外层索引使用 SHA-256 校验原始内容。

### 标准 seek table

所有整数 little-endian，遵循官方 Seekable 格式：

| 内容 | 长度 / 值 |
|---|---|
| skippable magic | u32，`0x184D2A5E` |
| frame_size | u32，`8 × frame_count + 9` |
| 每 frame 项 | compressed_size u32、decompressed_size u32 |
| frame_count | u32 |
| descriptor | u8，0 |
| seekable footer magic | u32，`0x8F92EAB1` |

### 外层 JSON 元数据

| 字段 | 语义 |
|---|---|
| version | 2 |
| block_bytes | 65536 |
| parent | 父 image ID 的 32 字节数组；base 为 null |
| layout.page_bytes | 4096..65536 的 2 次幂 |
| layout.regions | 按 Guest 地址排序的 `{guest_address, length}` |
| entries | 按 block ID 严格升序的本层替换项 |

每个 entry：`{block, frame, fill, checksum}`。
`frame` 与 `fill` 恰好一个存在；fill=0 为 ZERO，其他字节为 SAME。
frame ID 按 payload 顺序连续编号，长度由 region/block 几何决定。
checksum 为解码内容的 SHA-256。JSON 是磁盘格式，不直接序列化 Rust 内存布局。

image ID = SHA-256(元数据的实际 UTF-8 字节)。它通过各块 checksum 绑定内容，
并通过 parent ID 绑定继承关系；不是签名或防回滚机制。
文件名为 `<image_id 的小写十六进制>.pvdelta`，不从磁盘加载任意父路径。

### 固定 footer：64 字节

| 偏移 | 长度 | 字段 |
|---:|---:|---|
| 0 | 8 | `PVSNAP2\0` |
| 8 | 8 | metadata_offset |
| 16 | 8 | metadata_bytes |
| 24 | 8 | seek_table_bytes（含 skippable header） |
| 32 | 32 | image ID |

footer 必须处于 EOF，metadata_offset + metadata_bytes + 64 必须等于文件长度。
读取限制 frame 输入到 64 KiB + 1024 字节，输出到一个逻辑块，要求恰好一个
Zstd frame、输出长度精确一致，并校验 SHA-256。未知字段/版本与异常几何拒绝。
元数据和索引在打开时验证；payload 在实际读取时验证。

## Manifest 与提交协议

manifest 为稳定 inode，使现有 FD 与 offload 硬链接接口无需切换对象。

```text
magic PVZRAM\0\0（8 字节）
  → descriptor_bytes（u32 LE）
  → JSON {version: 2, directory: absolute_path}
  → SHA-256(descriptor)（32 字节）
  → 追加 head record（每项 80 字节）
```

head record：`PVHEAD2\0`（8）、logical_bytes（8）、image ID（32）、
SHA-256(前 48 字节)（32）。最多接受 100 万条记录。
只读检查器必须排除并发 manifest 提交；它没有与 writer 协商读取锁。

提交顺序：

1. 在 layers 目录创建临时私有文件，顺序写 payload、seek table、元数据、footer。
2. fsync generation；以不覆盖既有文件的方式发布内容命名文件；fsync layers 目录。
3. 将新 head 追加到 manifest，fsync manifest。
4. 切换内存中的解析链，清除 dirty 集合，截断 staging。
5. 先解析全部 pin 根及祖先，再删除不可达 generation，fsync layers 目录。
   pin 根缺失或损坏时停止回收；回收失败记录告警，不撤销已经持久化的 head。

同 ID 文件已经存在时验证其元数据和所有本层 payload，不能盲目复用。
失败不清除未提交的 dirty 集合，不返回 offload 成功；适配器进入失败状态，
随后由已有控制协议取消 attempt。可能留下未引用的 generation，需要外部维护。
旧 generation 本身不被改写。

manifest 追加不是 crash-atomic WAL。尾部 record 撕裂会被拒绝，不自动跳过，
已 pin 的旧 image ID 可直接打开此前的 generation。宿主崩溃后的 VM 恢复还需要 CPU、
设备和磁盘状态，此格式没有提供。fsync 的持久化能力也依赖平台/文件系统。

## 当前 VM 接入边界

FUSE 适配器继续提供 cached mmap，Linux 需要 `/dev/fuse` 与挂载权限，macOS
需要 macFUSE kernel backend。没有 SIGSEGV pager 或运行时地址替换。

读写请求仍由同一 FUSE 线程串行处理。稀疏 staging 按 4 KiB 页写入，每个
64 KiB block 用 16 位 mask 标记已暂存页；第一次局部页修改只补齐该页，
后续局部写直接覆盖输入区间，整页写不读取父块。读取时合并父块与暂存页。
压缩单位仍为 64 KiB；部分页读取仍可能需要解码一个父块。没有完整 RAM
用户态缓存，但 **staging 峰值仍可能等于全部已写 RAM**，原文暂存写入仍存在。

FUSE flush/fsync 只同步 staging 与 manifest，不产生 generation。offload 顺序：

1. 暂停全部 vCPU；关闭设备 RAM 门禁，等待 descriptor、Reader/Writer 和
   vsock packet 持有的访问引用排空。网络 TX 的地址列表也保留引用。
2. 等待时释放 VMM 锁，让请求所依赖的 VMM worker 能完成。排空限时 5 秒；
   失败保持关闭并禁止继续控制，由 pVisor 取消 attempt。
3. 门禁关闭期间完成各 region 的 msync/reclaim，再由宿主 fsync 逻辑文件
   排空 FUSE 写回。随后在 blocking worker 提交一次 generation 并回收旧层。
4. 完成提交后才能报告 offloaded；显式 resume 恢复 HVF 映射并打开门禁。

等待排空时允许已有访问下的嵌套 queue 操作；一旦引用数归零，后续访问全部
等待 resume。忙碌设备可能导致排空超时。普通 pause 保持原有仅暂停 vCPU 的
语义。GPU/audio/input/TEE 构建拒绝 offload，因为其特殊访存尚未纳入门禁。
Linux 请求回收 manifest 与 sidecar 文件缓存；macOS generation 写入 F_NOCACHE。

压缩不占用 FUSE 请求线程；提交仍持有 store 锁，但设备 RAM 访问此时已经关闭。
多 VM 压缩并发尚无独立全局预算，8 层合并仍会增加暂停时间。实际 Guest 布局
在 VM 适配器中仍以紧凑文件区间表达。RAM generation 不包含 CPU、设备或磁盘
状态，不能独立恢复整台 VM；驻留归零与平台性能仍需真实运行验收。

## 检查与验收

源码中已补未执行的检查：多 region/短块、base/delta 继承、零覆盖、局部写入、
无变化 head 复用、只读重开、完整恢复、8 层合并、旧 generation 保留、缺失父层、
损坏 payload、截断 footer/manifest、错误索引与 Seekable 表、随机写入多次提交。
新增未运行检查覆盖页级暂存/邻页保持、flush 不发布、pin 祖先保留与回收、
缺失 pin 根阻止删除、descriptor 消费后的访问引用保持及 resume 唤醒。
S-DOC-062 通过真实 VM 两次 offload 检查合法 bundle、新 generation、逻辑读取、
实际 bundle 磁盘占用和 guest 固定/可变内存完整性。规格仍为 UNREVIEWED。

待验收：Linux/KVM、macOS/HVF、FUSE 写回/服务中断、磁盘满和同步失败、清理
顺序、增量收益、staging 峰值占用、内存回收量、合并成本和恢复 P95/P99 延迟。
