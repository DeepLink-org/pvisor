# 实例内压缩

让可压缩冷内容由单个实例掌控，无需池化服务也能保存或恢复。
这种选择优先缩小所有权与故障边界，而不是追求跨实例共享。

## 目标与现状 {#status}

checkpoint 保存、压缩和恢复相互独立于运行态优化，是架构要求，
不是开启运行态压缩后的附带能力。本地运行态路径仍是设计方向，
不是新交付的 Linux/KVM pager。实验性的 macOS/HVF 堆冷池
不能证明已有独立的本地压缩后端。

## 所有权与数据流 {#architecture}

实例协调器拥有[完整 checkpoint](../environment-snapshot.md)，
负责 CPU/设备状态、兼容性和持久存储。checkpoint 编码可以复用 codec，
但不采用运行态冷对象的生命周期或持久性合同。
仅保存压缩 checkpoint 不会缩减运行中的 RAM。

运行态压缩由 VM 选择冷内容、捕获稳定副本，并在丢弃原页前
持有经过校验的本地编码对象。codec 负责编码和有界解码；
backing 只是载体。`memfd` 不提供自动压缩，也不提供 pager。

CPU 访问或设备准备将当前字节恢复到私有可写 RAM。
运行时负责映射与设备访问，guest 应用无需参与。
冷对象不提供宿主崩溃后的持久恢复。

## 选择与取舍 {#tradeoffs}

本地存储让恢复摆脱发布 IPC 和服务可用性依赖，
但每个实例分别保留编码 payload，并承担自己的 codec 成本。
[去重](deduplication.md)是可选优化，不是正确性的前提。

适合的目标是长期不访问、可压缩的冷内容。计入对象开销与反复解码后，
热内容或不可压缩内容可能比驻留 RAM 更贵。
预期节省不足时，优先保留原 RAM。

为稳定副本和编码预留临时内存，为解码页和 scratch 预留恢复余量。
后台工作必须让恢复优先；压缩率本身不能说明 CPU 成本或业务尾延迟。

## 实施方向与约束 {#direction}

先建立非破坏性的本地存储与恢复，再在真实平台上接入
CPU/设备访问和回收。Linux pager 支持尚需验证；
macOS 尚未交付等价 sealed 方案。复用
[两阶段发布](proof-of-concept.md#two-phase-publication)中捕获、发布和提交的分离，
而不是旧池依赖服务恢复的合同。

保留冷池与整 VM [offload/FUSE backing](offload.md)的现有互斥；
仅有本地所有权不能证明这些机制可以安全组合。

## 证据边界 {#evidence}

[内存证据](proof-of-concept.md#memory-evidence)尚未建立已知的生产净收益。
比较宿主总占用、临时峰值、CPU 成本和业务尾延迟，不能只看编码大小。
所有权选择见[概览](compression.md)与[池化方案](compression-pool.md)。
