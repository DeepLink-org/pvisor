# 池化服务器压缩

在授权实例之间共享编码后的冷内容，同时让每个客户端掌控恢复。
只有共享存储与编码工作节省的成本超过协调和服务开销时，池化才值得采用。

## 目标与现状 {#status}

Linux 已实现实验性 sealed 压缩池，通过
`pvisor-daemon serve --memory-pool --memory-pool-mode compressed` 显式选择。
默认池模式仍为原文物理页共享。压缩对象交付、去重、预算、断连引用生命周期
和服务退出后恢复已有进程级测试。2026-10-18 在获得内核缺页 userfaultfd
授权后，Linux x86_64/KVM 双 VM 正确性探针通过，覆盖池终止后的 guest
discard/refault、私有写入和文件 I/O；没有生产密度或延迟验收结论。

macOS/HVF 实验性堆冷池仍是独立路径：服务在堆中保留 payload，
服务丢失可能导致依赖它的 VM 失败。macOS 尚未交付等价 sealed backing。

## 所有权与数据流 {#architecture}

服务器索引、编码并管理内容预算，不接收 guest 指针或机器快照发布权限。
每个实例按[环境快照合同](../environment-snapshot.md)独立负责完整 checkpoint 发布。

VM 捕获稳定的 64 KiB 冷块。池按解码内容计算哈希，复用前比较完整字节；
相同内容在会话间共享一个不可变编码 memfd，独有可压缩内容使用 fill 或 zstd。
客户端通过 SCM_RIGHTS 接收只读描述符，要求 write/grow/shrink/seal 封印、
有界帧、身份匹配与完整字节相等。只有完成交付并再次复核 live bytes 后，
pager 才能 discard 原 RAM。`memfd` 本身不会压缩或去重。

CPU 或设备访问通过本地解码、checksum 校验与 `UFFD_COPY` 恢复私有可写 RAM。
restore 和本地 release 不执行 IPC；释放通知在 publication/stats 中分批发送，
pager 空闲维护也会发送。必须具备内核缺页 userfaultfd 权限；缺少权限时
VM 启动失败，不回退或修改全局策略。

交付并校验成功后，服务退出会永久停止新发布，但已持有句柄仍可解码。
交付前失败则保留原 RAM。意外断连后，服务端引用和连接名额继续计入预算，
直到 pidfd 证明 peer 退出。遗留 socket 不会被替换；没有服务重连或宿主崩溃恢复。

## 选择与取舍 {#tradeoffs}

相比[实例内压缩](compression-local.md)，池可以避免会话间重复 payload 和重复编码。
共享限于授权信任域，内容摘要不构成授权。这是编码对象共享，与默认的
[物理页池](deduplication.md#physical-pool)分别维护。

默认上限为 512 MiB 按页计费的编码 backing、32,768 个对象、32 个连接及
每连接 32,768 条引用。编码字节计入帧头及 4 KiB 文件页向上取整；每个对象
在取整并加上保守的 256 字节元数据余量后，至少节省 1/8 才被接纳。
预算拒绝或压缩收益不足时保留原驻留内容。每个对象及交付引用还消耗描述符，
操作系统描述符上限也可安全拒绝发布。索引、线程、VM 元数据及在途 scratch
在 payload 上限之外。池内存须独立于 sandbox 准入预留，并保留解码和恢复 RAM 余量。

传输失败后，stats 统计本地仍持有的字节、对象和引用；跨会话共享未知，报告为零。
日志 `pvisor-sealed-pool publication_disabled=true accounting_scope=local-pins`
标记统计范围变化。连接正常时统计全池 backing 与本会话引用。
这些计数不等于物理内存节省。

## 验证与约束 {#direction}

`just test pvisor-daemon` 包含真实池子进程、跨会话共享、终止服务后本地恢复、
遗留 socket 保留及 pidfd 确认客户端退出后的引用回收。
`just test pvisor` 覆盖 codec、不可变描述符校验、内容比较、预算及传输所有权。
这些测试不执行 guest 回收或 KVM/设备 refault。

依赖破坏性回收前，须验证 sealed store 下的 guest 读写和设备 I/O。
现有 Linux pager 提供两阶段发布与恢复，编码和 IPC 位于 VM 冻结窗口之外。
恢复仍可能在串行 store 锁上等待在途发布；本地解码不能证明恢复优先调度或尾延迟验收。

独立双 VM 正确性探针先等待池中出现已知共享和逐 guest 的 payload 块，
终止服务，再检查 guest 内存与 virtio-fs 文件往返。使用全新短输出路径和本地固件：

```bash
cargo build --locked -p pvisor-daemon --example compressed_pool_case
PVISOR_CASE_VM_LIBRARY_DIR=/absolute/firmware \
  PVISOR_EXPERIMENTAL_MEMORY_METRICS=1 \
  target/debug/examples/compressed_pool_case /tmp/pv-cmp-check
```

`result.json` 记录结果与逐 guest 恢复计数。成功要求回收字节数大于零，
服务丢失日志之后恢复字节数继续增长，且两个 guest 均完成摘要、私有写入和
文件检查。文件写入先于完整 payload 摘要读取。2026-10-18 的运行在获得宿主
授权后通过；此前的权限失败仍是独立失败记录。探针最多启动两个 256 MiB/单
vCPU VM 和一个 32 MiB 池，不验证 OpenSandbox API/bootstrap 或测量物理节省。
五个依赖权限的 Linux pager 测试也已通过，覆盖真实 KVM、内核 I/O 和 balloon
报告：

```bash
just test pvisor-vm -- -E 'test(cold_ram_linux::)' --run-ignored all --test-threads=2
```

保留冷 pager 与快照、文件/FUSE backing、整 VM [offload](offload.md)
及 KSM advice 的互斥约束。池模式绑定持久配置；切换需排空旧实例并使用新状态目录。

## 证据边界 {#evidence}

[内存证据](proof-of-concept.md#memory-evidence)尚未建立已知的生产净收益，
也未验证净物理内存节省。历史池测量不能验证 sealed backing。
[架构索引](index.md)与[压缩概览](compression.md)区分实现、组件正确性，
真实 guest 正确性探针，以及仍需完成的业务负载验收。
