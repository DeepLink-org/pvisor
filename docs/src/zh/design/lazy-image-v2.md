# 惰性镜像 V2：压缩目录索引与内容池

V2 是服务器、文件系统、S3 与 Docker 本地池共同使用的存储格式。所有文件系统元数据进入可惰性加载的索引；小文件按目录聚合，数据对象只保存内容与定位表。实现入口为 `crates/pvisor/src/image/cache/portable/`。

| 项目 | 当前合同 |
| --- | --- |
| 存储版本 | `format_version=2`、`pvisor-radix-v2`、`pvisor-v2:` 句柄 |
| 路由 | basename 原始字节的固定 seed `0x707669736f727632` 的 XXH3-64；每层 4 bits |
| 完整性 | SHA-256；快速路由 hash 不承担完整性校验 |
| 压缩 | 独立 Zstd level 3 frame，标准 seekable 尾表 |
| 迁移 | V1 读取器与发布器已删除；从 OCI 重新发布到空目录／前缀 |
| 性能证据 | [原生 Docker 对照](../lab-notes/image-format-performance-analysis.md)；存储、启动与内存分开测量 |

## 元数据与目录边界 {#architecture}

每个目录只管理自己的直接子项。普通目录使用一个压缩叶 frame；叶节点的未压缩编码超过 64 KiB 时，按名称 hash 的下一个十六进制位拆成最多 16 个子节点，必要时递归拆分。目录是局部性边界，不是不可拆分的最小 frame：海量小文件目录仍会拆分。完整 64-bit hash 冲突由有界的原始名称范围节点处理，精确名称比较决定文件是否存在。

叶记录 inline 保存名称、类型、大小、mode、uid/gid、inode/nlink、mtime/nsec、symlink 目标、内容 SHA-256 和内容引用。子目录记录指向该目录自己的索引根。路径、名称与 symlink 目标保留原始字节，不要求 UTF-8。没有通配符、概率存在判断或全局逐文件位置表。inode 与内容身份分离，相同内容不会合并不同文件的 inode。

索引与数据 frame 的 payload 使用固定依赖版本 bincode 2.0.1 的 standard 编码（little-endian、变长整数），控制对象保持 JSON。外层 format marker 与句柄版本决定解码器，不能仅改版本字段来转换旧对象。

分支记录保存子 frame 的位置、压缩／解压长度、编码字节 SHA-256 和条目数。父节点在逻辑上包含子树，但不会把已经压缩的子 frame 再压缩一遍。所有 frame 按子节点在先、父节点在后的顺序写入一个 `index.bin`；父子关系通过引用表达。读取器用引用直接 Range GET，不为一次 lookup 下载全局 seekable 尾表。小型 `root.bin` 保存根记录，受 COMMIT 认证；它连接整棵认证树，替代 V1 的全局页校验目录。

目录枚举按照 `(XXH3(name), name_bytes)` 排序，分页最多 256 项；continuation 是绑定目录的 opaque cookie，调用方不得把它解释成文件下标。每页同时返回完整属性。遍历使用子树计数跳过无关节点，而不是为每页从头下载整个目录。

## 内容池与小文件定位 {#content}

| 参数 | 当前值／含义 |
| --- | --- |
| 小文件阈值 | 普通文件 ≤64 KiB；空文件无内容对象 |
| pack 目标上限 | 去重后原始 payload 1 MiB，最多 4,096 项 |
| 数据 frame | 原始 payload 目标 16 KiB，最多 1,024 个 slot；单个 ≤64 KiB 文件保持完整 |
| 大文件块 | 独立切成最多 1 MiB 的原始内容块 |
| 压缩粒度 | 单独 frame；每个对象带标准 seekable 尾表 |

发布时按目录归组，组内按内容 SHA-256 排序打包。元数据的 hash 分桶与数据打包独立，避免用随机名称路由破坏目录内容局部性。同一 revision 的相同小文件内容只存一份；跨目录重复内容归属确定性扫描中第一个目录。已有 HEAD 中的 pack 位置按内容身份复用，所以 rename、跨目录移动或 chmod 可以只产生新索引，而不重写数据对象。不同发布历史不保证获得相同的 pack 组合；跨镜像部分重叠的小文件集合也不保证逐文件物理去重。

索引中的小文件引用为 `blob + frame_id + slot_id`，没有逐文件 byte offset。blob 的布局 frame 保存数据 frame 引用，数据 frame 内的 slot 表保存各文件的偏移。读文件时验证布局与目标 frame，再通过 slot 取内容并验证文件 SHA-256。定位表是内容布局，不含路径、权限、时间戳等文件系统元数据。

小文件对象为 `data/packs/<aa>/<bb>/<encoded-blob-sha256>`。大文件块为 `data/sha256/<aa>/<bb>/<plaintext-chunk-sha256>`，同样使用新的 framed 编码；相同大文件块可跨路径、镜像复用。单文件修改可能增加一个新 pack，旧 pack 会因旧 revision 的引用继续保留；当前没有在线 GC。

## 发布、打开与失败语义 {#publication}

```text
format.json
meta/<image-key>/identity.json
meta/<image-key>/platforms/<platform>/HEAD.json
meta/<image-key>/platforms/<platform>/revisions/<revision>/
  manifest.json  config.json  root.bin  index.bin  objects.bin  COMMIT.json
meta/<image-key>/platforms/<platform>/uploads/<upload-id>/
  plan.json  progress.json
data/packs/<aa>/<bb>/<encoded-blob-sha256>
data/sha256/<aa>/<bb>/<plaintext-chunk-sha256>
```

一次打开固定 image-key、platform 和 revision。发布顺序为内容依赖、索引与配置、COMMIT、最后 CAS 更新 HEAD；HEAD 才是可见性提交点。已有不可变对象须验证后复用，损坏对象不覆盖。HEAD 冲突明确失败，发布失败可能留下未引用对象；丢失 ACK 按原有事务规则核对结果。`objects.bin` 是离线依赖清单，不是打开时必读的全局表。

打开只读取控制对象、根描述与根目录 frame。路径查找逐层取相关 frame；内容 Range 读取和解压不持有全服务锁。同一 key 的冷 miss 合并，最多 128 个不同的并发 miss，超限明确失败；不能把网络、鉴权或校验失败当成 ENOENT。远端编码字节先校验再解压，限制输入、窗口与输出；本地编码缓存损坏后重新获取。

| 资源 | 限制 |
| --- | --- |
| 每镜像条目／内容跨度 | 1,000,000／2,000,000 |
| 单 metadata 对象／控制对象 | 64 MiB／1 MiB |
| 单解压 frame | 约 1 MiB（大文件块及编码开销）；普通索引叶目标 64 KiB |
| 每个已加载镜像索引 LRU | 16 MiB 解码驻留预算／4,096 项 |
| PortableCache 内容 LRU | 64 MiB／4,096 项 |
| PortableCache 已加载镜像 | 4 个 |

预算随最后一个活动引用释放，不因淘汰而提前解除计量。一个 `index.bin` 仍有 64 MiB 上限，达到条目上限之前也可能因长名称／属性而拒绝发布。发布器的元数据规划仍为 O(N) 内存，编码内容临时落盘、八个上传 worker 有界；没有实现外部排序或无限容量索引。

## 文件系统接入与本地存储 {#filesystem-access}

Docker pull/build、无 daemon 的 spawn 和 cache server 都写同一个 `<image-store>/pool-v2`；默认目录和覆盖规则见[本地镜像目录](../reference/shared-image-cache.md#local-image-store)。本地池通过 `pool-format.json` 标识，但与文件系统／S3 使用同一压缩物理格式，不再套一层专用 PVPOOL 编码。普通池读取只保留有界解码内存，没有持久的完整解压镜像副本。非池直接读取器可把编码 frame 保存在 `<user-cache>/pvisor/cache-v2/objects/`；磁盘缓存没有总量淘汰配额。

VM direct lower 与 host FUSE 共用读取器。VM 使用私有元数据／稀疏文件投影；普通 guest READ 从后端读取，copy-up、完整导出与校验可能读取整个文件。guest mmap 继续走 virtio-fs 和 guest page cache，没有新增 DAX。远程 VM 适配器原有的已校验文件块缓存仍可能持久保存原始块，应与本地池区分。镜像首次准备仍须下载、应用 OCI 层；本格式不实现 registry layer 的远端按需解压。

任意 xattr、ACL 与安全标签未纳入格式。本地池导入拒绝无法表示的 xattr；直接发布也不提供 xattr／ACL 保真合同。保留现有 ownership/mode override；macOS 本地池只忽略宿主 provenance 属性。协议 1／2、连接池与 reader prefetch 的开关属于传输层，与存储格式 V1 的退役无关。

V1 handle／format／pool marker 会返回明确错误，不原地改写，也不自动删除旧目录。新 pull 可以从 OCI 构建 V2，已经固定旧 handle 的容器需要重新创建。旧目录型实例仍按其原有路径处理。RAFS 不与此格式二进制兼容；标准 seekable framing 也不意味着可以直接挂载为 Nydus 镜像。

## 缓存边界与成本模型 {#cache-costs}

压缩存储与远程按需供给共享格式，但承担不同成本。Docker-style 本地池在 pull 完成后已有全部编码内容；远程客户端只在镜像服务完成准备后，才可以按文件读取。后者没有把首次 OCI 下载、解包和发布成本消除，只把它移到服务端并供客户端摊销。

| 路径 | 持久存储 | 首次读取 | 后续读取 |
| --- | --- | --- | --- |
| 本地 Docker-style 池 | 全部压缩索引与数据，无完整解压树 | 加载目录 frame，验证并解压内容 frame | 有界解码 LRU、guest page cache；新实例仍需启动 VM |
| 直接 filesystem／S3 | 发布端完整编码池；客户端可缓存已取编码 frame | 控制对象、相关索引与数据 Range | 编码缓存和解码 LRU；缓存目录无总量淘汰配额 |
| cache server／远程 VM | 服务端编码池及可能保留的 OCI staging；客户端索引与已验证原始文件块 | RPC 元数据和实际文件内容；传输内容可能已解压 | 新 VM 可复用客户端块缓存，VM 内另有页缓存 |

因此本地磁盘节省率不能直接当作远程传输节省率。文件型服务返回的原始 Read 数据可能大于 OCI 压缩 layer；完整扫描、校验、导出或 copy-up 会放大读取工作集。小文件聚合减少存储对象数量，不保证减少同等数量的远程文件 RPC。元数据 inline 让一次命中取得属性，但冷 lookup 仍需解析父路径和相应叶 frame。

容量主要减少的是常驻解包副本；索引、定位表和 pack 布局会产生开销，编码池不保证小于原始 OCI 压缩 blobs。不同机制的贡献需要独立实验，不能仅凭完整库存的差值给每个优化分配收益。

`mmap` 不会直接映射压缩 blob。guest 缺页由 virtio-fs READ 填充页缓存，后端验证与解压发生在这个路径上；热页复用可以掩盖冷 miss 成本。顺序遍历、随机首次触页、热重复读取和可写 copy-up 是不同负载，不能由一个启动探针推断全部 mmap 性能。缓存的解码预算也不等于整个 VM、服务或宿主的内存上限。

## 收益验证与适用范围 {#validation}

[惰性镜像基准](../benchmarks/lazy-image-startup.md)负责用户选型：分别统计本地完整库存、预准备实例、远程冷／热客户端以及大工作集的读取代价，主对照是同 digest 的原生 Docker。容量包含双方元数据和正常内容共享；服务端准备与客户端等待分开计量。客户端冷表示私有缓存为空，不表示宿主页缓存冷；本机 loopback 结果不能外推 WAN。

[原生 Docker 工程记录](../lab-notes/image-format-performance-analysis.md)保留完整启动／退出和整组内存口径。独立 guest kernel、容器内存 limit 和 VM guest RAM 不等价；较少的镜像磁盘占用不构成启动、每实例内存或吞吐全面领先的证据。V1 存储格式不再作为当前收益的主对照，历史传输优化开关也不能与存储格式版本混称。

尚需独立验证真实 S3／WAN、并发启动、随机 mmap 工作集、写入／copy-up、导入峰值及长期缓存回收。发布器 O(N) 规划、单索引对象上限和无在线 GC 仍是扩展边界，不能用静态压缩率推导无限镜像数量或长期磁盘稳定性。
