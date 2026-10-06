# 内存压缩

当编码节省超过存储、CPU 和恢复成本时，压缩可以降低保留冷 RAM 的成本。
先选择实例本地所有权；只有跨实例共享足以抵偿协调成本时，才引入池化服务。

## 目标与现状 {#status}

目标设计区分[实例内压缩](compression-local.md)与
[池化服务器压缩](compression-pool.md)，两者都不是生产收益承诺。
当前 macOS/HVF 堆冷池仍属实验，服务丢失可能导致依赖它的 VM 失败；
客户端持有不可变 backing 的方案尚属提案。

## 所有权与数据流 {#architecture}

每个实例独立负责 checkpoint 保存、压缩和恢复，不依赖运行态优化。
[完整环境快照](../environment-snapshot.md)包含机器状态与持久性合同；
运行态冷对象只保留当前 RAM 片段。压缩 checkpoint 不会自动回收活跃 RAM。

VM 运行时负责冷页选择和映射变更；codec 转换字节，backing 承载字节。
`memfd` 不会自动压缩。即使对象共享，目标设计也将可写页恢复到实例私有 RAM。

## 选择与取舍 {#tradeoffs}

本地压缩避免服务协调，但不能跨实例共享 payload。
池化增加可选的[去重](deduplication.md)、共享存储和宿主级预算，
代价是 IPC、竞争和更广的故障风险。
两者都需要临时内存与恢复余量、CPU 预算，以及可接受的尾延迟。

## 实施方向与证据 {#direction}

先证明独立恢复能力，再回收 RAM。Linux sealed `memfd` 交付仍是提案，
尚需验证 pager；macOS 的等价 sealed 方案尚未交付。
保留冷池与整 VM [offload、FUSE 压缩 backing](offload.md)的现有互斥。
从[优化架构](index.md)出发；[内存证据](proof-of-concept.md#memory-evidence)
尚未建立已知的生产净收益，也未验证净物理内存节省。
