# 内存去重与卸载机制实验报告

## 主要结论

**不可变基线共享、独立 COW 写入和原始文件回收通过三次完整性校验；KSM 登记成功，但扫描器关闭，实际 KSM 合并字节为 0。** 这是 B-MEMORY-DIAG 的底层机制诊断，不是 B-VM-MEMORY 的真实 VM benchmark，也不能据此宣称整机内存减少或生产密度提升。

| 机制 | 本批结果 | 结论范围 |
| --- | --- | --- |
| 同 inode 不可变基线 | 两份 64 MiB 映射，各 RSS 64 MiB、PSS 32 MiB | 选定映射共享基线；合计 PSS 64 MiB，不是 RSS 相加得到的 128 MiB |
| 全页独立 COW 写入 | 每份 PSS 变为 64 MiB，基线保持不变 | 写入隔离成立；修改全部页面后基线共享收益消失 |
| Linux KSM | 每份接受建议 64 MiB，`VmFlags` 出现 `mg`；`KSM: 0 kB` | 仅登记成功，未发生 KSM 合并 |
| 原始文件回收 | 64 MiB 映射 RSS/PSS、mincore 驻留均降至 0，读回后恢复为 64 MiB | 本批所选映射及文件页可回收，完整内容可读回 |

## Motivation

去重登记不是物理页合并，暂停也不是 RAM 卸载。该诊断用于确认重构后的映射、写入隔离、引用生命周期和原始 backing 回收机制，避免把 API 返回的字节数解释为实际节省。真实 VM 的 CPU/设备静默、压缩提交、业务恢复延迟和整机净收益需要另行实验。

## 实验设定

- 日期：2026-10-06；注册 ID：`B-MEMORY-DIAG`；角色：`diagnostic`。注册规则见 `benchmark/README.md#b-memory-diag`。
- 宿主：Linux `7.2.8-200.fc44.x86_64`；16 个逻辑 CPU，允许 CPU 0–15，无额外绑核；`MemTotal` 为 31,980,420 KiB。
- 编译器：`rustc 1.98.1 (48a229cea 2026-09-01)`；未优化 debug test profile。
- 宿主页：4 KiB。backing 位于仓库 `target/` 下的 Btrfs，statfs magic 为 `0x9123683e`，不是 tmpfs、ramfs 或 FUSE。
- KSM：`run=0`，`pages_shared=0`，`pages_sharing=0`，`full_scans=0`；`pages_to_scan=100`、`sleep_millisecs=20`。实验不写 sysfs、不启用全局扫描。
- 宿主不存在 `/dev/kvm` 和 `/dev/fuse`；未启动真实 VM，未运行 FUSE 压缩卸载。
- 三个独立 trial，每个重新创建映射与文件；每份映射 64 MiB。seed 为 `0x20261006 + trial`（539365382、539365383、539365384），使用确定性非零重复页，不包含不可压缩随机负载。
- 基线试验：两份映射引用相同只读 inode，先完整读取，再建议 KSM，随后对每个页面分别写入不同内容。逐字节验证两实例内容及未修改的基线；删除文件名、释放第一个映射后，验证第二映射及由其保留 FD 建立的新映射仍可读取。
- 匿名试验：两份独立匿名映射写入相同重复页，建议 KSM 后等待约 2 秒。使用不同 VMA advice 防止与相邻映射合并，以准确读取地址范围对应的 smaps；两秒等待不是 KSM 完成期限。
- 原始卸载试验：一份 `MAP_SHARED` 磁盘 backing，填充并验证全部内容，调用生产实现 `ram::reclaim`，随后完整读回并逐字节验证。使用相同 `map_snapshot_ram`、`advise` 和 `ram::reclaim` 实现，不另造替代回收算法。
- 正确性判据：完整字节比较、checksum、COW 隔离、基线不变、私有 COW 回收拒绝及 unlink 生命周期检查全部通过；失败不能作为零延迟或节省样本。
- 测量口径：按地址从 `/proc/self/smaps` 读取 RSS/PSS、Shared_Clean、Private_Dirty、KSM、VmFlags；mincore 记录文件页缓存驻留，不能等同映射 RSS。没有 cgroup/整机归因或峰值统计，没有生产负载、CPU 成本或尾延迟结论。

## 执行命令

从 `pvisor` 仓库根运行。下面的诊断命令实际执行一次，内部包含三次 trial；使用直接 `cargo test` 是因为这是显式 ignored、nocapture 的特殊诊断 runner。

```sh
uname -a
ls -l /dev/kvm /dev/fuse
cat /sys/kernel/mm/ksm/run /sys/kernel/mm/ksm/pages_shared \
  /sys/kernel/mm/ksm/pages_sharing /sys/kernel/mm/ksm/full_scans \
  /sys/kernel/mm/ksm/sleep_millisecs /sys/kernel/mm/ksm/pages_to_scan

cargo test -p pvisor-vm --lib ram_dedup::tests::memory_diagnostic --no-run

# 输出目录在运行前创建；复测应选择新的目录，不覆盖本批原始记录。
mkdir -p benchmark/pvisor/.data/memory-diagnostic-20261006
cargo test -p pvisor-vm --lib ram_dedup::tests::memory_diagnostic -- \
  --exact --ignored --nocapture --test-threads=1 --format terse \
  > benchmark/pvisor/.data/memory-diagnostic-20261006/raw.log 2>&1

sha256sum benchmark/pvisor/.data/memory-diagnostic-20261006/raw.log
```

诊断 runner 要求宿主 `KSM run=0`，并限制运行时间为 30 秒；该要求用于可重复验证“登记不等于合并”，不是启用扫描后的收益测试。若宿主扫描已开启，不要为了运行该诊断修改全局设置。

合并后相关功能回归使用的命令：

```sh
cargo nextest run --locked -p pvisor -p pvisor-vm \
  -E 'test(ram_dedup) | test(executor::vm::control::tests) | test(executor::vm::restore_ram::tests) | test(runner_checkpoint_binding) | binary(api_contract) | binary(repository_boundary)'
git --no-pager diff --check
```

该选择覆盖 42 个常规测试；显式 ignored 诊断不计入常规测试通过数。完整 `just test pvisor` / `just test pvisor-vm` 另受已有文件系统权限失败和缺少 KVM 限制，不能宣称完整套件通过。

## 实验数据与分析

### 去重：共享基线与 KSM 分开计算

以下为每份映射的实测值，单位 MiB；三次 trial 的两份映射均得到相同计数，不是跨批汇总或推测值。

| 条件与阶段 | 每份 RSS | 每份 PSS | Shared_Clean | Private_Dirty | KSM | `mg` |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| 私有基线，建议前 | 64 | 32 | 64 | 0 | 0 | 否 |
| 私有基线，建议后 | 64 | 32 | 64 | 0 | 0 | 是 |
| 私有基线，每页不同 COW 写入后 | 64 | 64 | 0 | 64 | 0 | 是 |
| 匿名重复内容，建议前 | 64 | 64 | 0 | 64 | 0 | 否 |
| 匿名重复内容，建议后 | 64 | 64 | 0 | 64 | 0 | 是 |
| 匿名重复内容，等待约 2 秒后 | 64 | 64 | 0 | 64 | 0 | 是 |

两份基线映射合计 PSS 为 64 MiB，尽管各自 RSS 都为 64 MiB。这个共享由相同文件 backing 建立，在建议 KSM 前已经存在；不能归因于 KSM。每页分别写入后，合计 PSS 为 128 MiB，说明 COW 成本必须计入容量规划。

匿名条件的每份 `accepted_bytes=67,108,864`，但每份 PSS 始终为 64 MiB，KSM 字节为 0。该宿主上**实际观察到的 KSM 去重结果为 0 MiB**；扫描器关闭，因此本实验不能判断启用扫描后的合并率、扫描 CPU 成本或完成时间。

### 卸载：原始 backing 回收和完整读回

三个 trial 的映射 RSS/PSS 均为 `64/64 → 0/0 → 64/64 MiB`，mincore 驻留均为 `64 → 0 → 64 MiB`；阶段依次为回收前、回收后、完整读回后。每次 `backed_bytes=67,108,864`，校验均 PASS。

| Trial | Seed | 填充时间（ms） | 回收时间（ms） | 全量读回＋逐字节验证（ms） | 完整性 |
| --- | ---: | ---: | ---: | ---: | --- |
| 0 | 539365382 | 6.727 | 7.807 | 107.552 | PASS |
| 1 | 539365383 | 7.430 | 8.363 | 107.493 | PASS |
| 2 | 539365384 | 7.020 | 13.018 | 114.120 | PASS |

“全量读回＋验证”包含 debug 模式的逐字节检查与 checksum，不是纯存储读取延迟，也不是 `RunHandle::resume` 或业务首次响应时间。回收后的零驻留仅描述本批选定映射和文件缓存采样，不保证每次卸载都释放全部目标字节，更不表示 VMM/设备状态为零。

诊断总时长为 10.693 秒，31 条 JSON 记录（三个 trial 各十个阶段及一条完成记录），无失败或剔除样本。只有三次观察，不报告 P95/P99，也不推导 A/B 加速比例。

## 证据与未测项

原始日志、冻结运行时源码、测试二进制及来源清单保留在本地 `benchmark/pvisor/.data/memory-diagnostic-20261006/`；`.data/` 按仓库规则不进入 Git。该报告是版本化的加工结果，文件路径仅用于本机复核，不是公开下载链接。

| 证据 | SHA-256 |
| --- | --- |
| `raw.log` | `7c76f4407f48064e25ae195c409c28b732f1eedf8b65331ec3d956f940eca6e4` |
| 实际执行的测试二进制 | `25b7d04ea8a7594d4bc82368d07105caad843076e828c93a4e8cd2b494f7457f` |
| `source-manifest.json` | `01a2e5f8876ae3a1dedefacac314c1289294a01f3405d145be1f9f57de67a903` |

构建基于 `b8f06b1ca1bcf5407fe2a3f68c55b304d6fd9966` 加本次未提交源码，不能把原 HEAD 当成二进制完整来源；实际源码摘要记录在 `receipt.json` 和 `source-manifest.json`。

**未测：** 开启扫描器后的实际 KSM 合并、跨进程真实 VM 共享、CPU/设备静默路径的整机收益、FUSE 压缩卸载、随机不可压缩负载、cgroup/整机净内存、恢复峰值和业务尾延迟。已有控制测试验证回执校验、提交失败及 resume 互斥，但不替代这些实验。后续真实 VM 测量应使用 B-VM-MEMORY 已登记的 `live_vm_memory.py`，在具备 KVM/FUSE、独立受限 cgroup 和来源收据的宿主执行，不沿用本报告数字填补未测条件。
