# VM 缓存优化逐项验证报告

## 主要结论

**分析提出的重复加载与 OPEN 成本已在真实 KVM guest 中得到验证。rootfs 数据缓存使 Cargo 快 20.17%、npm 快 1.14%；独立 N30 的完整七工具任务仍未检出加速。受限只读 no-open 能让稳态 open/fstat/close 达到零文件系统请求，是值得继续接入的方向；它尚不适用于普通混合可写 rootfs/workspace。**

| 方向 | 验证结果 | 决策 |
|---|---|---|
| rootfs 跨 open 数据缓存 | rootfs READ 减少 47.1%；Cargo 的正式改善明确 | 保留为稳定 rootfs 输入的候选，继续完善准入；不能宣称整轮提速 |
| rootfs 长元数据 TTL | LOOKUP 减少约 3.0%；完整任务改善未检出 | 不单独升级为默认优化 |
| workspace 长元数据 TTL | LOOKUP 减少 43.2%；完整任务改善未检出 | 留作实验，先补足混合视图失效合同 |
| 独立只读 no-open | 512 次暖 open/fstat/close 的新增请求为 0；重复 exec 有阶段改善 | 保留只读机制原型，优先研究工具闭包的实际接入 |
| 元数据导入与混合选择性本地 open | 初次查询仍有请求，首次 exec 未检出改善；新机制尚未实现 | 条件性后续研究，不能计作已完成优化 |
| snapshot/restore 与长期资源成本 | 原型显式拒绝未验证的保存/恢复；尚无长期密度数据 | 完成相应合同后再接入生产 |

这些结果来自冻结原型的工程 A/B 和独立诊断，不替换[用户跨后端表](../../docs/src/zh/benchmarks/filesystem.md)中的 5.46 s。实验源码、配置和二进制保留在本地 `.data/`。之后仅接入具备物理不可变声明的 rootfs 数据缓存默认路径，准入比原实验更严格；元数据长 TTL 和 no-open 仍未接入。新增默认没有独立端到端性能测量，不能直接套用本批倍率。

## 动机

需要判断 VM 多服务的一整套 rootfs，以及普通 OPEN 的缓存失效，是否构成可优化成本。验证同时覆盖请求数量、工具阶段与完整任务，避免将请求减少或重复 exec 的倍率当作整轮收益。

## 实验设计

注册角色分别为 B-FS-ENG（计时）和 B-FS-DIAG（插桩计数），见[注册表](../../benchmark/README.md#b-fs-eng)。所有原始样本、日志、失败尝试、输入清单、冻结源码、制品和验收收据分别保留；不同批次不合并。

CLI 使用同一冻结 release/gateway 二进制，仅改变真实缓存模式。包含当前的 I/O 屏障与 journal 修复；READ/WRITE 路径、队列调度、固件和七工具输入相同。整组 affinity CPU 0,1、两核 quota、16 GiB、swap 0，guest 2 vCPU/16 GiB；Btrfs、热宿主缓存、每任务独立工作区与工具缓存、新 namespace/pivot_root/Host listener。namespace 与输入准备不计时，Host/VM 启动和正常退出计入完整任务；新 VM 写合并和 DAX 关闭。

数据缓存原型保留现有 OPEN、策略/基线观察与后端句柄，只在 rootfs lower 的只读 OPEN 返回 KEEP_CACHE。元数据原型单独改变指定导出的回复 TTL；LinuxSimplified 的上下文相关零属性 TTL 保留。输入是由实验持有的封闭、稳定工具树；O_RDONLY、只读挂载或 OCI 路径本身不证明通用视图稳定性，产品接入还需明确准入。

- CLI 快速筛选：五条件各 N10/W1，50 个计时任务，随机同轮排列。
- CLI 独立正式确认：基线与 rootfs 数据缓存各 N30/W3，60 个正式任务及 6 次预热。只有这一批用于正式端到端结论。
- CLI 请求诊断：五条件各三个插桩任务，15 个任务；两个明确 tag 的协议实例分别对应 rootfs/workspace。每个 VM 的全部 13 个观测 profile 实例都有 final 记录，13 是组件实例数，不是 13 个 virtio-fs 设备。
- 受限只读机制：同一公开 VmBuilder 驱动的实验 subject，2 vCPU/512 MiB guest，整组两核/2 GiB/swap 0，FD 限额 65,536，普通 rootfs 缓存关闭。所有模式共用数据、元数据与验证过的同字节 ELF/loader/动态库副本；数据、元数据和 no-open 逐项切换。四条件各 N5/W1，仅是探索性阶段筛选。
- 只读独立诊断：各模式分别执行 prime-only、prime 加 512 次 open/fstat/close、完整 read/mmap/pread/exec 窗口，共 12 个 guest。所有预期 tag 的协议实例及观测组件都有 final 记录。

正式计时关闭 profiling；诊断耗时不进入正式分布。配对 95% bootstrap 区间使用 5,000 次同轮重采样，seed 20261012；N5/N10 是探索性证据。分离簇报告各簇中位数和数量；不报告 P99。任何输出/字节/隔离/制品/输入/预算错误、OOM 或检出的外部构建、测试、KVM 干扰都拒绝整批，不剔除有效慢样本。0.5 秒进程扫描可能漏掉短暂、不可见和未识别活动。

只读的各阶段使用 guest CLOCK_MONOTONIC_RAW。完整墙钟包含初始化、priming、1.2 秒 TTL 等待、阶段校验与退出；工具闭包准备在计时外，其离线成本尚未测量，不把阶段时间相加替代完整任务。每个 VM 的 20 次重复 exec 先求本 VM 均值，外层 N5 是五个新 VM，而不是 100 个独立样本。

## 数据与分析

### CLI 正式确认

仅比较 rootfs 数据缓存，N30/臂；单位 ms。负百分比表示耗时减少。

| 指标 | 基线 P50 | 数据缓存 P50 | 变化 | 配对 95% CI | 判断 |
|---|---:|---:|---:|---|---|
| Ready | 727.08 | 725.25 | -0.25% | [-1.11%, +0.51%] | 未检出差异 |
| 完整任务 | 4573.07 | 4351.70 | -4.84% | [-9.28%, +0.65%] | 未检出差异 |
| 工具与校验总时间 | 3626.81 | 3383.29 | -6.71% | [-11.88%, +0.53%] | 未检出差异 |
| 遍历 2,048 文件 | 146.37 | 141.07 | -3.62% | [-12.37%, +10.85%] | 未检出差异 |
| 读取校验 64 MiB | 117.59 | 118.18 | +0.51% | [-2.62%, +2.01%] | 未检出差异 |
| 写入 256 文件 | 128.64 | 123.68 | -3.85% | [-10.82%, +2.46%] | 未检出差异 |
| git status | 378.54 (n=17); 701.86 (n=13) | 360.53 (n=18); 691.21 (n=12) | — | — | 分离簇，不排名 |
| rg | 369.13 | 378.54 | +2.55% | [-1.54%, +4.45%] | 未检出差异 |
| 离线 Cargo | 485.10 | 387.24 | -20.17% | [-21.50%, -18.88%] | 更快 |
| 离线 npm | 936.24 | 925.53 | -1.14% | [-1.95%, -0.40%] | 更快 |

完整任务点估计减少 221.37 ms，但置信区间包含零。不能将 N10 的探索性改善提升为正式整轮提速，也不能据此宣称历史退化恢复。Cargo 的 97.86 ms 改善支持工具链重复加载成本可被缓存减少；npm 改善较小。其余操作未检出差异或出现分离簇。

正式完整精度 CSV（本地归档：`.data/derived-tables-20261018/vm-cache-confirm.csv`）保留分布与精确区间。两臂的整组 CPU 时间中位数为 6.052 s 和 5.653 s，此处是描述性观测；未给出该资源指标的配对区间，不作为 CPU 收益结论。

### CLI 快速筛选

N10/条件；单位 ms，完整任务。此批不能与正式确认合并。

| 候选 | 基线 P50 | 候选 P50 | 变化 | 配对 95% CI | 判断 |
|---|---:|---:|---:|---|---|
| rootfs 数据缓存 | 4610.83 | 4260.10 | -7.61% | [-9.72%, -2.89%] | 探索性改善 |
| rootfs 元数据缓存 | 4610.83 | 4539.85 | -1.54% | [-4.26%, +5.87%] | 未检出差异 |
| workspace 元数据缓存 | 4610.83 | 4518.50 | -2.00% | [-5.48%, +1.36%] | 未检出差异 |
| 三项组合 | 4610.83 | 4218.18 | -8.52% | [-10.51%, -2.22%] | 探索性改善 |

组合相对数据缓存的点估计为 -0.98%，探索性 95% CI [-7.08%, +3.81%]，未检出额外收益。因此正式确认选择较小的数据缓存候选。快速筛选 CSV（本地归档：`.data/derived-tables-20261018/vm-cache-screen.csv`）

### rootfs 与 workspace 请求

独立 N3 插桩诊断的请求数中位数，无计时归因。

| 变化 | 请求类型 | 基线 | 候选 | 减少量 |
|---|---|---:|---:|---:|
| rootfs 数据缓存 | rootfs READ | 6,130 | 3,244 | 47.1% |
| rootfs 元数据缓存 | rootfs LOOKUP | 4,608 | 4,469 | 3.0% |
| workspace 元数据缓存 | workspace LOOKUP | 3,914 | 2,222 | 43.2% |
| rootfs 数据缓存 | rootfs OPEN | 1,347 | 1,347 | 0 |
| rootfs 数据缓存 | rootfs FLUSH / RELEASE | 1,304 / 1,346 | 1,304 / 1,346 | 0 |

数据缓存减少重复回源，却保留了打开与关闭控制请求；延长 rootfs TTL 对首次路径查询没有明显帮助。workspace 在数据缓存条件下仍有约 4,501 次 OPEN、4,789 次 FLUSH、4,784 次 RELEASE，说明局部请求数量问题仍需分别处理。以上计数不提供精确的耗时占比。

### 受限只读缓存与 no-open

只读实验把权限与来源稳定性收窄到已验证的 profile。元数据/数据缓存条件与 no-open 条件使用相同的 60 秒 TTL 和 KEEP_CACHE，再比较是否跳过普通 OPEN。

| 512 次暖 open/fstat/close 的新增请求 | 有数据/元数据缓存 | no-open |
|---|---:|---:|
| OPEN | 512 | 0 |
| FLUSH | 512 | 0 |
| RELEASE | 512 | 0 |
| LOOKUP / GETATTR / READ | 0 / 0 / 0 | 0 / 0 / 0 |

初始化保留一次 OPEN 与一次 FLUSH 探测。宿主按可信 inode 和凭据延迟建立普通只读数据绑定，缓存上限 512；并发 I/O 用 Arc 保留在途绑定，淘汰不关闭仍被使用的 FD。此机制没有在混合可写连接上全局停掉 OPEN。

下表为四条件各 N5 的探索性阶段结果，单位 ms。

| 比较与负载 | 对照 P50 | 候选 P50 | 变化 | 配对 95% CI | 判断 |
|---|---:|---:|---:|---|---|
| 数据缓存：重复 exec | 4.6497 | 1.4888 | -67.98% | [-69.36%, -67.13%] | 探索性改善 |
| 元数据缓存：TTL 后 stat | 42.2862 | 0.8173 | -98.07% | [-98.62%, -97.17%] | 探索性改善 |
| no-open：512 次 open/fstat/close | 70.5717 | 0.2756 | -99.61% | [-99.64%, -99.34%] | 探索性改善 |
| no-open：重复 exec | 1.4387 | 0.8522 | -40.77% | [-42.36%, -35.06%] | 探索性改善 |
| 组合：首次 exec | 7.4096 | 7.2937 | -1.56% | [-14.29%, +32.77%] | 未检出差异 |

普通模式每次重复 exec 约有 38 个 major faults；数据缓存和 no-open 的重复窗口为零。首次 exec 仍有约 38 个 major faults，首次查询 priming 仍有 1,026 次 LOOKUP。缓存命中与首次回源是不同问题；这些结果不能折算当前 CLI 完整任务，也不证明元数据包已导入。

只读阶段完整精度 CSV（本地归档：`.data/derived-tables-20261018/vm-cache-readonly.csv`）

### 正确性与生命周期

- 独立原型 `just test pvisor-vm`：395 passed、8 skipped；包括并发首次绑定、512 项淘汰、在途引用存活和现有 VM/API/快照边界回归。跳过项不计为通过。
- 真实 guest 验证完整数据、并发 pread、mmap 私有 COW、关闭 FD 后映射、缓存压力后的重读、symlink/hardlink、不同 uid 的访问拒绝、只读写入/截断/删除拒绝以及 fsync。
- 六个混合视图 guest 比较当前模式、数据缓存、数据加元数据缓存的一个 copy-up 序列：旧 FD 仍读 lower，新 FD 读 upper，宿主 lower 未修改、upper 字节正确。它不代替全部 rename/hardlink/失效交错验收。
- 数据/元数据实验 Overlay 配置与 no-open 绑定状态的 snapshot/restore 尚未验收，原型明确拒绝；元数据导入与混合可写连接的选择性本地 open 尚未实现。不能按只读试验启用普通 mutable lower 的全局缓存。
- 已验证固定资源上限与零 OOM，尚无缓存增量内存、长期 FD 回收、并发吞吐或多 VM 密度的独立正式结果。

### 接入建议

rootfs 数据缓存现默认选择 `root-data`，但仅在 `/dev/root` 的实际选中 lower 有明确 `LayerMutability::Immutable` 声明时，对普通只读文件 OPEN 返回 KEEP_CACHE。关闭控制为 serving runner 中的 `PVISOR_VM_CACHE_EXPERIMENT=off`；普通宿主 `/`、任意 `--rootfs`、OCI 提取和 lazy 物理投影不自动取得此声明。`rootfs_immutable` 的 apply 限制不代替物理稳定保证，详见 [VM 缓存合同](../../crates/pvisor-vm/README.md#rootfs-data-cache-default)。workspace、upper、可变 lower、元数据 TTL、OPEN 检查与基线观察保持原行为。

接入代码的定向检查为 397 passed、8 skipped，CLI release/gateway 构建和 rustdoc 警告拒绝检查通过。三个短真实 KVM 正确性用例使用私有只读 noatime 输入：不可变 lower 默认模式的后端 READ 为 4，关闭控制为 36，可变 lower 默认回退也为 36；每组 OPEN 均为 36。32 次重复 READ 被缓存命中省去，copy-up 的旧 FD/新 FD 字节、拒绝访问和 lower 未修改检查通过。这是机制与正确性验证，不提供速度或整机 RAM 快照恢复结论；证据保留在 `target/cache-default-validation-20261012/`。

继续设计显式不可变只读工具导出，完成受限 no-open 的产品接入。rootfs 工具、动态库与解释器的加载路径需要实际覆盖，而非只缓存 workspace。

普通混合 rootfs/workspace 的长 TTL、选择性本地 open 和元数据导入，需要各自的能力与失效合同。首次 exec 和首次 LOOKUP 的残余成本是后续诊断目标；只有端到端净收益和生命周期验收通过后，才扩大启用范围。

### 本地证据

主归档：`benchmark.new/filesystem/.data/vm-cache-verify-20261012/`。CLI 原始批次分别在 `benchmark/.data/vmc-screen-retry/`、`vmc-confirm/`、`vmc-diagnostic-retry/`；只读在 `vmc-ro-screen/`、`vmc-ro-diag/`。冻结执行入口均通过 semspec bench 运行，正式与诊断分开。失败或验收拒绝的尝试单独保留，见主归档 `report-outline.json` 与各 launch/validity 收据，不进入上述统计。

实际 CLI 实验二进制 SHA256：`683e0ed29b501ceaeb841271dd618765acdc47abc12c581748115740fe40a190`；固件 SHA256：`b61f68dac3ef20a88e1ee387733e4baed2f7c02edac940c8882e0b549dac95e4`。CLI 冻结源码清单 2,228 个文件；只读源码与制品保护清单 2,238 个文件，完整内容留在本地，来源、CSV 与报告摘要见验收摘要（本地归档：`.data/derived-tables-20261018/vm-cache-validation.provenance.json`）。

机制参考：[Linux 6.12 FUSE OPEN/no-open](https://github.com/torvalds/linux/blob/v6.12/fs/fuse/file.c)、[virtio-fs 挂载权限默认值](https://github.com/torvalds/linux/blob/v6.12/fs/fuse/virtio_fs.c#L1432)、[EXP-002](../../docs/src/zh/design/experimental/guest-stage-cache/index.md)。
