# pvisor-vm

pVisor 的 Rust VM 运行时。VMM、设备、架构支持和虚拟化后端在同一个 crate 中编译，替代原来分散的 libkrun 组件与 C 风格 context API。

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

## 核心接口模型

所有定义集中在 `src/api.rs`，实现集中在私有适配器中。文件系统配置使用 `PathBuf`，UTF-8 校验及内部设备格式转换由运行时完成。

| Trait | 实现 struct | 契约 |
| --- | --- | --- |
| `RuntimeSupport` | `VmPlatform` | 能力、日志、内核/固件准备及冷 RAM 诊断 |
| `VmConfiguration` | `VmBuilder` | 在启动前接收配置与资源所有权 |
| `VmRuntime` | `VmBuilder` | 消费配置，启动一个独立 runner 的 VM |
| `VmControl` | `VmmHandle` | 查询、暂停、恢复、RAM offload；可用于动态分派与 mock |
| `SnapshotControl` | `VmmHandle` | 在完整冻结窗口执行带类型返回值的动作 |
| `SnapshotCapture` | `FrozenMachine` | 捕获 CPU、RAM 和设备状态；冻结 guard 不可逃逸 |
| `ColdRamControl` | `VmmHandle` | 冷 RAM worker、冻结、驻留查询和缺页处理 |
| `ColdRamStore` | 宿主存储适配器 | 不可变 block 的发布、校验恢复和引用释放 |
| `RamFileMapping` | `RamFileMount` | mmap 兼容 RAM 文件与 FUSE 生命周期 |
| `RamFileStore` | 宿主存储适配器 | 有界暂存 I/O，与代际发布区分 |
| `FrozenMemory` | `FrozenMachine` | 冻结窗口内的内存操作 |
| `SnapshotState` | `MachineSnapshot` | 快照清单查询和文件系统副本绑定 |
| `RestoreState` | `MachineRestore` | 恢复校验和私有 RAM 映射 |
| `RamAccess` | `RamBlock` | RAM 块访问与 unsafe 前置条件 |

`VmConfig` 表达 CPU 与 RAM 配置；`Capabilities` 统一描述架构、虚拟化平台及可用原语。`MachineSnapshot`、`MachineRestore` 与 `RamMappingSnapshot` 表达快照边界；`OverlayConfig` 与 `PermissionSemantics` 表达文件系统输入。所有 trait 仅声明方法，不提供默认实现。

`VmBuilder.inner` 仅保存内部状态，不承担对外接口定义。`VmmHandle` 直接保存私有运行状态，在 `handle.rs` 实现 `VmControl`，不再通过第二个 handle 类型逐方法转发。Rust 不支持无方法体的固有方法声明，因此接口使用 trait 声明，私有适配器使用 `impl VmConfiguration for VmBuilder` 等实现。调用方显式导入所需契约：

```rust
use pvisor_vm::api::{VmBuilder, VmConfig, VmConfiguration};

fn configure() -> std::io::Result<VmBuilder> {
    VmBuilder::from_config(VmConfig { cpus: 2, memory_mib: 256 })
}
```

## 仓库迁移规则

CLI 执行器、containerd shim、暂停/恢复、快照、checkpoint、RAM pager、示例和 guest-init 基准均使用本模块的契约。禁止恢复 `libkrun` / `krun-vmm` / `krun-devices` / `krun-hvf` 等独立核心运行时依赖，也不保留 C context/裸指针配置入口。内部硬件探针和设备测试归属 `pvisor-vm`，不要求外部访问私有模块。

RAM 文件的 FUSE 挂载、readiness、mmap 缓存 I/O 和卸载顺序由私有 `ram_file` 模块管理。`RamFileStore` 接收宿主的暂存存储实现；`RamFileMount` 只公开所有权与挂载契约。压缩代际提交和持久化发布由宿主存储层负责。

冷 RAM pager 的状态机、采样/发布窗口、回收线程及 CPU/设备缺页恢复也在本 crate 内。`ColdRamStore` 是外部存储适配契约：pVisor 只负责 pool 连接授权、存储传输和产品诊断目录，VM 指针与映射状态不越过边界。`ColdRamOptions` 在所有平台都存在；不支持的后端返回明确错误，重复启动同一 VM 的 pager 会被拒绝。

`GuestCommand` 配合禁用 implicit init 的自定义 init；普通 Rust supervisor 继续使用 `/.pvisor-guest.json`。参数和环境不会继承宿主值，拒绝不支持的引号、控制字符、保留环境键及超长命令。`NetworkOptions` 显式控制自定义 init 的 DHCP，请求不会开启 TSI。`network` 默认关闭 DHCP。

Rust supervisor 的可选私有 tmpfs 契约见 [pvisor-guest](../pvisor-guest/README.md)。容量属于已有 guest RAM 预算，工作区 stage 与临时 RAM 数据面分别管理；执行器默认策略不改变本 crate 的跨平台 API 定义。

静态 x86_64 musl 的内核提取、无损打包、加载全部在本 crate 内完成；既有 `PVISOR_KRUNFW_PATH` / `PVISOR_KRUNFW_KERNEL_BUNDLE` 构建输入保持兼容。`VmRuntime::run` 自动安装构建内置内核；`VmPlatform::embedded_kernel` 提供不可变共享字节和启动地址用于身份绑定。固件的版本、校验、缓存、下载和平台产物处理集中在本 crate 内；调用方决定何时授权并调用阻塞的准备操作，负责宿主隔离和证据存储。

为保持已有 Run/证据协议兼容，部分记录标识、trace stage、环境变量和 runner 参数保留历史 `krun` 命名；它们不再调用原 C API。既有原始基准证据保持原样，不能当成新实现的验证结果。

Clippy 清理以语义和契约为先：保留 `EAX/EBX/ECX/EDX`、`RTC` 等硬件专名以及诊断含义明确的错误名称，必要时使用带理由的局部豁免。优先删除失效豁免、整理配置与资源参数、修复实现问题，不为消除告警改变专有术语或持久化协议。生成的 ABI 定义、跨平台 libc 字段宽度和 FUSE 协议签名需单独判断。

## 验证入口

`just test pvisor-vm` 运行设备、快照和接口契约测试；macOS 自动使用仓库既有 Hypervisor entitlement 签署测试程序。真实 HVF/KVM 和本地 socket 测试需要宿主权限。Linux 测试中创建 VM 的用例需要可用 `/dev/kvm`。

`repository_boundary` 检查所有工作区 manifest 与外部 Rust 调用者，防止重新依赖旧 VM 核心 crate。`api_contract` 保证 API 无条件编译、无方法体，并禁止私有适配器另设公开固有方法。

真实 Linux guest 的独立 rootfs/RAM 保存与恢复验证使用 `python3 scripts/check-environment-snapshot.py --report target/vm-validation/environment-linux.json`（Apple Silicon HVF）。guest 探针在 `src/probes/guest_linux.rs`，readiness 使用原子 rename 发布，避免把探针写文件的中间状态误判为恢复失败。底层 CPU/RAM 与 VMM-thread CPU/RAM/GIC 检查分别使用 `check-hvf-cold-restore.py` 和 `check-vm-snapshot-state.py`，它们的报告只描述各自覆盖的范围。

## Stage snapshot rebinding

`api::SnapshotState::rebind_filesystem_stage` relocates independently verified
upper/work/preimage copies while retaining explicitly leased immutable lowers.
The host coordinator imports and pins bases, validates stage inventories, and
holds leases until VM exit. The runtime validates overlay topology and saved
inodes/handles; writable stage roots cannot be retained as immutable lowers.
Legacy full-tree/layer rebinding remains separate. The trait is available with
the same signature on every platform; unsupported backends return an error.


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

OverlayFs 的 READ、LOOKUP、GETATTR、目录查询等可共享 operation guard。
改名、copy-up、写入、前像首次观察、release 与快照恢复仍使用独占 guard；
读取通过 backing I/O 持有共享 guard，阻止原生句柄提前释放。文件句柄和
不可变目录项只在查表时持有 handle map 锁，不把整张表锁带入 I/O。

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
本次为 pVisor 内部实现，未复制上游代码。未直接引入 vhost-user、DAX、
writeback 或上游的工作目录切换；这些机制需要各自的权限、缓存和冻结契约。
