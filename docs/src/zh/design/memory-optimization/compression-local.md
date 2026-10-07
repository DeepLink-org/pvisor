# 实例内压缩

让可压缩冷内容由单个实例掌控，无需池化服务也能保存或恢复。
这种选择优先缩小所有权与故障边界，而不是追求跨实例共享。

## 目标与现状 {#status}

Linux x86_64 支持实验性的实例本地 live pager。默认关闭的
`VmSettings.cold_ram_compression`（`[vm].cold_ram_compression`）或
`--vm-cold-ram-compression` 启用后，runner 自动在私有匿名 RAM 上启动
pager，并使用 `LocalColdRamStore`。它不是采用 FUSE 压缩文件 backing 的
`vm.ram_compression`。guest 持续运行，无需 guest 应用参与；这不是普通
pause，也不是整 VM offload。

checkpoint 的独立保存、压缩和恢复仍是单独的架构要求。live 冷压缩当前
不能与快照捕获或恢复组合。macOS/HVF 堆冷池是另一条实验路径。

## 所有权与数据流 {#architecture}

![两次静止窗口、压缩发布与 userfaultfd 恢复](../assets/cold-page-cycle.svg)

实例协调器拥有[完整 checkpoint](../environment-snapshot.md)，
负责 CPU/设备状态、兼容性和持久存储。checkpoint 编码可以复用 codec，
但不采用运行态冷对象的生命周期或持久性合同。
仅保存压缩 checkpoint 不会缩减运行中的 RAM。

runtime 在 CPU 停驻、设备 lease 排空的窗口中捕获 64 KiB 块，每批发布
最多保留 4 MiB。编码与发布在此静止窗口外进行，guest 继续运行。第二次
静止窗口重新核对 live 字节，变化的块保持驻留。只有未变化且已有经过
校验的实例所有对象的块，才通过 `MADV_DONTNEED` 丢弃。pager 持有映射时，
balloon 空闲页报告仍被确认，但不丢弃 RAM，避免绕过 pager 的恢复所有权。

支持内核缺页的 userfaultfd 阻塞 CPU/KVM 及内核/设备对缺失 RAM 的访问。
runtime 解码并校验长度及 checksum 后执行 `UFFD_COPY`，确认完整复制后才
唤醒访问并释放冷引用。其他实例不能恢复或释放本实例的 store 引用。
guest 应用无需参与。冷对象不提供宿主崩溃后的持久恢复；映射错误或恢复
损坏会使 runner 失败，而不是静默暴露清零数据。

## 选择与取舍 {#tradeoffs}

本地存储让恢复摆脱发布 IPC 和服务可用性依赖，
但每个实例分别保留编码 payload，并承担自己的 codec 成本。
跨实例[去重](deduplication.md)不是正确性的前提；`vm.ram_dedup`
与此 pager 显式互斥。

适合的目标是长期不访问、可压缩的冷内容。计入对象开销与反复解码后，
热内容或不可压缩内容可能比驻留 RAM 更贵。
本地 store 拒绝 raw 块，以及满足
`encoded_bytes + 256 > decoded_bytes * 7 / 8` 的编码 payload。
预算或压缩拒绝会保留原 RAM。编码 payload 上限为配置 RAM 的一半，
对象数上限为 `ceil(configured_ram_bytes / 65536)`；这些分别有界，
不是进程 RSS 上限。

为稳定副本和编码预留临时内存，为解码页和 scratch 预留恢复余量。
后台工作必须让恢复优先；压缩率本身不能说明 CPU 成本或业务尾延迟。

## 实施方向与约束 {#direction}

Linux 要求 4 KiB 宿主页及普通私有匿名可写 RAM。builder 授权的不可变
raw 固件映射只有在指针、长度及 guest 拓扑均匹配时才被排除；未知 raw、
文件 backing/恢复 COW、shared 与 hugetlb RAM 被拒绝。设备/DAX 窗口不作为
候选。启用 `tee`、`aws-nitro`、`gpu`、`snd` 或 `input` 的构建被拒绝，
已有 device prepare、去重 advice 或第二个 pager 也被拒绝。
编译具备缺页能力不代表宿主已经授权。

内核缺页权限必须来自 userfaultfd syscall 或 `/dev/userfaultfd` 访问授权；
仅用户态缺页不足以支持 KVM 与内核 I/O。缺少权限时启动失败，不回退。
pVisor 不修改全局 sysctl。设备可用时，管理员可只为一个用户授权并撤销
（示例用户为 `reiase`）：

```bash
sudo setfacl -m u:reiase:rw /dev/userfaultfd
```

启用此功能的 VM 退出后撤销授权：

```bash
sudo setfacl -x u:reiase /dev/userfaultfd
```

准入拒绝与 `vm.ram_backing`、`vm.ram_compression`、`vm.ram_dedup`、
`vm.snapshot_filesystem_pool`、快照捕获/恢复及整 VM
[offload/FUSE backing](offload.md) 组合。本地压缩与外部 `vm.memory_pool` 二选一。
当前 Linux daemon 的 `--memory-pool` 使用 raw-page 物理共享：只读、大小封印的
memfd 槽位经引用固定后，由 VM 以 `MAP_PRIVATE` 映射；它不注册 userfaultfd，
也不压缩独有内容。两条路径复用采样/复核屏障，恢复方式和对象生命周期不同。
部署入口见 [daemon memory pool](../../guides/daemon/index.md#memory-pool)。

选择策略是实验性的驱逐/refault 探测，不是真正的读访问热度检测器：
字节不变仍可能被频繁读取。重新核对保护内容，不保证工作负载延迟。
历史的编码对象池与更广的客户端持有方案保留在[池化压缩](compression-pool.md)，
不能用它们替代当前物理池的合同；macOS 尚未交付等价的 daemon 物理共享路径。

## 证据边界 {#evidence}

[内存证据](proof-of-concept.md#memory-evidence)尚未建立已知的生产净收益。
比较宿主总占用、临时峰值、CPU 成本和业务尾延迟，不能只看编码大小。
所有权选择见[概览](compression.md)与[池化方案](compression-pool.md)。
