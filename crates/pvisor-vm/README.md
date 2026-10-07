# pvisor-vm

pVisor 的 Rust VM 运行时。VMM、设备、架构支持和虚拟化后端在同一个 crate 中编译，对外使用统一的 Rust API。

## 必须遵守的 API 边界

1. **唯一对外入口是 `pvisor_vm::api`。** `lib.rs` 只公开 `api`；其他模块属于私有实现，不允许其他 crate 直接访问，也不通过另一个公开模块绕过这个边界。
2. **`api` 只放接口和调用契约，不放实现。** 对外的数据结构、trait、类型与接口导出在这里集中定义。**全部公开方法签名在 `api.rs` 的 trait 中声明**，不允许在私有模块通过 `impl Struct { pub fn ... }` 另行增加公开方法。私有模块直接实现这些 trait；方法体、后端分派、校验、资源管理和系统调用留在内部。
3. **`api` 不做条件编译。** 不以操作系统、CPU 架构、硬件后端或 Cargo feature 改变公开类型、字段、方法或签名。所有支持平台使用相同的数据结构定义和方法定义。
4. **差异由 crate 内部消化。** aarch64/x86_64 的启动布局与 HVF/KVM 的运行能力分别建模；后端 trait、泛型和条件编译用于内部实现；公开契约 trait 在 `api.rs` 中统一声明。调用方通过统一能力描述了解支持范围；不可用操作返回明确的不支持错误，不要求调用方复制后端条件。
5. **契约必须写清楚。** 接口文档说明输入校验、资源所有权、调用顺序、生命周期、线程与同步要求、错误后的状态，以及快照冻结、发布和恢复的职责边界。
6. **底层验证也守住边界。** 需要访问寄存器、设备队列或内部状态的测试放在本 crate 内；其他 crate 的测试使用公开 API。不能为了测试重新公开实现模块。

API 中的 `PermissionSemantics`、`RamReclaim`、`RamMappingSnapshot` 和内存回调类型也是内部运行时使用的唯一类型定义；内部不复制等价记录。快照恢复验证和 COW 映射共用 RAM 清单及文件大小校验，映射过程另外检查拓扑和宿主页对齐。

统一的 Rust API 不表示不同架构之间可以互相恢复快照，也不表示机器已具备虚拟化权限。能力描述、运行时安装证据和持久化兼容性校验是不同契约。

## 调用方与运行时的职责

调用方负责独立 runner 的监管、宿主沙箱安装、继承描述符校验、rootfs 的独立副本、快照存储事务、host/boot/build 兼容性与单次执行所有权。

运行时负责配置所有权、架构启动布局、设备和 CPU 生命周期、事件循环与 VM 控制。虚拟文件持有自身字节，网络持有自身描述符。恢复机器先保持暂停；冻结或设备排空失败后保持停止推进，调用方必须终止失败的 runner。

源码来自 libkrun 1.19.3 组件及仓库已有适配；来源、许可证和既有同步记录保存在 `provenance/`。底层操作系统 FFI 与固件数据 ABI 仍是私有边界，不属于公开 VM 控制接口。

VM 文件系统经 guest virtio-fs 和 virtqueue 直接调用宿主 runner 内的
`pvisor-overlay-core::service::FilesystemService`。host FUSE 使用同一服务；
VM staged 和 lazy image 均不建立中间宿主文件系统 FUSE 挂载。lazy 镜像的
元数据、按块校验读取和缓存由 `pvisor` 的 `image/cache/backend.rs` 提供，
通过私有元数据投影保留现有本地 FD、路径检查与快照合同。该投影没有挂载，
文件内容通过后端读取；copy-up 和完整快照导出会补齐所需内容。
协议 inode/handle 表和平台权限仍留在入口适配器中。

## 核心接口模型

所有定义集中在 `src/api.rs`，实现集中在私有适配器中。文件系统配置使用 `PathBuf`，UTF-8 校验及内部设备格式转换由运行时完成。

| Trait | 实现 struct | 契约 |
| --- | --- | --- |
| `RuntimeSupport` | `VmPlatform` | 能力、日志、本地固件发现及冷 RAM 诊断 |
| `VmConfiguration` | `VmBuilder` | 在启动前接收配置与资源所有权 |
| `VmRuntime` | `VmBuilder` | 消费配置，启动一个独立 runner 的 VM |
| `VmControl` | `VmmHandle` | 查询、暂停、恢复、RAM offload；可用于动态分派与 mock |
| `SnapshotControl` | `VmmHandle` | 在完整冻结窗口执行带类型返回值的动作 |
| `SnapshotCapture` | `FrozenMachine` | 捕获 CPU、RAM 和设备状态；冻结 guard 不可逃逸 |
| `ColdRamControl` | `VmmHandle` | 冷 RAM worker、冻结、驻留查询和缺页处理 |
| `RamDedupControl` | `VmmHandle` | 显式 Linux KSM advice 与逐映射安装结果 |
| `ColdRamStore` | 宿主存储适配器 | 不可变 block 的发布、校验恢复和引用释放 |
| `RamFileMapping` | `RamFileMount` | mmap 兼容 RAM 文件与 FUSE 生命周期 |
| `RamFileStore` | 宿主存储适配器 | 有界暂存 I/O，与代际发布区分 |
| `FrozenMemory` | `FrozenMachine` | 冻结窗口内的内存操作 |
| `SnapshotState` | `MachineSnapshot` | 快照清单查询和文件系统副本绑定 |
| `RestoreState` | `MachineRestore` | 恢复校验和私有 RAM 映射 |
| `RamAccess` | `RamBlock` | RAM 块访问与 unsafe 前置条件 |

`VmConfig` 表达 CPU 与 RAM 配置；`Capabilities` 统一描述架构、虚拟化平台及可用原语。`MachineSnapshot`、`MachineRestore` 与 `RamMappingSnapshot` 表达快照边界；`OverlayConfig` 与 `PermissionSemantics` 表达文件系统输入。所有 trait 仅声明方法，不提供默认实现。

`VmBuilder.inner` 仅保存内部状态，不承担对外接口定义。`VmmHandle` 直接保存私有运行状态，在 `handle.rs` 实现 `VmControl`。Rust 不支持无方法体的固有方法声明，因此接口使用 trait 声明，私有适配器使用 `impl VmConfiguration for VmBuilder` 等实现。调用方显式导入所需契约：

```rust
use pvisor_vm::api::{VmBuilder, VmConfig, VmConfiguration};

fn configure() -> std::io::Result<VmBuilder> {
    VmBuilder::from_config(VmConfig { cpus: 2, memory_mib: 256 })
}
```

## 运行时与存储边界

CLI 执行器、containerd shim、暂停/恢复、快照、checkpoint、RAM pager、示例和 guest-init 基准均使用本模块的契约。核心运行时组件只在本 crate 内编译；调用方使用 `pvisor_vm::api` 的 Rust 契约，不使用 C context/裸指针配置入口。内部硬件探针和设备测试归属 `pvisor-vm`，不要求外部访问私有模块。

RAM 文件的 FUSE 挂载、readiness、mmap 缓存 I/O 和卸载顺序由私有 `ram_file` 模块管理。`RamFileStore` 接收宿主的暂存存储实现；`RamFileMount` 只公开所有权与挂载契约。压缩代际提交和持久化发布由宿主存储层负责。

冷 RAM pager 的状态机、采样/发布窗口、回收线程及 CPU/设备缺页恢复也在本 crate 内。`ColdRamStore` 是宿主存储适配契约：pVisor 负责实例本地 store 或受支持平台的 pool 授权、传输和诊断目录，VM 指针与映射状态不越过边界。`ColdRamOptions`、`ColdRamControl` 在所有平台都存在；不支持的后端返回明确错误，重复启动同一 VM 的 pager 会被拒绝。

Linux x86_64 支持实验性的 runtime-owned userfaultfd pager，由默认关闭的 `VmSettings.cold_ram_compression` / `--vm-cold-ram-compression` 自动启动，使用 pVisor 的 `LocalColdRamStore`，不是 FUSE `vm.ram_compression`。`ColdRamControl::start_cold_pager` 使用相同 API；Linux 的缺页与静止窗口由 runtime 内部持有，外部 `install_ram_fault_handler`、`with_ram_quiesced`、`experimental_ram_residency` 及 `FrozenMemory::experimental_ram_blocks` 返回不支持。编译能力不代表权限：必须具备 syscall 或 `/dev/userfaultfd` 的内核缺页授权，缺少授权时启动失败，不回退或修改全局 sysctl。

pager 仅接受 4 KiB 宿主页上的普通私有匿名可写 RAM；严格匹配身份与拓扑后排除 builder 授权的不可变 raw 固件，拒绝未知 raw、文件/COW、shared 和 hugetlb RAM，排除设备窗口。`tee`、`aws-nitro`、`gpu`、`snd`、`input` 构建及已有 device prepare/dedup advice 被拒绝。实例本地压缩使用 64 KiB 块，Linux daemon 池使用独立的 4 KiB 页；每批最多暂存 4 MiB；两次 CPU 停驻/设备 lease 排空窗口分别捕获与复核，编码发布期间 guest 继续运行。持有校验对象后才 discard；refault 校验长度、checksum 与完整 `UFFD_COPY` 后唤醒访问。实例本地 UFFD 压缩的 balloon 空闲页报告仍确认但不 discard；物理共享模式的协同回收见下文。此策略是驱逐/refault 探测，不是真正的读访问热度检测器，也不是普通 pause。

Linux pager 在进入采样窗口前，仅持有 pager 锁检查 cold 状态与 cooldown；没有候选时不进入 CPU/设备 barrier，但仍检查 VM 退出。首次 barrier 仍必须停驻 CPU 并排空设备 lease，才能安全复制 live RAM；存储编码/发布不持有 barrier，全部发布被拒绝或批次为空时跳过提交窗口，拒绝后的 cooldown 只更新元数据。成功发布仍在第二次 barrier 内复核 live bytes 后才 discard。缺页查找按宿主地址排序的块做二分查找（不是 guest 地址顺序），并检查块长度，拒绝映射间隙及不足一个块的尾部之外的地址。这里描述实现与正确性契约，不声明实测性能收益。

缺页恢复持有 pager 锁后才进入 store；发布必须先释放 store 锁，再更新
pager 的 cooldown，包括不可压缩或容量不足的拒绝路径。禁止持有 store
锁等待 pager，否则发布拒绝和缺页恢复会互相等待。

快照捕获/恢复、整 VM offload、文件/FUSE backing、`ram_dedup` 与此模式互斥；Linux 外部 `memory_pool` 使用相同 pager，由 daemon 持有有界跨实例 store。完整权限、存储预算与剩余提案见[实例内压缩](../../docs/src/zh/design/memory-optimization/compression-local.md)。

Linux daemon 物理共享模式使用同一采样/复核屏障，但不注册 userfaultfd：
宿主 store 提供只读、大小封印的文件与被引用固定的 4 KiB 槽位；发布后复核
live bytes，再以 MAP_PRIVATE 替换原位置映射。读取保持共享，写入由内核 COW
隔离；pagemap 的 present/file 位用于跳过仍共享的页并发现已写入的私有页，
不读取 PFN。共享模式每批最多暂存 1 MiB；驻留状态按连续 RAM 区间批量
查询，逐页采样/复核仍在屏障内。跨实例重复候选进入物理池，独有候选只保留
有界摘要；写入成为私有页后，发布前释放旧池引用，即使新页被拒绝。
替换后才释放仍被映射的旧引用；worker 退出仍保留映射，连接断开由池通过
pidfd 确认进程退出后释放引用，不能把断连当作可安全复用槽位的证明。
该模式不压缩独有页，也不要求 userfaultfd；前述 UFFD 驱逐/恢复契约只适用于
实例本地冷压缩。快照、offload 与 KSM advice 仍互斥。

共享模式将 balloon free-page reporting 交给 pager：每个设备最多保留一条
未确认的 descriptor chain，仅暂存 guest 地址/长度与完成状态，不保留 device
lease。guest 在 used-ring 确认前不能复用报告页。worker 在 CPU 停驻和设备排空
窗口校验完整 RAM 范围（含对齐、溢出、区域间隙及固件/设备排除），替换为稀疏
私有匿名零页；旧池引用在窗口外释放，然后通知队列 owner 发布 used ring。
不能直接对私有文件页使用 MADV_DONTNEED，否则 backing 字节可能再次出现。
报告页重新分配/写入后继续参与普通共享扫描。暂存报告使 snapshot capture
明确拒绝，不把未完成队列状态当成可恢复设备状态。

逐页状态只保存池对象指针和单调毫秒 cooldown，地址/长度从有序 RAM 区间
推导；512 MiB / 4 KiB 的基础状态数组为 2 MiB（不含少量区间记录）。恢复
checksum 表仅在 UFFD 压缩模式分配，共享模式提交时仍完整比较 live bytes 和
被引用固定的 backing 页。传输对象的 session 身份按连接共享，offset 使用紧凑
可选值；每条共享页 authority 为 64 字节，不含分配器和池侧索引开销。这些是
结构大小与所有权合同，不能直接当作整组物理内存节省的实测结论。

`RamDedupControl::advise_ram_dedup()` 仅显式登记适合的普通私有 RAM（匿名映射及私有文件 COW 候选），跳过 shared、hugetlb 和设备窗口，不替换映射、不更改全局 sysfs，也不自动启用。`RamDedupReport` 逐映射区分 accepted、skipped、unsupported 和 error；`accepted_bytes` 只表示本次建议被接受的区域长度，不是已合并字节或实际节省。macOS 对候选报告 unsupported。调用与 VM transition 串行化，任一登记成功后，本 VM 生命周期内拒绝启动冷 pager 或安装 device prepare；反向也跳过已启动 pager/prepare 的 VM。建议不可用不暂停或破坏健康 VM；共享信任域和侧信道授权由调用方负责。现有私有 COW RAM 的 reclaim 拒绝逻辑保持不变。

`GuestCommand` 配合禁用 implicit init 的自定义 init；普通 Rust supervisor 继续使用 `/.pvisor-guest.json`。参数和环境不会继承宿主值，拒绝不支持的引号、控制字符、保留环境键及超长命令。`NetworkOptions` 显式控制自定义 init 的 DHCP，请求不会开启 TSI。`network` 默认关闭 DHCP。

Rust supervisor 的可选私有 tmpfs 契约见 [pvisor-guest](../pvisor-guest/README.md)。容量属于已有 guest RAM 预算，工作区 stage 与临时 RAM 数据面分别管理；执行器默认策略不改变本 crate 的跨平台 API 定义。

静态 x86_64 musl 的内核提取、无损打包、加载全部在本 crate 内完成；既有 `PVISOR_KRUNFW_PATH` / `PVISOR_KRUNFW_KERNEL_BUNDLE` **构建输入**保持兼容。`VmPlatform::embedded_kernel` 提供不可变共享字节和启动地址用于身份绑定。已配置内核或带内核布局的快照不重新加载固件；默认启动顺序在所有平台一致：构建内置内核优先，其次 `VmConfiguration::set_firmware_path(&mut self, PathBuf) -> io::Result<()>` 显式选择的文件，最后可执行文件旁的打包固件，缺失返回明确 `NotFound`。

`RuntimeSupport::resolve_firmware_path(directory: Option<&Path>) -> io::Result<PathBuf>` 仅解析本地平台固件名：`Some` 严格使用指定目录，不回退；`None` 使用当前可执行文件旁的目录。结果必须为 regular file，并 canonicalize 为绝对路径。setter 拒绝相对路径，在替换旧选择前校验并 canonicalize 文件；调用方负责信任文件并保持其可用直到启动，解析不固定文件内容。动态加载向 `KernelOwner` 传递所选绝对文件路径，不使用 basename、`LD_LIBRARY_PATH` 或 `DYLD_LIBRARY_PATH` 选择固件。静态 musl 不支持动态库加载。

运行时不下载、编译、校验发行版、维护固件缓存或提供版本/准备 API；固件构建、获取和打包属于仓库构建工具与 Python packaging。调用方使用本地 resolver，并通过 setter 把精确文件交给 runner，负责宿主隔离和证据存储。

为保持 Run/证据协议兼容，部分记录标识、trace stage、环境变量和 runner 参数使用 `krun` 命名；VM 控制通过 Rust API 实现。基准证据只描述其实际测量的实现，不能作为其他实现的验证结果。

Clippy 清理以语义和契约为先：保留 `EAX/EBX/ECX/EDX`、`RTC` 等硬件专名以及诊断含义明确的错误名称，必要时使用带理由的局部豁免。优先删除失效豁免、整理配置与资源参数、修复实现问题，不为消除告警改变专有术语或持久化协议。生成的 ABI 定义、跨平台 libc 字段宽度和 FUSE 协议签名需单独判断。

## EXP-001 M0：observe-only vCPU 观测

`api::VcpuObservationControl` 由 `VmmHandle` 实现，默认关闭，所有平台公开
同一接口。实验调用方可在 ready callback 中启用，然后由自己的采样线程
读取；collector 不创建线程、不积累事件队列、不调用 pause/offload/resume。

```rust
use pvisor_vm::api::{VcpuObservationControl, VmmHandle};

fn sample(handle: &VmmHandle) -> Result<(), String> {
    handle.set_vcpu_observation(true)?; // 重复启用不会清空本 session
    let snapshot = handle.vcpu_observation()?;
    println!("{snapshot:?}");
    Ok(())
}
```

快照是 collector 锁下的一致视图，保存每个配置 CPU 的当前状态、局部和全局
sequence、hypervisor 来源、转换/等待进入退出计数与累计等待时间。`sampled_at` / `since` /
`all_waiting_since` 为本 VM 的单调时间偏移，不是 guest 时钟。`sampled_at`
与 CPU 记录在同一 collector 锁内采集/复制，不在解锁后给旧视图补时间戳。
全 CPU 实际等待才能打开 `idle_epoch`；任一退出等待、控制停驻、Unknown
或退出都会关闭窗口。`online` 指注册后端 CPU 未停止，不代表 guest Linux
CPU online；尚未 PSCI boot 的 HVF secondary 保守阻止全等待聚合。
VM 退出/CPU drop 登记 Stopped 并推进 topology generation，不复活停止的 CPU。
当前拓扑固定、不支持热插拔。关闭再启用会新建 session 并清空 session 计数，
旧 epoch 不复用；中途启用不会 kick CPU，既有 wait/park 保持 Unknown，直到
下一个可观察接点。关闭观测时，以同一时间截断逐 CPU wait 和全等待窗口，
将活动 wait 标为 Unknown；`wait_exits` 包含此类观测区间截断，不等同于后端
wake 次数。禁用期间 Stopped 仍登记生命周期/topology，但不累计不可观测的
等待时长。调用方绑定自己的 Attempt、源码及 binary/firmware 身份。

HVF 只在 `should_wait` 通过且真正进入 WFE/timeout channel select 前采集
WaitingForEvent，离开 select 即关闭；已过期或有 pending interrupt 的路径
不生成等待窗口。KVM 执行入口记录 Executing 接点，整个 `KVM_RUN` 内是
Unknown，返回后是 HandlingExit。不以调用未返回、低 CPU、抢占或 HLT
判断 idle；HLT/shutdown 保持原 Stopped/退出语义。控制停驻（包括快照及 RAM
维护）是 ManualPaused，绝不是 guest idle。HostDescheduled 预留但未推断。

内存为 O(配置 CPU 数)，只存一份当前记录；每次采样返回同样有界的副本，
调用方自行限制保存样本的容量。关闭时普通接点只有原子读取，不获取锁或
读取时钟（退出清理仍登记拓扑）；启用时每个接点序列化更新 collector。
这只是实现层面的开销边界，尚无真实任务开销测量。
`rejection` 区分 Disabled / TopologyIncomplete / Unknown / NotAllWaiting；
即使 all_waiting 也恒为 WakeDeadlineUnavailable：M0 **没有完整 deadline、
clock conversion 或 wake latch，不能自动卸载，也不证明 Linux runqueue 空闲**。
真实 HVF 任务、SMP guest 和性能验收仍需对应宿主实验，不以单测替代。

**实测后源码修复（2026-10-07）：**当前 collector 已修复快照时间戳的锁内
采集，以及关闭观测时逐 CPU 等待区间的截断/禁用退出记账。这些修改发生在
冻结的真实 KVM 实验之后；旧实验仅验证其冻结 binary/source 对应的修复前
版本，不能作为当前源码的实测通过记录。本次保留旧 binary、receipt 和
实验数据；后续实测须另建制品/收据，不覆盖原实验。macOS/HVF 尚未实际
编译或运行验证。

## 验证入口

`just test pvisor-vm` 运行设备、快照和接口契约测试；macOS 自动使用仓库既有 Hypervisor entitlement 签署测试程序。真实 HVF/KVM 和本地 socket 测试需要宿主权限。Linux 测试中创建 VM 的用例需要可用 `/dev/kvm`。

`repository_boundary` 检查所有工作区 manifest 与外部 Rust 调用者，保证 VM 核心组件只在本 crate 内编译。`api_contract` 保证 API 无条件编译、无方法体，并禁止私有适配器另设公开固有方法。

真实 Linux guest 的独立 rootfs/RAM 保存与恢复验证使用 `python3 scripts/check-environment-snapshot.py --report target/vm-validation/environment-linux.json`（Apple Silicon HVF）。guest 探针在 `src/probes/guest_linux.rs`，readiness 使用原子 rename 发布，避免把探针写文件的中间状态误判为恢复失败。底层 CPU/RAM 与 VMM-thread CPU/RAM/GIC 检查分别使用 `check-hvf-cold-restore.py` 和 `check-vm-snapshot-state.py`，它们的报告只描述各自覆盖的范围。

## Stage snapshot rebinding

`api::SnapshotState::rebind_filesystem_stage` relocates independently verified
upper/work/preimage copies while retaining explicitly leased immutable lowers.
The host coordinator imports and pins bases, validates stage inventories, and
holds leases until VM exit. The runtime validates overlay topology and saved
inodes/handles; writable stage roots cannot be retained as immutable lowers.
Legacy full-tree/layer rebinding remains separate. The trait is available with
the same signature on every platform; unsupported backends return an error.
Restore can add the new Attempt's private authority paths through
`rebind_filesystem_exclusions`. It preserves every existing exclusion, rejects
absolute/escaping paths and additions that hide saved inode paths, and leaves
other topology checks intact. This keeps new control sockets and storage hidden
without treating their changed names as a change to the captured workload.


## virtio-fs 并发与冻结契约

每个文件系统设备仍提供一个普通 request queue 和一个 hiprio queue。
普通队列对可重叠的大 READ（请求至少 64 KiB）和目录读取，自动启动有界
blocking I/O 线程池；短元数据请求和需要串行的修改由队列 owner 内联处理。
完成结果异步返回队列 owner，由 owner 独占 available/used ring 的更新。
默认 worker 数为宿主可用 CPU 数，上限 4；最多接收 `2 × workers` 个在途请求。
没有可重叠请求时走内联路径，hiprio 的 FORGET/INTERRUPT 也不占普通线程池容量。
这里的异步指请求完成与队列分发解耦；文件 I/O 仍使用既有 pread/pwrite。

线程间传递已校验的 descriptor 地址与长度，在执行线程内创建借用的
Reader/Writer，保留 RAM access lease 直到 used ring 发布完成。禁止通过
延长 VolatileSlice 的生命周期或新增 unsafe Send 绕过这个契约。
FUSE header 只解码一次；INIT/DESTROY 独占 session guard。

OverlayFs 的 READ、LOOKUP、GETATTR、目录查询和只读 OPEN 可共享 operation guard。
带 O_APPEND/O_TRUNC、非只读或需要 kill_priv 的 OPEN，以及改名、copy-up、
写入、release 与快照恢复仍使用独占 guard。只读 OPEN 的首次观察继续由
Core 的逐路径 journal 事务同步，快照必须等待所有 operation guard 释放；
读取通过 backing I/O 持有共享 guard，阻止原生句柄提前释放。文件句柄和
不可变目录项只在查表时持有 handle map 锁，不把整张表锁带入 I/O。
目录缓存用 `Arc` 持有原生 lookup 引用，借用者在缓存锁外执行 backing I/O；
淘汰、失效和 clear 仅移除缓存所有权，最后一个借用者离开后才 forget。
原生 inode 表在同一个 map 锁窗口内固定引用或仲裁插入，防止并发 lookup
替换已返回的 inode，以及 final forget 与引用固定之间的竞争。

freeze/reset 停止接收新请求，排空已接收请求、回填所有完成结果并 join
全部 I/O worker 后才能返回。thaw/restore 主动扫描 available ring，不依赖
guest 再发 kick。正在执行的请求不序列化进入快照；超时由现有 runner
失败契约处理。公开 `api` 的结构和方法不随平台或 worker 数改变。

宿主诊断变量 `PVISOR_VM_FS_WORKERS=1..8` 可设置每个设备的 worker 上限；
`1` 禁用线程池分发。生产调用方通常不需要设置。`PVISOR_FS_PROFILE=1`
会额外记录 `virtio-fs-dispatch` 的 inline/pool 请求数及创建的 worker 数；
带 profiling 的运行只用于诊断，不作为性能验收。

设计参考 [virtiofsd 的线程池和 EVENT_IDX 处理](https://gitlab.com/virtio-fs/virtiofsd/-/blob/main/src/vhost_user.rs)
及 [passthrough 的资源持有方式](https://gitlab.com/virtio-fs/virtiofsd/-/blob/main/src/passthrough/mod.rs)。
并发调度使用 pVisor 内部实现，不复制上游代码，也不提供 vhost-user、DAX、
writeback 或上游的工作目录切换；这些机制需要各自的权限、缓存和冻结契约。
