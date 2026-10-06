# 卸载文件格式

存储布局决定哪些内容可以读回、发布和安全删除。
原始文件偏重简单；压缩 base/delta bundle 则用额外复杂度换取复用。

## 两种存储模型 {#storage-models}

原始 backing 按 VMM 的紧凑文件 offset 保存 RAM 字节，没有 pVisor 格式头。
它可以直接映射，但逻辑长度不等于磁盘分配量：文件可能是稀疏的。
原始 RAM 文件和压缩 bundle 都不包含恢复完整 VM 所需的全部状态。

压缩模式的 manifest 标识 sidecar 目录和 committed head。
不可变 generation 保存完整 base，后续 delta 继承未变化的块。
可写 staging 保存新的原始页；FUSE 将这些页与已提交内容组合，
形成活动实例使用的逻辑 RAM 文件。staging 不是历史快照。

## 随机读取与完整性 {#reads-and-integrity}

块索引解析链中最新的数据来源，seek table 定位对应压缩 frame。
均匀块使用 fill entry。读取校验 metadata、祖先关系、解码长度和块
checksum，无须解码整份 RAM。完整性校验检测损坏，不等于完整
VM 检查点，也不代表 CPU、设备和外部副作用的原子捕获。

当前 staging 使用 4 KiB 页，压缩使用 64 KiB 块；宿主页是另一粒度。
小粒度减少部分写入与冷读取放大，却增加索引和管理开销。
大粒度可能提高压缩率、减少 metadata，但一次小访问也可能解码整个
块。[本地压缩](compression-local.md)讨论相关的内存与 CPU 权衡。

## 发布与保留 {#publication-and-retention}

writer 先将逻辑文件写入刷到 staging，构建并同步新 generation，
再以不覆盖已有 generation 的方式发布，最后追加 manifest head。
仅有 FUSE 写回或 fsync 不会发布 committed epoch；该边界由卸载协调。
不可变性保持已发布字节稳定，但本身不保证对应文件一直存在。

当前 GC 保留 current head 及祖先，以及显式 pin 的历史保留根。
旧 manifest head 记录不是保留根，标准卸载也不会自动 pin。
读者租约是未来并发检查或可移植 bundle 所需的生命周期约束，
不是该格式新承诺的租约能力。当前检查要求 writer 已停止。
独立的[完整环境快照](../environment-snapshot.md)存储有自己的恢复租约。

## 合并与容量 {#compaction}

delta 减少重复存储，却加深依赖。当前按链深触发的合并在 VM 暂停时
构建新完整 base，解码旧块并重新编码全部逻辑 RAM。
容量预算应同时计入旧链、正在构建的新 base、staging 和 pin 保留历史。
不要把同一新 generation 的临时文件与发布后的名称重复计为两份数据。
小 delta 不能预测合并延迟、空间峰值或冷读取的解码放大。

## 当前限制与设计方向 {#status}

manifest 当前保存 sidecar 的绝对路径；复制或建立 alias 不会迁移
它的依赖。open 拒绝撕裂的 head log 尾部，而不是自动回退。
staging 有效页掩码只在内存中，内核脏页也可能尚未进入 staging；
正常退出不会提交恢复后的新写入。这些文件不是崩溃原子的完整 VM 归档。

可移植引用、可安全恢复的发布以及协调读者与 GC 的租约是未来目标，
不是这些文档确立的能力。[架构](index.md)与
[内存卸载](offload.md)说明范围；现有[字节布局与 schema 附录](../offload/disk-layout-and-schema.md)
负责定义二进制字段、大小公式与解析限制，不在这里重复。
