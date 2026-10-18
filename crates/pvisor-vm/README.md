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

pager 仅接受 4 KiB 宿主页上的普通私有匿名可写 RAM；严格匹配身份与拓扑后排除 builder 授权的不可变 raw 固件，拒绝未知 raw、文件/COW、shared 和 hugetlb RAM，排除设备窗口。`tee`、`aws-nitro`、`gpu`、`snd`、`input` 构建及已有 device prepare/dedup advice 被拒绝。实例本地及 daemon sealed 压缩使用 64 KiB 块，Linux daemon 物理池使用独立的 4 KiB 页；压缩模式每批最多暂存 4 MiB；两次 CPU 停驻/设备 lease 排空窗口分别捕获与复核，编码发布期间 guest 继续运行。持有校验对象后才 discard；refault 校验长度、checksum 与完整 `UFFD_COPY` 后唤醒访问。UFFD 压缩的 balloon 空闲页报告仍确认但不 discard；物理共享模式的协同回收见下文。此策略是驱逐/refault 探测，不是真正的读访问热度检测器，也不是普通 pause。

Linux pager 在进入采样窗口前，仅持有 pager 锁检查 cold 状态与 cooldown；没有候选时不进入 CPU/设备 barrier，但仍检查 VM 退出，并在锁与屏障外调用 store stats 处理延后的引用释放。首次 barrier 仍必须停驻 CPU 并排空设备 lease，才能安全复制 live RAM；存储编码/发布不持有 barrier，全部发布被拒绝或批次为空时跳过提交窗口，拒绝后的 cooldown 只更新元数据。成功发布仍在第二次 barrier 内复核 live bytes 后才 discard。缺页查找按宿主地址排序的块做二分查找（不是 guest 地址顺序），并检查块长度，拒绝映射间隙及不足一个块的尾部之外的地址。这里描述实现与正确性契约，不声明实测性能收益。

缺页恢复持有 pager 锁后才进入 store；发布必须先释放 store 锁，再更新
pager 的 cooldown，包括不可压缩或容量不足的拒绝路径。禁止持有 store
锁等待 pager，否则发布拒绝和缺页恢复会互相等待。

快照捕获/恢复、整 VM offload、文件/FUSE backing、`ram_dedup` 与此模式互斥；Linux 外部 `memory_pool` 使用相同 pager，由 daemon 持有有界跨实例 store。完整权限、存储预算与剩余提案见[实例内压缩](../../docs/src/zh/design/memory-optimization/compression-local.md)。

Linux daemon 物理共享模式使用同一采样/复核屏障，但不注册 userfaultfd：
宿主 store 提供只读、大小封印的文件与被引用固定的 4 KiB 槽位；发布后复核
live bytes，再以 MAP_PRIVATE 替换原位置映射。读取保持共享，写入由内核 COW
隔离；pagemap 的 present/file 位用于跳过仍共享的页并发现已写入的私有页，
不读取 PFN。共享模式每批最多暂存 1 MiB；驻留状态只查询选中的最多 2 MiB
连续区域，逐页采样/复核仍在屏障内。跨实例重复候选进入物理池，独有候选只保留
有界摘要；写入成为私有页后，发布前释放旧池引用，即使新页被拒绝。
已共享页的 present/file 状态按同一最多 2 MiB 区域批量读取 pagemap，
每次最多暂存 512 项，不为每个已共享页单独 pread；仅解析状态位。
候选调度直接使用同一次查询的 present 位，不再另外对整个区域执行
mincore；没有 present PTE 的稀疏 RAM 或已缓存文件页都保持惰性，
不为发现候选而触发缺页。屏障内复制前的驻留及 file 状态复核保持不变。
发布期间也只有 present 且为匿名页的 PTE 才能证明 COW 并释放旧池引用；
缺失的文件 PTE 仍可能从原槽位缺页读回，因此保持 authority 到映射替换完成。
该批量结果只用于候选调度，进入采样屏障后仍逐页核对实际状态和内容。
替换后才释放仍被映射的旧引用；worker 退出仍保留映射，连接断开由池通过
pidfd 确认进程退出后释放引用，不能把断连当作可安全复用槽位的证明。
该模式不压缩独有页，也不要求 userfaultfd；前述 UFFD 驱逐/恢复契约适用于
实例本地及 daemon sealed 冷压缩。快照、offload 与 KSM advice 仍互斥。

物理池默认接收 virtio-fs 成功 READ 的调度提示：数据 Writer 与回复头分离，
只在成功后登记实际写入字节前缀中的完整页；相邻 descriptor 片段可合并，
EOF、错误、取消或越界完成数不登记。提示只包含宿主地址范围，不持有数据或
共享权限；原 device lease 保持到完成/取消，pager 自身固定并验证 RAM 映射。
每 VM 队列最多 4096 个范围（64 KiB 范围存储），满时丢弃最旧提示，
不会影响普通读写和后台扫描。关闭、KSM 和压缩模式没有此队列或捕获分配。

候选最多一半来自文件提示；与后台候选交替，后台先获得采样机会，
启动/改写冷却中的提示延后，稀疏页和映射外范围跳过。原完整内容哈希、
两次 CPU/设备屏障、live-byte 复核及槽位引用合同继续决定是否共享。
因此可变 rootfs 的普通库读取也能驱动候选发现，不需要把文件声明为不可变，
也不绕过 Core 的读取授权或把文件身份当作内容相等的证明。
宿主私有实验变量 `PVISOR_MEMORY_FILE_HINTS=0` 关闭提示以便同制品 A/B；
未设置或 `1` 开启，其他值在预留 pager 之前拒绝。该变量只在物理池模式读取。
诊断 `file_hint_candidates_total` 统计被选为优先候选的次数，包含重访，
不代表实际成功合并、文件命中数或内存节省。

物理页扫描借鉴 [UKSM 的区域分层设计](https://www.usenix.org/system/files/conference/fast18/fast18-xia.pdf)，
由私有 `cold_ram_scan.rs` 调度，每个 RAM mapping 分成最多 2 MiB 的区域；
区域不会跨越映射边界。四级配额为每轮 16/64/256/512 页，初始最低级；
至少采样 8 页、成功合并比例达到 10% 且变化数低于合并数一半时升一级。
观察到 COW、发布复核失败或共享页提前释放，会增加变化计数；变化数达到
合并数一半时回最低级，其余低收益轮次降一级。已全部共享的区域也会降频。
每个区域保留最低平均探测配额，带随机起点和互质步长的遍历在重复前覆盖
区域所有页；这不保留逐页随机排列数组。按区域轮转，每次最多取 64 页，
单批最多考察 4096 个索引，选择阶段保留 8 ms 软时间预算，完整轮次之间
至少休息 250 ms。低优先级区域的实际发现延迟仍取决于 RAM 大小、冷却期
和 VM/IPC 调度；这些参数是当前策略常量，不是实测 CPU 百分比保证。
512 MiB RAM 额外区域元数据不超过 32 KiB，不改变既有逐页状态大小。

完整 512 页区域走完首次遍历后，最低级将每轮 16 页合为每四轮 64 页；
其余三轮不查询该区域的 pagemap。仍在任意连续 32 轮覆盖该区域所有页，
首次探索、高收益区域和不足 2 MiB 的尾部保留原节奏。这样减少整区域
元数据查询而不延长完整遍历的轮次上界；不是按无限指数退避跳过低收益区域。
来自文件提示的成功反馈可直接提升区域并取消等待。上述轮次仍受实际
工作耗时和 250 ms 休息影响，不等于固定墙钟 SLA。

这份 Rust 策略独立实现区域调度，没有移植 UKSM 内核补丁或 APH 部分页哈希。
完整内容哈希、字节比较、两阶段发布及 COW 所有权契约继续决定能否合并。
`PVISOR_EXPERIMENTAL_MEMORY_METRICS=1` 增加 `pvisor-shared-scan` 诊断，记录
区域级别、探测、成功合并、COW 和变化计数；计数不等于净物理节省。
确定性测试覆盖完整遍历、公平性、升降级、负载变化与元数据上限；已有真实
映射测试也断言 COW/复核失败的反馈。双 VM correctness driver 的 `--physical`
模式见 [daemon README](../pvisor-daemon/README.md#validation)。

daemon sealed store 的编码、IPC 和描述符校验在 pVisor 中；本 crate 仅持有
`ColdRamStore` 引用。对象交付后 restore/release 不执行 IPC，池丢失停止新发布但
保留本地恢复能力。无候选时的 store stats 也会处理延后释放。store 互斥锁仍可能
让恢复排在在途发布之后；这不构成恢复优先调度或尾延迟验收。

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

硬件专名（如 `EAX/EBX/ECX/EDX`、`RTC`）、诊断含义明确的错误名称和持久化协议不因 lint 告警而改变；必要的局部豁免须说明理由。生成的 ABI 定义、跨平台 libc 字段宽度和 FUSE 协议签名须保持兼容。

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

**验证状态：**现有真实 KVM 实验仅覆盖其冻结的 binary/source，不覆盖
当前 collector 的锁内时间戳采集、关闭时等待区间截断及禁用期间退出记账。
当前源码尚无对应的真实 KVM 实测通过记录；macOS/HVF 尚未实际编译或运行
验证。后续实测须使用独立制品和收据，不覆盖已有 binary、receipt 或实验数据。

## Physical lower stability contract

`api::OverlayConfig::lower_mutability` uses the shared
`pvisor_overlay_core::LayerMutability`, ordered like `lower_dirs`. Empty means all
mutable (including older serialized private adapter configurations). A nonempty
length mismatch is `InvalidInput` before builder configuration replacement and
is also checked during private adapter construction/restore. API shape remains
unconditional and no API method bodies are added. The private adapter passes the
promises into `OverlayLayout::with_lower_mutability`; caches are newly constructed,
not serialized into machine snapshots. Rebinding copies downgrades declarations
to mutable; only explicitly retained, unchanged lower owners keep old promises.

Immutable is a caller-owned lifetime guarantee of physical contents, metadata,
namespace, parent and mount identity, including hardlink aliases, not proof from
a read-only mount or `frozen_baseline`. Do not declare a merged upper immutable.
The Core cache is bounded and never bypasses merged precedence, upper markers,
policy, hardlink checks or baseline read observations. Guest TTLs are unchanged;
root-device data-page retention is described below. Set
`PVISOR_DISABLE_IMMUTABLE_LOWER_CACHE=1` in the host runner
before device construction for the same-contract uncached A/B control; unset
for caching. `PVISOR_FS_PROFILE=1` reports Core hit/miss/eviction counters; the
shared Core README specifies bounds and exact measurement names. No real guest
performance claim is made by these contract tests.

## Rootfs data-cache default

The root Overlay device (`/dev/root`) defaults to the selected `root-data`
profile: retain guest data pages across read-only regular-file OPENs only when
the selected physical lower has an explicit `LayerMutability::Immutable`
declaration. The default does not promote host `/`, arbitrary prepared rootfs,
OCI extraction, lazy physical projections or frozen baselines to immutable.
`rootfs_immutable`'s apply restriction alone is not this lifetime guarantee.

Host OPEN still resolves merged precedence, checks policy/aliases and permissions,
records the baseline observation and acquires its normal native descriptor.
Without writeback, writable or privilege-changing OPENs, copy-up and reads from
upper/mutable layers retain the original cache-invalidating OPEN behavior. Workspace devices
retain their original data policy even if a lower is explicitly immutable.
The root-data setting itself leaves metadata TTLs and FUSE no-open negotiation
unchanged; directory images and deferred opens follow the separate contract below.
New VM writeback and
DAX remain default-off; existing explicit choices and saved writeback sessions
keep their documented behavior.

Before constructing the serving host runner, set
`PVISOR_VM_CACHE_EXPERIMENT=off` for the uncached control or `root-data` for this
profile; unset also selects `root-data`. The old frozen experiment's metadata,
combined and no-open values are rejected rather than silently enabled. This
variable controls admission to the optimization, not physical immutability.
No per-request environment lookup is added.

## Read-only local FLUSH experiment

`PVISOR_VM_FS_READONLY_FLUSH_ELISION=1` opts into a Linux host/guest experiment;
unset or `0` keeps the existing path. Other values fail before filesystem owner
construction. Host and patched guest negotiate `FsOptions::PVISOR_LOCAL_FLUSH`
(INIT bit 43); qualified OPENs return `OpenOptions::PVISOR_LOCAL_FLUSH` (bit 29).
Old guests do not negotiate it and keep ordinary FLUSH, including snapshots.
It requires guest cache, LinuxComplete permissions, no DAX window, an explicitly
Immutable physical lower and no negotiated writeback or remote POSIX/flock locks.
Mixed views qualify each selected lower separately; upper and Mutable-lower
opens never receive this per-open flag.

Normal Core resolution, authorization, baseline observations and native OPEN
still run. Only a read-only, non-privilege-changing regular-file lower open can
qualify. The actual pinned native descriptor must be O_RDONLY and regular; its
fstatfs must report EXT (EXT2/3/4 share `0xef53`), XFS, Btrfs or tmpfs. Unknown
and remote native filesystems retain FLUSH. This tests the actual native
filesystem, not the origin of its content: an attached remote-content backend's
local projection can qualify when its native descriptor meets these conditions;
that backend currently has no FLUSH method and the original callback only
dup/closes the native projection descriptor. The quick experiment uses actual
local sealed inputs.

The new flag does **not** use standard FOPEN_NOFLUSH, whose early return skips
guest checks. The guest still runs write_inode_now, fuse_sync_writes and
filemap_check_errors, then skips only the backend FLUSH RPC. Ordinary RELEASE,
descriptor leases, local guest lock lifetime, writable handles, data caching and
metadata validity are unchanged. Eliminating the backend dup/close naturally
eliminates its internal resource errors such as dup's EMFILE; this is not a claim
that every historical close errno remains identical. It preserves the guest's
data/locking error boundary and does not fix lower/upper shared-page-cache issues.

Sessions actually negotiating the new capability reject capture with Unsupported;
restore rejects records containing bit 43. No new snapshot fields are introduced,
and requesting the feature with an old, non-negotiating guest preserves snapshots.
DESTROY clears active session state so a subsequent INIT may fall back normally.
Profiles emit `readonly_flush_elision_requested` units 0/1 and
`readonly_flush_elision_enabled` units 0/1 (whether this owner has ever negotiated
the contract, counted once across DESTROY/INIT). Fresh single-session diagnostics
therefore report actual negotiation. `readonly_flush_elision_open_flags` counts
qualified OPEN replies; it is not a count of skipped guest RPCs.

Focused private tests cover parsing, negotiation/fallback, native descriptor and
filesystem qualification, normal OPEN/RELEASE, policy denial, snapshots and
DESTROY/INIT. The independent guest `review-flush` probe has eleven cases:
root/stable/Mutable/upper bound and unbound descriptors, each with dup and explicit
close checks (eight); lower close with an upper writer, local flock duplication
and lifetime, and mmap readability after close (three). Both probe mounts are
unmounted at the end. Existing `review-pages` cases remain separate evidence;
FLUSH elision makes no claim to repair their data-mapping behavior.

The preference adds no serialized cache object or handle field. Restore keeps
the existing backing identity/content checks and guest RAM pages. Future OPENs
use the newly constructed device's preference and saved lower declarations;
legacy mutable declarations and copies downgraded during rebinding cannot
qualify. Unit tests cover cache flags, unchanged TTLs, denied OPENs, writable
copy-up/old-FD ownership, live mutable reads, mixed lower order and snapshot
round trips. Frozen engineering A/B results apply to their owned inputs; the
new stricter default has no independent end-to-end performance measurement.

## Standard negative-entry experiment

`PVISOR_VM_FS_NEGATIVE_ENTRY_CACHE=1` enables a separate default-off host
experiment; unset/`0` preserves ordinary lookup, and other values fail before
filesystem construction. Setting `1` explicitly promises that upper namespace,
roots, physical ancestors, mounts and relevant directory permissions remain
owned for this filesystem owner's entire lifetime. All changes must use this
VM's filesystem connection and its normal mutation/VFS paths; external writers
and new overlapping metadata-backend attachments are unsupported.

Eligibility additionally requires LinuxComplete permissions, guest cache,
WB=0, DAX=0, every lower explicitly Immutable and default authorization rules.
Session/view audit bindings do not change default rules. Actual
upper/lower directory FDs must be on EXT/XFS/Btrfs/tmpfs; failed/unknown native
filesystem evidence retains the old path. Any active ancestor/descendant backend
attachment disables eligibility, using a read-only registry query rather than
querying attributes of a possibly missing name. The registry is checked again
before a promise; a late attachment prevents further promises but cannot cancel
already valid guest dentries and violates the ownership contract.

Each selected ordinary miss uses Core's checked optional backing lookup. Only
successful merged absence after required observation publication returns a
standard successful FUSE Entry with nodeid=0, entry TTL=1 second and attribute
TTL=0. It allocates no inode or lookup/forget reference. Errors, including
ENOENT from exclusions, backend or journal failures, are not converted to
cached absence. Positive replies keep their old timeouts, metadata checks and
inode ownership. Core-validated exclusion prefixes and worker-provided synthetic
names protect their entire subtrees **and their ancestors**: those paths always
retain ordinary lookup, including missing ancestors and names consumed by
one-shot injection. Unrelated paths in the same owner can qualify. In particular,
ordinary VM rootfs host-authority exclusions remain hidden by Core's original
ENOENT; they do not disable caching for unrelated rootfs misses. Prefix comparison
uses path components, so an excluded `tmp/private` does not protect `tmp/private2`.
Existing COMPLETE directory-image negatives remain separate guest-side behavior.

No new kernel protocol or capability bit is added. Profiles emit explicit
`negative_entry_cache_requested`/`negative_entry_cache_enabled` units 0/1 for
each Overlay owner. Enabled describes configured ownership/qualification;
`negative_entry_cache_replies` counts returned cacheable negative Entries,
including zero, and `negative_entry_cache_fallbacks` counts enabled-owner
lookups declined for protected names or new/unknown backend evidence. These
are response decisions, not measured guest hits or successful transport writes.

Once a valid negative Entry has been returned, capture and restore on that
owner return Unsupported. The monotonic fence is checked under the exclusive
operation lock and survives DESTROY/INIT. Configuration, protected names and
the fence are not snapshot fields; a new restored owner starts without them
and is configured again by the worker. A snapshot from this experiment cannot
contain its negative replies because capture after the first reply is rejected.
Before any such reply, the prior capture/restore path remains available.
An enabled negative-cache owner also rejects a saved WB=1 native session under
that lock before restoring any layer or namespace state, including when an unset
writeback environment otherwise permits historical WB=1 restoration. Disabling
this preference retains that existing compatibility; WB=0 restores remain valid.
Private tests cover qualification, unchanged positive TTLs and errors, real
exclusions and their existing/missing ancestors, protected subtrees and unrelated
sibling misses, no negative inode allocation, create/link/rename/unlink behavior,
backend overlap/late fallback, journal overlap and snapshot lifetime; positive
eligibility fixtures use Linux `/dev/shm` tmpfs. Real guest cache invalidation
and independent performance runs remain required before any benefit/default claim.

## Independent immutable-lower data objects

`PVISOR_VM_FS_LOWER_DATA_CACHE=1` enables a default-off Linux experiment for
explicitly Immutable selected physical lowers, on root and workspace devices.
Unset/`0` uses the previous path; other values fail before filesystem owner
construction. LinuxComplete permissions, guest cache, WB=0, no DAX window and
no negotiated POSIX/flock remote locking are required. Qualification is per
opened lower; upper, Mutable, direct-I/O, privilege-changing, nonregular and
multiply-linked native files fall back. Attached remote-content projections
also fall back per selected physical path: their placeholder inode is not a
content identity. Synthetic guest configuration/init files do not disable
unrelated ordinary lower files.

Host and guest negotiate `PVISOR_DATA_OBJECT` (INIT bit 44). Only `FUSE_OPEN`
then returns a fixed 24-byte payload: the standard 16-byte `OpenOut` prefix
(`fh:u64`, `open_flags:u32`, `backing_id:u32`) followed by little-endian fixed
physical size `u64`. Qualified OPENs set bit 28 and a nonzero object ID; fallback
OPENs return ID/size zero and clear bit 28. Unnegotiated OPEN, CREATE and OPENDIR
retain their standard reply lengths; CREATE/OPENDIR always clear the new flag.
This does not negotiate standard FUSE passthrough or export host descriptors.

Every OPEN still performs ordinary Core resolution, authorization, baseline
observations and native OPEN. The optional receipt uses the captured handle,
not a new namespace lookup: its pinned native FD must be O_RDONLY, regular,
nlink=1, without O_DIRECT, on the supported EXT/XFS/Btrfs/tmpfs native filesystem
set. The default classifier checks fstat metadata against complete statx
identity/mount/size evidence; the optional same-OPEN classifier below reuses
complete FD statx instead of issuing duplicate metadata queries. A table assigns
stable IDs by the fixed layer generation and actual
FD device/inode/mount/size. The caller must preserve the Immutable physical
owner, including roots, ancestors and mount identity, for the device lifetime.
No FD is retained by this table and IDs grant no access by themselves.

At most 4096 identities are retained per OverlayFs owner. IDs are never evicted
or recycled, including across DESTROY/INIT; new identities at capacity fall back,
while registered identities can still be reused. Final owner teardown frees
the table. Each new guest open still owns its normal namespace nodeid/FH and
RELEASE, while the patched guest uses the ID for a distinct immutable-data
mapping with the supplied fixed EOF. Copy-up and upper truncation therefore do
not change the old lower mapping's size or place upper bytes in its pages.
The namespace inode's current size is never used as an old lower size receipt.
The existing root-only KEEP_CACHE preference is separate; this experiment does
not turn on ordinary workspace KEEP_CACHE for shared merged-inode data pages.

Negotiating this experimental contract makes capture/restore unsupported for
the remaining host owner lifetime, including after DESTROY; server restore
rejects saved bit 44. The ID table is not serialized. Requesting it with an old,
non-negotiating guest leaves snapshots unchanged. Profiles emit
`lower_data_cache_requested` and `lower_data_cache_enabled` as explicit units
0/1; enabled means ever negotiated once per owner (actual negotiation for fresh
single-session diagnostics). `lower_data_cache_open_flags` counts qualified
OPEN replies, `lower_data_cache_object_registrations` counts assigned IDs,
`lower_data_cache_object_reuses` counts repeated receipts for registered IDs,
and `lower_data_cache_object_rejections`/`lower_data_cache_capacity_rejections`
count fallback receipt attempts/its capacity subset. All are diagnostic work
counts, not bytes saved, elapsed time or a performance result.

Focused private tests cover strict parsing, old-peer and incompatible-session
fallback, exact OPEN/CREATE/OPENDIR framing, zero/invalid receipts, fixed lower
EOF across copy-up, normal OPEN denial, nlink and native FD qualification,
release without permanent FD pins, bounded IDs, and snapshot/lifecycle rules.
Real guest byte/page/mmap/exec probes and independent profile-off timing remain
required before interpreting this candidate as a speedup.

## Same-OPEN qualification experiment

`PVISOR_VM_FS_SHARED_OPEN_QUALIFICATION=1` selects a default-off private Linux
host experiment. Unset/`0` retains separate DATA and readonly-FLUSH classification;
other values, including non-UTF-8, fail before Overlay filesystem creation.
macOS validates the setting but retains its existing path. There is no new
guest capability, public API, wire field, handle field or snapshot field.

Server still owns negotiated/request eligibility, including readonly flags,
O_DIRECT/O_PATH, killpriv and write-open filtering. The private `OpenReply`
checks actual returned options/handle for both adapter paths, excluding DIRECT_IO
and passthrough. `AugmentFs` forwards ordinary OPEN responses explicitly and
retains synthetic-file handling. Only the server OPEN response path can share
native evidence; direct private `open`/`lower_data_object` calls retain their
fresh separate checks.

After ordinary Core authorization/alias checks, baseline observation, native
lookup and successful native OPEN, the shared qualifier pins that new native
descriptor and samples F_GETFL, complete FD statx and fstatfs once. DATA still
requires actual readonly regular nlink=1, TYPE/NLINK/INO/SIZE/MNT_ID result bits,
nonzero mount ID, representable fixed EOF, no O_DIRECT/O_PATH, selected Immutable
lower ownership and the native local-filesystem whitelist. Unsupported or
incomplete evidence declines optional DATA without failing OPEN. FLUSH retains
its broader rules: multiple hardlinks and failed/incomplete statx do not remove
its qualification; missing TYPE evidence falls back to its original fstat check.
FLUSH-only sessions retain their original three-query classifier.

The native decision moves once through a stack-owned OPEN result. Receipt
assembly still acquires its FileIoLease, validates the handle/inode/lower and
rechecks the selected-path backend before assigning/reusing an object ID.
No evidence is cached on handles or reused by a later OPEN; status flags and
native identity are sampled again for each new FD. Snapshot capture/restore,
old-peer fallback, RELEASE and the DATA/FLUSH lifetime contracts remain unchanged.

Linux Overlay profiles explicitly initialize every measurement, including zero:
`shared_open_qualification_enabled` is the effective host preference (not guest
negotiation), and `open_qualification_fgetfl_attempts`,
`open_qualification_fstat_attempts`, `open_qualification_statx_attempts`, and
`open_qualification_fstatfs_attempts` count only actual qualification query
attempts, including failures. They exclude native lookup/getattr and other
operations' metadata syscalls. `_shared_attempts`, `_legacy_flush_attempts` and
`_legacy_data_attempts` with the same `open_qualification` prefix count classifier
entries. `_receipt_hits` counts request-local decisions consumed after the second
backend check, including negative decisions; `_receipt_misses` counts fresh
legacy DATA classification there. Existing flag, registration, reuse and
rejection counters retain their meanings. Missing measurements/owner records
are incomplete evidence, never inferred zeros; these native counters are not a
macOS diagnostic contract.

On the successful combined source path, seven qualification query sites become
F_GETFL+statx+fstatfs, while DATA-only goes from four to three. These are static
call counts, not measured syscall totals or elapsed speedup. Focused private
tests compare real pinned descriptors and profile attempts, independent DATA/
FLUSH fallback, incomplete statx masks/unsupported statx, nlink/status flags,
copy-up/old lower EOF, second backend checking, fresh later receipt calls,
release, default parsing and snapshot schema. Independent real guest and
profile-off timing remain required before a performance claim.

## 独占 upper 物理不存在缓存实验

宿主 runner 可显式设置 `PVISOR_VM_FS_EXCLUSIVE_UPPER_ABSENCE_CACHE=1`；
`0` 或未设置默认关闭。worker 在创建任何文件系统前严格校验 `0|1`，
非法值（含非 UTF-8）拒绝启动。仅全部 lower 显式 `Immutable`、
`LinuxComplete` 且 guest cache 开启的 Overlay 才有资格；普通 passthrough、
Mutable/未声明 lower 和关闭 guest cache 的设备不启用。

启用者除目录镜像的 upper 命名空间独占契约外，还承诺 upper 根目录、物理祖先
及 mount 身份在整个设备生命周期内保持稳定；宿主不得绕过 VM 改动 upper
命名空间或目录权限等会改变路径检查结果的元数据。该显式承诺不能从只读挂载、
frozen baseline 或某个 lower 的稳定性推导。Core 校验 preimage journal 与
upper 的 canonical 物理路径，重叠（相同、祖先或后代）时拒绝启用，避免读观察
在读操作期间修改同一个 upper 树。默认配置不作这个额外承诺。

缓存只保留当前稳定 upper 命名空间 epoch 内的物理 `MissingLeaf` 和
`UnavailableParent`，不缓存错误、upper 属性、合并赢家、whiteout/opaque
判断、权限或日志决定。每个修改操作先持有原 VM 独占 operation lock，再建立
Core mutation guard；整个修改期间禁用命中与填充。guard 在锁释放前清空并
恢复缓存，成功、失败和展开退出均走同一 Drop 顺序。capture、restore、INIT
和 DESTROY 使用同样的边界；原生 descriptor I/O admission、copy-up、对象绑定
及其他串行化规则保持不变。

配置和缓存不进入 filesystem snapshot schema；恢复 worker 按当前宿主配置
重新设置。`PVISOR_FS_PROFILE=1` 的 overlay 记录
`exclusive_upper_absence_cache_requested`（请求值 `0|1`），Core 的
`exclusive_upper_absence_cache_enabled` 才是实际生效值；暂时 mutation 禁用
不会改变配置值。hit/miss、MissingLeaf/UnavailableParent 分类和失效计数用于
诊断宿主物理查询变化，不代表少了 FUSE 请求或已经测得时间收益。
guest 属性与 entry TTL 保持原值；本实验不修改内核、KEEP_CACHE、WB 或 DAX，
也不把未测候选作为默认优化。

## 不可变只读文件的 DAX

`VmConfiguration::immutable_filesystem(tag, path, window_size)` 为独立的、
物理内容不可变的目录建立只读 virtio-fs share。当前支持 Linux x86_64/KVM，
不支持 TEE/Nitro；其他平台通过相同 API 返回 `Unsupported`。调用方须在
VM 生命周期内持有目录的不可变所有权，包括 metadata、目录结构、祖先、
挂载身份与 hardlink 别名；guest 的 `ro` 挂载本身不构成这个保证。

DAX 默认关闭。普通 `filesystem(..., 0)` 与 `overlay(..., 0)` 不创建 DAX
窗口；CLI 的 staged rootfs 和 workspace 使用此默认路径。保留
`immutable_filesystem` 作为显式启用入口，只有调用它才为独立不可变 share
配置 DAX 窗口。

显式不可变 share 的目录项及 LinuxComplete 属性 TTL 为 60 秒，guest 使用
`dax=never` 时同样生效。普通只读 share 和可写 Overlay 保留原 TTL；
LinuxSimplified 的属性仍不缓存，以保持请求 uid/gid 的语义。
数据缓存策略保持原配置。

```rust
use pvisor_vm::api::{VmBuilder, VmConfiguration};
use std::path::Path;

fn attach_tools(vm: &mut VmBuilder, leased_directory: &Path) -> std::io::Result<()> {
    vm.immutable_filesystem("tools", leased_directory, 128 << 20)
}
```

guest 内核需要 virtio-fs DAX 支持，挂载使用
`mount -t virtiofs -o ro,dax=inode tools /opt/tools`。同一 share 可使用
`ro,dax=never` 关闭 guest DAX，走普通 READ；此时仍保留已配置的窗口及
该 attachment 的生命周期、快照限制。只对宿主 regular backing
file 返回 inode DAX hint；虚拟文件继续走普通 READ，不能对含虚拟文件的
share 强制 `dax=always`。OPEN 和 metadata 仍经过 virtqueue；建立映射后，
文件的 read/mmap 数据经共享窗口访问。窗口由 guest 按需回收和重映射，
不把整个目录预读到 RAM。

映射复用 OPEN 的原生 FD。Linux 的 SETUPMAPPING 使用 `fh=-1`，因此从
该 inode 的活跃只读 handle 取得 Arc lease；没有活跃 handle 时拒绝新映射，
不重新按路径打开文件。映射自身固定 backing，关闭 FD 不立即撤销已有映射。
setup/remove 检查地址溢出、页对齐、窗口边界与重叠；批量 remove 先验证全部
范围，移除后以匿名零页替换，不留下不可访问地址。尾部整页超出 EOF 时也
使用零页，避免恶意物理地址读取引发宿主 SIGBUS。

KVM 将整个窗口登记为只读 memory slot，使正常读取保持宿主文件页共享，
guest 物理写入产生 MMIO exit。宿主映射保留 MAP_PRIVATE 的 COW 保护，
设备 descriptor 若引用窗口也不能改写宿主 backing。guest 的 MAP_PRIVATE
写入只改变自己的匿名页。窗口要求 2 MiB～1 GiB、以 2 MiB 为单位；它占用
额外 guest 地址空间，resident 文件页和 COW 页仍计入宿主内存预算。

可写 merged OverlayFS、普通 writable passthrough 的非零 `shm_size` 明确
返回 `Unsupported`。DAX 与 snapshot profile、restore、freeze/capture 互斥，
不通过省略映射状态生成不可恢复快照。该阶段只提供显式不可变 share 的
数据路径，不声明现有 CLI 工作负载或启动时间已经加快。

## Guest 目录镜像

在宿主 runner 启动前设置 `PVISOR_VM_GUEST_CACHE=0`，可整体关闭 guest
目录元数据镜像和惰性只读 open；`1` 或未设置默认开启。该开关对所有
virtio-fs attachment 生效，在 worker 构造时读取，非法值会导致启动失败。
关闭时宿主不协商 `FUSE_PVISOR_META`，同一份已打补丁的固件回到普通
FUSE 查询和打开路径。1 MiB 请求窗口与预读、宿主 root-data 策略、TTL
及 writeback/DAX 配置保持一致，用于隔离目录镜像和惰性 open 的整体收益。
已有快照恢复保留保存的协商状态；A/B 应分别启动新 VM。

```bash
PVISOR_VM_GUEST_CACHE=0 pvisor run --executor vm --safe -- YOUR_COMMAND
PVISOR_VM_GUEST_CACHE=1 pvisor run --executor vm --safe -- YOUR_COMMAND
```

协商 `FUSE_PVISOR_META`（init flag bit 41）后，guest 可以把宿主合并后的目录
页缓存在 virtio-fs 内部。Overlay 的全部物理 lower 必须显式声明为
`Immutable`，且 upper 的命名空间修改只通过该 VM 的文件系统连接进行；
独立 `immutable_filesystem` share 和虚拟-only 文件系统也支持目录镜像。
任一 lower 可变或未声明、普通 passthrough 或仅只读的挂载都在枚举前对
`READINDEX` 返回 `ENOSYS`，guest 在连接上记住拒绝，后续目录不再重试，
并保留普通 FUSE lookup/readdir 和原有元数据期限，避免永久隐藏宿主新增文件。
惰性只读 open 独立于此资格，目录镜像被拒绝时仍可使用。
已导入且完整的目录上，lookup、getattr 所依赖的
dentry、readdir，以及普通文件和目录的只读 open，都在 guest 内核内完成。
单文件首次 lookup/stat 使用原生 FUSE 缓存；同一目录积累一定数量的不同名字后才
尝试导入一页；以已有目录大小作准入提示，门槛限制在 4–64 次不同名字的
缺页，目录大小不被当作准确项数。初始窗口为四个客机页（当前固件 16 KiB）。只有该窗口包含
请求的名字，且已访问计数达到窗口项数的四分之三，后续缺页才扩到协商预算；
导入时已存在的正 dentry 立即计入，其他 inode 只计首次访问，避免大目录反复取小页。
未命中或利用不足就停止该目录的推测性预取，回到按需查询。重复访问同一
名字不推进首次准入。显式枚举仍可完整导入目录。
装载期间的并发 lookup 直接回退宿主，避免等待预取或消费尚未完成引用归属
处理的目录项。命中计数只接触 inode 状态，支持 RCU 路径，不延长镜像寿命。
尚未完整导入时发生 create/unlink，丢弃镜像并回退宿主，避免分页 cookie 错位。
一次请求使用协商得到的 FUSE 页预算，
最大 1 MiB（256 个 4 KiB 客机页）。宿主用一次 `readdirplus` 填满这次预算，
并在返回前关闭这次目录句柄。只有带 `COMPLETE` 的页能证明某个名字不存在。这一页包含
`synthetic_entries` 里的注入文件（例如 `/init.krun` 和
`/.pvisor-guest.json`）；它们不在底层目录的 `readdir` 结果中，但完整镜像
仍必须能按名字找到它们。
一次性注入文件保留零 entry/attribute 缓存期限；guest 只记录其名字，
不预先创建 dentry，也不在 readdir 中列出，查找仍交给宿主 LOOKUP
消费名字。RELEASE 同时移除注入 inode 和残留名字，不能被完整镜像复活。
注入名字使用稳定 inode cookie，底层 cookie 使用独立的正数区间；
分页之间消费注入文件不会使后续目录项跳过或重放。
顺序读取的 readahead 使用同一个页预算，大文件的 `READ` 以最多 1 MiB 往返。
旧宿主若通告更少的 `max_pages`，guest 按那个较小预算请求。
文件字节、可写 open、create、unlink、rmdir 和 copy-up 仍到宿主。create
和 unlink 成功后，guest 在本地目录镜像里记下新名字或 whiteout，后续
lookup 不再为这个名字询问宿主。rename 直接丢弃相关目录镜像，下一次
lookup 回到宿主。

只读普通文件的宿主句柄延迟到第一次 I/O；guest 保存其原始打开凭据，
在 unlink、rename、copy-up 或属性修改前绑定仍未使用的 fd，关闭与绑定串行化。
因此 `open → unlink → read(fd)` 和覆盖重命名仍读取原对象。实际绑定
失败时取消该次修改，已经成功绑定的 fd 仍然有效；原始凭据在绑定或关闭时释放。
绑定遵守宿主的 data-cache flags，不把可变对象强制改为 KEEP_CACHE。
`SEEK_SET/CUR` 和有效元数据下的 `SEEK_END` 不触发 OPEN；需要刷新属性
时，尚未绑定的 fd 按 inode 查询，不使用不存在的 fh。稀疏文件
`SEEK_DATA/HOLE` 仍先绑定。目录镜像不持有子 dentry 的强引用，目录可随
正常 inode 回收释放其镜像。

真实 guest 回归探针覆盖惰性打开、unlink/覆盖 rename、copy-up、并发读取与关闭、
冷扫描与反复 open/read 竞争、部分镜像的缺页/创建/枚举、预取触发项的读句柄、
可变 lower 的外部原子创建，以及扫描后卸载稳定目录：

```bash
rustc --edition=2021 --target aarch64-unknown-linux-musl -C linker=rust-lld \
  crates/pvisor-vm/src/probes/guest_metadata_guest.rs -o /tmp/metadata-guest
cargo build --locked -p pvisor-vm --example guest_metadata_case
target/debug/examples/guest_metadata_case ROOTFS FIRMWARE /tmp/metadata-guest full
```

Linux x86_64 使用 `x86_64-unknown-linux-musl`；macOS 必须给宿主探针签名，
使用 `crates/pvisor/macos-hypervisor.entitlements`。ROOTFS 须为调用方保持
不变的 Linux 根目录；探针写入独立临时 upper。设置 `PVISOR_FS_PROFILE=1`，
分别运行 `baseline` 和 `warm`，可核对重复 1,000 次 open/stat/普通 seek
没有增加宿主 OPEN/GETATTR/FLUSH/RELEASE 请求；这不是性能基准。
固定其他设置后，也可分别设置 `PVISOR_VM_GUEST_CACHE=0/1` 运行同一个
`warm` 探针，核对关闭后 `READINDEX` 为零且普通 OPEN 等请求恢复。
计时对比应关闭 profiling；上述请求统计用于确认两组实际走了不同路径。

`atime` 模式使用现有固件诊断 read 后的属性失效，须设置
`PVISOR_FS_PROFILE=1`。它对 root lower、root upper、Immutable share lower
和 Mutable share lower，分别执行 `SIZE|MTIME`、`ATIME`、`BASIC_STATS`
三种精确 `statx` 请求；每种请求比较不读、首次读和同 fd 的缓存读。
每次先以 `AT_STATX_FORCE_SYNC` 刷新完整基本属性，再用同步诊断 ioctl
分别包围 prime、read、首次 stat 和重复 stat，总计 36 个 `ATIME_CASE`、
144 段和 288 个 token。输出包含各段 token、实际返回的属性和
`prime_to_first_ns`，可核对 GETATTR、READ、OPEN 的差分并辨别属性期限到期；
不睡眠、不修改内核或挂载的 atime 策略，也不把插桩耗时作为性能结果。
token 在整个探针中唯一，`scope` 指定该段所属设备；只有该设备输出该 token。
其他模式的 fixture 与固定计数保持不变。

未打补丁的 guest 不置该 flag，行为与逐请求 FUSE 相同。旧宿主不回该
flag，或对 `FUSE_READINDEX` 返回 `ENOSYS` 时，新 guest 关闭目录镜像。
该镜像不解析 Lazy Image 的页格式，也不把元数据放进 DAX 窗口。固件需
重新执行 `just fw build` 后，guest 才会带上这部分 FUSE 改动。

公开 API 的真实 guest 正确性用例为 `readonly_dax_case`：

```bash
cargo build --locked -p pvisor-vm --example readonly_dax_case
target/debug/examples/readonly_dax_case ROOTFS FIRMWARE IMMUTABLE_DIRECTORY
```

ROOTFS 须包含 `/usr/bin/python3`，FIRMWARE 是支持 DAX 的 libkrunfw 文件；
输入目录须含 `payload`，大小超过 6 MiB，第 i 个字节等于 `i % 251`。
用例将 rootfs 写入隔离到临时 overlay；DAX 窗口仅 2 MiB，检查 inode hint、
顺序/并行读取、mmap/私有 COW、关闭文件后的映射、虚拟文件普通读取和
写入拒绝。PASS 是正确性结果，不是性能 benchmark。

## 验证入口

`just test pvisor-vm` 运行设备、快照和接口契约测试；macOS 自动使用仓库既有 Hypervisor entitlement 签署测试程序。真实 HVF/KVM 和本地 socket 测试需要宿主权限。Linux 测试中创建 VM 的用例需要可用 `/dev/kvm`。

`repository_boundary` 检查所有工作区 manifest 与外部 Rust 调用者，保证 VM 核心组件只在本 crate 内编译。`api_contract` 保证 API 无条件编译、无方法体，并禁止私有适配器另设公开固有方法。

真实 Linux guest 的独立 rootfs/RAM 保存与恢复验证使用 `just test-environment-snapshot`，整机冷恢复使用 `just test-linux-cold-restore`（均为 Apple Silicon HVF 的私有专项入口）。报告保存到 `${CARGO_TARGET_DIR:-target}/vm-validation/`，可传 `--report PATH` 覆盖；固件需先通过 `just fw build` 准备，并将 `PVISOR_CASE_VM_LIBRARY_DIR` 指向生成的 dylib 目录，默认读取构建目录的 `release/`。guest 探针在 `src/probes/guest_linux.rs`，readiness 使用原子 rename 发布，避免把探针写文件的中间状态误判为恢复失败。底层 CPU/RAM 与 VMM-thread CPU/RAM/GIC 检查分别使用 `just test-hvf-cold-restore` 和 `just test-vm-snapshot-state`，它们的报告只描述各自覆盖的范围。Just 负责编译、签名与签名校验；Python 只运行已构建的探针并检查状态和报告。环境快照验证仍为源进程与恢复进程复制同一个已签名 runner 到私有目录。

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

设备提供一个 hiprio 和多个普通 request queues；默认普通队列数为宿主
可用 CPU 数，上限 4，guest 可只初始化其中一部分。每个已初始化队列由
专有线程直接取请求、执行和发布完成。hiprio 线程只处理 FORGET、
BATCH_FORGET 和 INTERRUPT，不承担普通 I/O，也不参与 work stealing。
INTERRUPT 当前不执行实际取消，独立队列不表示阻塞 syscall 可立即取消。

普通线程通过自己的 epoll/kqueue 直接等待和消费 guest kick，并直接
执行、发布请求；普通请求没有协调线程到 owner 的唤醒交接。线程优先
服务自己的队列，空闲时只认领重操作持续至少 2 ms 的 owner ring 中
尚未执行的请求，轮换 victim。READ/WRITE、同步和可能 copy-up 的修改
操作属于候选；LOOKUP/GETATTR 等短操作不会仅因 busy 被偷取。
2 ms 是耗时启发式，不证明宿主线程处于内核阻塞状态。

控制线程只等待 stop，并每 2 ms 检查长操作与真实 ring backlog；有积压
才唤醒一个已初始化的空闲 thief，不消费或转发普通 kick。队列线程空闲
时无限期等待，不周期轮询；控制线程有上述周期检查成本。认领时在
ring 锁内重新检查当前执行时长，拒绝过期救援提示。认领和 used-ring
发布使用队列短锁，I/O 在锁外执行；已执行请求不迁移，完成回写原队列。
没有中间工作列表或完成 channel，每个执行线程至多持有一个在途请求。
所有普通线程都阻塞时没有额外救援线程。

执行线程持有已校验的 descriptor 地址与长度，在执行线程内创建借用的
Reader/Writer，保留 RAM access lease 直到 used ring 发布完成。禁止通过
延长 VolatileSlice 的生命周期或新增 unsafe Send 绕过这个契约。
FUSE header 只解码一次；INIT/DESTROY 独占 session guard。

descriptor plan 在认领时校验全部输入、输出范围与累计长度；header
按捕获地址读入固定大小对象，不构造临时 Reader/Writer。执行线程仅为
body 与 reply 建立一次借用 consumer，不重新读取 opcode。队列主动
drain 时保持 guest kick 通知关闭，仅在空队列时 enable/recheck 后休眠；
控制线程独立检查长操作和 backlog，因此仍可救援正在执行的 owner。

OverlayFs 的普通 READ/WRITE/FLUSH/FSYNC 直接在 handle map 和
I/O admission 短锁内取得原生 Arc lease，不取得 namespace operation guard。
原生文件锁协调该 handle 的读写与同步。namespace mutation、copy-up、
writable OPEN 仍串行，但只关闭受影响 handle 的 admission
并等待其在途 I/O：路径范围覆盖相关文件、目录子树和已知原生对象别名，
RELEASE/RELEASEDIR 使用 namespace 读锁，允许无关元数据查询继续；RELEASE
只等待被释放的 handle。FH GETATTR/LSEEK 的原生调用同样持有 admission
lease，关闭不会撤销它们正在使用的 mapping；目录枚举持有 Arc 快照。
scope 屏障保持到修改完成，失败也会释放。
没有受影响 handle 的路径屏障不取得 admission 锁，也不通知等待者。
普通 I/O 完成只在全局排空或本 handle 的屏障已登记时通知；屏障关闭和
排空检查仍在同一 admission 锁内，避免 close/freeze 丢失唤醒。
等待时不持有 handle map 或 admission 锁，无关 handle 的新 I/O 仍可进入。
LOOKUP/GETATTR 和目录元数据查询仍遵循 operation guard 的 namespace
串行约束。capture/restore/destroy 使用全局 admission 屏障并排空全部
lease，普通 namespace 修改不使用 mount-wide I/O drain。
普通 I/O lease 通过 Arc 共享不可变的 open 元数据，WRITE/FLUSH/FSYNC
不再逐请求分配并复制保存路径。原生 descriptor lease 与 admission 屏障
继续保护 I/O、release 和 freeze；快照保留原有 File handle 的序列化格式。
GETATTR 与 backing attributes 的重复查询只在同一请求内复用，不缓存
可变 backing 的跨请求结果。
携带 FH 的大小变更沿已打开的 upper 文件句柄执行，保留 unlink/rename
后的 ftruncate 语义，不重新解析旧路径或修改同名替代文件。句柄必须匹配
overlay inode；lower 文件句柄不能通过这条路径写入。截断前排空同一物理
文件所有打开句柄的已准入 I/O，包括删除后的别名句柄。回归覆盖删除、替换、
跨句柄 I/O 排空和 inode/handle 不匹配，以及无效、只读 upper 和 lower 句柄拒绝。
rootless OCI 的 virtual metadata 模式下，新建目录记录 guest 创建者的
UID/GID，并从虚拟父目录继承 setgid；不把宿主 UID 当成 guest 所有者返回。
Linux 回归通过临时目录检查这些行为及普通 metadata、排除路径，不依赖
OCI 镜像、外部模型或 guest 启动。
Linux regular-file LOOKUP 和无 FH GETATTR 保留 Core 的 namespace、
父目录、hardlink 与策略检查，再通过原生父目录的强引用 FD 对最终名称
执行 no-follow fstatat。每次查询重新读取属性，省去仅用于 metadata 回复
的叶子 O_PATH FD、native inode 插入与 forget。ROOT_ID 继续绑定初始化时
的根 FD；目录、符号链接、其他类型、macOS、OPEN 和 FH GETATTR 保留
原生路径，virtual metadata 的回复处理保持原有规则。
目录缓存用 `Arc` 持有原生 lookup 引用，借用者在缓存锁外执行 backing I/O；
淘汰、失效和 clear 仅移除缓存所有权，最后一个借用者离开后才 forget。
原生 inode 表在同一个 map 锁窗口内固定引用或仲裁插入，防止并发 lookup
替换已返回的 inode，以及 final forget 与引用固定之间的竞争。

freeze/reset 停止所有队列接收新请求，等待已认领请求发布 used ring，join
hiprio 和普通线程后才能返回。未认领请求保留在 guest ring；不以丢弃在途
请求缩短冻结。thaw/restore 主动扫描 available ring，不依赖
guest 再发 kick。正在执行的请求不序列化进入快照；超时由现有 runner
失败契约处理。公开 `api` 的结构和方法不随平台或 worker 数改变。

恢复机器在注册 MMIO/eventfd 前，从保存的队列清单恢复普通队列数，
不重新按宿主 CPU 数推导拓扑。原有单普通队列快照仍按其原拓扑恢复。

宿主诊断变量 `PVISOR_VM_FS_WORKERS=1..8` 现控制普通队列数量；`1`
仍有独立 hiprio 线程。`PVISOR_FS_PROFILE=1` 记录 `virtio-fs-dispatch`
的 direct、hiprio 和 stolen 请求数；
带 profiling 的运行只用于诊断，不作为性能验收。

`PVISOR_FS_PROFILE=1` 还启用私有同步诊断 ioctl `0x400876f1`
（Linux `_IOW('v', 0xf1, u64)`）。guest 在工具调用前预打开根与工作区的目录
fd，传入恰好 8 字节 little-endian token；宿主在回复前复制该设备的 protocol、
dispatch、overlay 与 Core 累计计数，并输出单行 `pvisor-fs-profile-phase`。
根设备 scope 为 `rootfs`，其他设备保留 virtio-fs tag。请求使用独占 session
guard，等已进入宿主的操作结束；它不触发 flush、capture 或额外物理查询。
关闭 profiling 时保留普通未知 ioctl 的 ENOTTY 行为，没有新公开 API。
`benchmark/pvisor/phase_profiles.py` 严格核对各设备的每个 token/实例及全部
注册实例的最终记录，原始差分保留，并另行扣除标记自身的一个 IOCTL、
80 个请求字节和一个 direct dispatch。区间包含整个 `checked()` 的子进程
启动及原有校验，不能解释为精确的 `worker_ms` 边界；内核异步 FORGET 等
生命周期工作仍可能跨界。该诊断使用独立封存 rootfs，计时批次不插入标记。

staged OverlayFS 的新会话默认不协商 FUSE_WRITEBACK_CACHE；构造前设置
`PVISOR_VM_FS_WRITEBACK=1` 可显式开启，`0` 可显式关闭。未显式覆盖的
恢复会话保留快照中已协商的模式、guest 脏页与原生 handle 模式；首次
或 DESTROY 后再次 INIT 按当前新会话策略协商。显式 `0` 仍拒绝恢复
已开启 writeback 的快照，backing、handle 与 session 一致性校验不变。
直接共享宿主目录的 passthrough 保持默认关闭。开启后工作区内容和元数据须由 guest
独占修改，不能依赖宿主并发修改的即时可见性。O_WRONLY 的原生 FD
转换为 O_RDWR 以支持部分页回读，O_APPEND 由 guest 计算 offset，原生 FD
清除 O_APPEND。未协商批量扩展时，close/fsync 和 guest 后台回写使脏数据
到达宿主；文件写入错误可能延后报告。协商状态随 backing 和 FUSE session 保存并校验，完整
机器快照保留 guest 脏页，不声称排空 virtqueue 已把 guest 脏页刷到 stage。
恢复仍拒绝 exported descriptors/DAX 等既有不支持状态。

### Host 大粒度写缓存基础

Linux staged virtio-fs 可用实验变量 `PVISOR_VM_FS_HOST_WRITEBACK=1` 开启
host 拥有的异步写缓存；未设置或 `0` 关闭，其他值（含非 UTF-8）拒绝。
首版要求 guest `PVISOR_VM_FS_WRITEBACK=0`（默认）、DAX 关闭；macOS、
DAX 和恢复 guest-writeback 会话明确不支持。直接共享目录的 passthrough
不使用此缓存。该机制不需要新增固件补丁或公开 VM API。

每个连接的 host payload 预算是 **64 MiB**，最多 **1024 个预留/排队/执行项**。
先取得预算、完整复制 guest 输入并保留 native 对象/文件 I/O lease，再回复 WRITE。
后台项不保存 guest Reader、descriptor chain 或 RAM lease。相邻、同一打开
句柄、相同 killpriv 语义及连续 offset 的写最多聚合 **1 MiB / 64 chunks**，
最老待处理写入达到 **10 ms** 或同步屏障到来时也提交；以一次 `pwritev` 执行。
单 FIFO 保证多 FD/硬链接已接收写的顺序，后续可在此合同上增加对象级并行。

私有工程变量 `PVISOR_VM_FS_HOST_WRITEBACK_CONFIG` 接受严格 JSON object，
未知/重复字段、错误类型与越界值在连接建立前拒绝；未设置使用上述默认值。
字段为 `capacity_mib`（4–256）、`jobs`（1–4096）、`batch_kib`（4–4096，
不超过容量）、`chunks`（1–64）和 `delay_us`（0–50000）。单个大请求不为
满足合批大小而拆分，单独执行；`chunks=1,delay_us=0` 是不合并的异步对照。
`qualification_lock=true` 恢复前台资格检查等待后台 native I/O 锁的旧路径；
默认 false 在保留 File 的同一个 HandleLease 中查询固定 FD，实时核对 flags、
export 和 statx，不缓存可变资格。后台仍持 native 写锁并再次核对资格。
`synchronous=true` 在相同 host-owned 复制与 worker 路径后等排空才回复，
用于测量复制、线程调度和异步接管的消融，并非原同步实现。上述控制是会话
局部实验接口，不写入 snapshot；DESTROY 后 INIT 沿用当前连接的配置。
队列诊断包含预算内预留/排队/执行项高水位、实际批次数和预算等待；每队列
退出后只记录一次，跨重连的 peak sum 不能当单次最大值。

只缓存 upper 的普通可写 native FD；请求与实际 FD 的 direct/sync/append
形态、导出 FD 和 DAX 文件保留同步路径。后台再次核对 FD 条件，保留原有
CAP_FSETID 行为。预算包含尚在复制和正在执行的数据；超预算反压，控制屏障
不消费数据额度。64 MiB 限制 payload，句柄、队列元数据和分配器另有开销。

READ、metadata/OPEN、namespace 修改、FLUSH、FSYNC、RELEASE 和机器
capture/restore 等待相应已接收前缀或已有 I/O fence。短写、EIO/ENOSPC 与
worker 异常作为 sticky error 保留到会话销毁；同步与捕获不能掩盖失败，
RELEASE 即使返回错误也完成句柄清理。DESTROY 排空并 join，新的 INIT
创建新队列；普通 owner drop 同样等待后台退出，不留下越过 stage 生命周期的写。

新缓存会话实现 FUSE_SYNCFS，并由 AugmentFs 转发：排空接收的写与文件 I/O，
同步 preimage，再对 upper 所在宿主文件系统执行真实 `syncfs`。PID 1 既有
最终同步由此能观察后台错误；该调用也可能同步同一宿主文件系统的其他写入。
未启用新缓存的会话保留原 ENOSYS 行为，不能据其 syncfs 成功宣称 backing 耐久。

首版主要覆盖保持打开文件的连续写入。WB=0 的 close 仍通过 FLUSH 排空，
大量小文件 write-close 不会因此自动跨文件合并。它提前释放已接收 WRITE 的
guest RAM 引用；后续 READ/SETATTR/FLUSH 等阻塞请求仍可能持 RAM 引用，
完整 Agent IOCP 与阻塞存储期间的 VM offload 尚未实现。设计见
[EXP-003](../../docs/src/zh/design/experimental/agent-io-completion/index.md)。

### 文件 I/O 的合版验收线

文件 I/O 先作为独立机制验收，再接入 Attempt 调度、Gateway 与 RAM
offload/resume。及格不要求所有 FUSE 请求异步，也不以 WRITE 提前返回
或单个吞吐数字代替生命周期正确性。首个候选范围是 Linux staged overlay、
独占 upper、guest writeback=0、DAX=0、host writeback=1。

| 合同 | 当前状态与剩余验收 |
| --- | --- |
| 输入接管 | 先验证、预留预算、完整复制并固定 native 对象，再确认接收；后台不得持 guest descriptor、Reader/Writer 或 RAM lease。 |
| 完成与错误 | 区分 accepted、completed 与 durable；已确认的短写、ENOSPC、EIO、worker 异常必须在同步和最终收尾暴露。当前有 FIFO 前缀屏障和 sticky error，尚无供调度器使用的非阻塞完成通知。 |
| 顺序与可见性 | 覆盖同一对象的不同 FD、硬链接，以及 read/getattr/open、truncate、rename/unlink、release。当前采用保守屏障，metadata 仍会全局排空；不能声称已支持对象级独立推进。 |
| RAM 静止 | 已确认 WRITE 的后台完成不得访问或唤醒 guest RAM；在途 READ/SETATTR/FLUSH 与预算反压仍可能阻塞冻结。后续合版必须能证明这些请求已排空，或增加 host 拥有的延迟回复机制。 |
| 封存与恢复 | RAM offload 不等于快照或 stage 封存；快照/封存须排空所需写入并检查错误，或明确拒绝。正常 guest 最终 syncfs 已有错误回传；仍须验证真实 runner 收尾期间无晚到写入、失败不能发布成功结果。 |
| 有界与控制路径 | 字节和任务预算包含预留、排队及执行；队列满、后台失败和销毁时不能遗留等待者。当前控制屏障不占 payload 预算，阻塞宿主 syscall 仍不可立即取消。 |

`write_buffer::tests::owned_input_completes_with_guest_ram_gate_closed` 验证
descriptor 引用释放、guest 输入被复用和 RAM gate 关闭后，host 队列仍以
原始字节完成。Linux 的
`host_buffer_protocol_write_detaches_guest_ram_before_backing_completion`
进一步经过真实 FUSE WRITE 解码和 OverlayFs 接管路径，分别验证成功与
延迟 ENOSPC，后台完成不修改此前的 guest 回复。
这两项是所有权边界测试，不等于真实 VM offload/restore 验收。

合版前仍须在 Linux/KVM 跑通受阻 backing 下的暂停/恢复、完整收尾与错误
注入；待处理请求、恢复后唤醒和 stage 生命周期也须联测。guest writeback=1
的历史 close→unlink→syncfs ENOENT 已有独立的宿主对象生命周期修复与 SDK
正确性证据，见下文；这不属于 host 写缓存或完整 Agent IOCP 的验收。
DAX、并行写 worker 和更大预算不作为补齐验收线的前置优化。

### Guest 跨文件异步回写

Guest writeback 仍为新会话默认关闭的显式选项：构造前设置
`PVISOR_VM_FS_WRITEBACK=1` 开启，`0` 关闭。它与
`PVISOR_VM_FS_HOST_WRITEBACK=1` 互斥；二者不能同时接管写入。
以下新固件与宿主修复的实际验证范围为 Linux x86_64/KVM，未据此验收 macOS。

固件在合格 writeback 会话中协商 `FUSE_PVISOR_WRITEBACK`（bit 42），
独立于 `PVISOR_VM_GUEST_CACHE`，继续复用原生 guest 页缓存与 FUSE WRITE。
0048 以 backing device 的实际 `WB_RECLAIMABLE` 脏页和 FUSE 自行记账的
临时 `WB_WRITEBACK` WRITE 页为依据，不再累计用户 write 字节；同页覆盖、
不同 fd 或 hardlink 别名不会重复消耗一份操作字节额度。干净页缓存不在这个计数中，
内存元数据和句柄也另有开销，不引入第二份文件内容缓存。

每个连接/BDI 的软压力目标为 guest `totalram_pages()/8`，截在 **16–256 MiB**。
它使用内核实际可用页数，不以 VM 配置的名义内存推算固定字节；协商生效后可读取
`/sys/module/fuse/parameters/pvisor_wb_budget_bytes` 查看当前目标。
该诊断参数只有只读 getter，不能经 sysfs 或 boot argument 调参。
生产者在写入 folio 解锁、释放后检查脏页加临时 WRITE 页，达到目标便唤醒原生
BDI flusher 并等待推进；错误或断连结束该等待。目标是背压水位，不是整个 guest
页缓存或 RAM 的硬上限；独立 rootfs/workspace BDI 可各占一份目标，Linux 原有
全局 dirty、回收和内存压力规则继续约束总量。

每连接最多保留 **256 个句柄 lease**，包括排队、提交与等待完成的对象。
0049 将提前提交提示设为压力目标页数的 **1/4，截在 1–16 MiB**；512 MiB guest
通常对应16 MiB提示，单次协商的 FUSE WRITE 上限仍为1 MiB。小量写入继续由首笔
待处理工作的 **10 ms** timer 推进，句柄上限和显式同步也能提前推进。
入队前在连接锁外读取近似脏页数，仅用于调度提示；不再逐次求完整 per-CPU 和，
也不重复执行写返回后的背压检查。逐 folio 的实际压力检查保持不变；入队路径只在
pending 非空时调度提交，完成线程负责把再次写脏的句柄重新入队。
0049v2 还在启动每个 request virtqueue 前预分配有界 scatterlist 与指针数组，
在原有 VQ 锁内构造并提交描述符后复用，消除满1 MiB WRITE 的瞬时16 KiB
`GFP_ATOMIC` scatterlist 分配。scratch 只存传输元数据；文件页与独占请求
argbuf 保持原有完成期所有权，其他原子分配和原生内存压力边界仍然存在。
独立完成队列先等待已提交的临时 WRITE，再执行可能发送 SETATTR 的数据完整性
回写，最后再次等待新提交 WRITE，再做 FLUSH/RELEASE。
一个 inode 的完成等待不占用提交工作队列；重新写脏会重新排队，
保持原 inode/handle 引用到最终释放。timer 是调度目标，实际延迟可能更长；
宿主准入与后端阻塞仍可能串行化实际请求，不等于非阻塞全链路 IOCP 或多文件原子事务。
宿主协商的后台请求上限仍为64，
拥塞阈值48，原生请求背压继续适用。

固件解除 FUSE 默认1%的最大脏页份额，并向原生 BDI 尽力申请 **16 MiB**
最低份额；它独立于 RAM 比例压力目标。BDI 的字节设置转换为当时的 dirty 比例，
并非持久绝对字节保证。全局最低份额不足或低内存时申请可失败并记录警告；
FUSE 严格记账及临时回写页记账保持，不能重新开启默认 WRITEBACK_ACCT 而双计。

可写普通文件的 close 不逐个等待数据完成，队列持 inode 和宿主句柄引用到收尾。
只读 close 恢复原生 FLUSH 及原有写同步/错误检查，不以 WB 协商代替本地后端资格。
显式 fsync/fdatasync 和 O_SYNC 保留同步语义；syncfs/卸载排空提交与完成两队列。
异步错误在连接中保持 sticky，后续同步及关闭可见；PID1 在报告成功前同步所有
virtio-fs 挂载，任一失败以125退出。宿主读取 stage 前须经过该边界，但它不单独
证明宿主介质耐久；仅排空 virtqueue 也不证明 guest 脏页已回写。
后台 WRITE 的短写仍为错误，不能以成功回复丢弃尾部。DAX、direct I/O 与远程锁
保留原路径；共享 mmap 脏页继续使用 Linux 原有的回写与同步，没有新增 fault 等待。

宿主在 WB 会话成功 unlink 或 rename-overwrite 后，按原 guest nodeid 保留已退休的
普通文件身份：upper 保留一个 native O_PATH lookup 引用；lower 只保留逻辑属性
墓碑，时间戳回写不修改物理 lower。关闭所有 fd 后，纯 MTIME/CTIME SETATTR
仍能绑定原对象，不重新打开旧名字，也不吞未知 ENOENT。guest LOOKUP 引用精确
跟踪到 FORGET，未发布的 READDIRPLUS/READINDEX 引用须回滚，最后一个引用释放
退休对象。仍有 detached/orphan 引用时 snapshot 返回 Unsupported，不能序列化为
失效路径。新 snapshot 保存已知引用；旧 WB snapshot 缺少计数时，仅其有限旧节点
集合保守保留到 DESTROY，新节点仍正常计数，不能推断它们已收到 FORGET。

真实 guest 回归（固件和输入路径使用绝对路径）：

`guest_metadata_case` / `guest_metadata_guest.rs` 的 `review` 模式另外验证
只读 fd 在时间戳、setxattr、removexattr 和 hard-link 引发 copy-up 后仍读原 lower；
`review-time`、`review-setxattr`、`review-removexattr`、`review-link` 可分别复现。
`review-poll` 注入 deferred OPEN 的 ENOENT，验证 poll 只返回 POLLERR。
`bench` 在同一显式 Immutable share 内执行三轮 stat256、scan300 和 open10000；
它属于 B-FS-ENG 独立 SDK 计时，不与普通 CLI 工作区的结果合并。
`review*` 与 `full` 是正确性探针，不使用其运行耗时作为性能结果。固件 0045 修复保留原生 parallel
lookup/目录 alias 协议、regular-file mutation 与 deferred-open 发布的串行化区间，
以及 auto-submount 的同步最终 RELEASE；性能需独立比较修复前后固件。

writeback 预算探针按目标挂载的 `st_dev` 定位 BDI，分别检查根与附加 share 的
strict limit、最大份额和最低字节预留，避免把无关 loop 设备当成 virtio-fs。
2026-10-16 的历史 supervisor 复测记录了普通 CLI 在 WB 开启后
close→unlink→syncfs 返回 ENOENT，旧0044/0045固件均可见；旧制品与失败现场保留。
2026-10-17 的新宿主 SDK 回归在新0048与旧0047固件上均通过完整探针，
包括 fd 已关闭的 unlink/rename-overwrite 后 syncfs；这验证宿主对象退休修复
不依赖新压力算法。新宿主+0048的 backing EIO 用例也通过，PID1 以125报告失败。
本地[正确性收据](../../benchmark.new/filesystem/.data/guest-writeback-correctness-20261017/receipt.json)
关联三组成功与旧组合失败日志，保留为0048历史证据。

0049v2 的[独立正确性收据](../../benchmark.new/filesystem/.data/guest-writeback-tuning-correctness-v2-20261017/receipt.json)
关联新固件 FULL、故障注入 ERROR、同 inode 多 FD、7项固件合同及当时50项
filesystem Python 合同；511项 Rust 通过与8项跳过明确复用未变化的宿主实现范围。
原失败写入历史随后在512 MiB guest／2 GiB父预算下由5个全新 guest 完成，
[压力验证](../../benchmark.new/filesystem/.data/guest-writeback-memory-v2-20261017/verification.json)
保留225行、guest与host全字节校验及原分配错误未复现的记录。
新的四臂 N5 曲线完成20个环境、900行；普通 CLI 则以同一0049v2固件对照
WB0/1，PRE、独立诊断与 N5正式环境均通过。CLI七工具整体及 Ready 未检出收益，
逐工作负载结果与回退点见[独立实验](../../docs/src/zh/design/filesystem-io-performance-analysis.md#guest-writeback-tuning)
和[普通 CLI 数据](../../docs/src/zh/design/filesystem-guest-writeback-cli-comparisons.csv)。
探针时间不作为性能样本，新会话默认仍关闭 guest writeback。

```bash
rustc --edition=2021 --target x86_64-unknown-linux-musl -C linker=rust-lld \
  crates/pvisor-vm/src/probes/guest_writeback_guest.rs -o /tmp/writeback-guest
cargo build --locked -p pvisor-vm --example guest_writeback_case
PVISOR_VM_FS_WRITEBACK=1 target/debug/examples/guest_writeback_case \
  ROOTFS FIRMWARE /tmp/writeback-guest full
PVISOR_VM_FS_WRITEBACK=1 target/debug/examples/guest_writeback_case \
  ROOTFS FIRMWARE /tmp/writeback-guest error
```

上述命令对应 Linux x86_64 新验证路径。父进程拥有临时 upper，并校验
空闲定时回写、256文件关闭重开、rename/unlink、截断、关闭后立即卸载和最终提交。
当前 full 探针还写入128 MiB以越过该小内存 guest 的压力目标，对同一4 KiB页
覆盖16384次，并核对 O_SYNC 与8 MiB共享 mmap/msync及完整内容。故障用例用
独立 runner 的文件大小限制模拟后台写失败，要求最终125。旧macOS/HVF入口仍需
原有 entitlement；没有本次0048/宿主修复的对应新验证。PASS仅表示所述正确性范围。

新增同 inode 多 FD 回归使用4线程独立打开同一文件，三代重写互不重叠的1 KiB
字节区间；这些区间共享4 KiB页，部分写跨页。文件为2 MiB加256字节，累计写入
6 MiB，交错114次 checked-close/reopen，并在写线程仍活动时短暂空闲15 ms，
给提交 timer 推进机会。最后显式 fsync、syncfs，guest 与宿主分别核对完整长度及
全部字节。该新增 full 探针已在0048基线上通过；[独立调用记录](../../benchmark.new/filesystem/.data/guest-writeback-tuning-correctness-20261017/full-baseline-0048.invocation.json)
与原0048性能批次分开。0049v2 的新 FULL 也通过该用例，记录在上方的新正确性收据中；
该正确性结果不用于推导性能收益。

本次只修改 VM virtio-fs 执行路径，不改宿主 FUSE 的 dispatch。
回归覆盖直接 kick/stop、短操作不偷取、长操作 backlog 原 ring 完成、
同对象屏障、无关 namespace 修改及等待期间新 I/O admission，并保留
hiprio、RAM lease、冻结与 writeback 恢复覆盖。性能结果使用独立冻结
release 制品与正式配对批次，不由并发单测推导收益。

设计参考 [virtiofsd 的线程池和 EVENT_IDX 处理](https://gitlab.com/virtio-fs/virtiofsd/-/blob/main/src/vhost_user.rs)
及 [passthrough 的资源持有方式](https://gitlab.com/virtio-fs/virtiofsd/-/blob/main/src/passthrough/mod.rs)。
并发调度使用 pVisor 内部实现，不复制上游代码；不提供 vhost-user 或
上游的工作目录切换。只读 DAX 使用上文的独立权限、缓存和冻结契约。
