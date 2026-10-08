# 自动冷页回收与实例内压缩：哪些收益已经验证？

> 历史范围：本报告记录 Linux live pager 实现之前的存储诊断，其“不支持/尚未交付”描述仅适用于当时源码。当前已实现实验性 Linux 实例内 pager，真实 VM 验证与收益见 [LINUX_COLD_RUNTIME_REPORT.md](LINUX_COLD_RUNTIME_REPORT.md)。以下原始批次与编码数字保留，不并入新运行态实验。

## 主要结论

**当前 Linux/KVM 上，两项产品级运行态收益均未验证：自动冷页 pager 仅接入 Apple Silicon/HVF，实例内运行态压缩后端尚未交付。可执行的压缩存储诊断验证了内容完整性与编码空间缩减，但不能替代真实 VM 回收。** 64 MiB 唯一可压缩块编码为 282,368 bytes；唯一随机块仍占 67,108,864 bytes。9/9 新进程测试和 18/18 完整恢复通过。

这是 B-COLD-STORAGE-DIAG，角色 diagnostic；不填充用户 benchmark 数字，也不与 KSM/offload 的不同批次合并。

## Motivation

要确认冷内存优化是否值得部署，必须同时验证页面可安全回收、CPU/设备访问能恢复，以及整个宿主占用确实下降。仅有编码率或可运行的池服务不能证明这些条件。此诊断区分已测的存储收益与缺失的 live pager 证据，避免误报功能交付。

## 实验设计

### 两项功能的执行资格

| 功能 | 当前 Linux 状态 | 本实验是否证明 VM 净收益 |
| --- | --- | --- |
| 运行态自动冷页回收/压缩 | `cold_ram_faults=false`，`start_cold_pager` 返回 Unsupported；真实 worker 仅 macOS/aarch64 | 否，未执行 live reclaim |
| 实例内运行态压缩 | 本地 pager/存储接入仍为设计方向，未交付独立完整后端 | 否，未实现功能不能计为通过 |

依据为 `crates/pvisor-vm/src/portable.rs`、`runtime_modules.rs` 和 `docs/src/zh/design/memory-optimization/compression-local.md`。产品 `vm.memory_pool` 在 Linux 返回 `vm.memory_pool requires macOS on Apple Silicon`。单独设置旧 pool 环境变量不能启用 Linux pager。

### 可执行的存储诊断

- Linux `7.2.8-200.fc44.x86_64`，debug/unoptimized binary；测量进程绑 CPU 0，未安装 cgroup 内存预算。
- 每次新进程、独占新的 CompressedPool，64 MiB 输入，1,024 个 64 KiB 块。没有 VM，没有修改全局 KSM。
- fill：非零 `0x5a` 填充，允许全部引用复用一个对象。
- patterned：重复的 0..250 字节序列加每块独有 identity；断言 1,024 个对象，编码收益不来自块去重。
- random：固定种子的 xorshift64 数据加独有 identity；断言 1,024 个对象，作为难压缩对照。
- 每种内容三次独立执行，预先固定 seed=20261006 的交替顺序，顺序保存到 receipts。无 warmup、失败重试或样本剔除。
- 每次两遍完整恢复，逐字节比较并计算 SHA-256；输入保留用于校验，因此本实验没有回收原文。
- put 时间包含内部 hashing、codec 和 dedup；restore 时间包含内部 identity 检查。生成输入、分配输出、外层全字节比较/全摘要及覆盖输出不计入这些阶段时间。

## 实验数据和分析

### 已验证：编码 payload 空间

单位 bytes，每个条件 n=3，各次空间计数相同。

| 内容 | 输入 | 按全部引用累计的编码 payload | 池中唯一编码 payload | 对象数 |
| --- | ---: | ---: | ---: | ---: |
| fill | 67,108,864 | 1,024 | 1 | 1 |
| patterned | 67,108,864 | 282,368 | 282,368 | 1,024 |
| random | 67,108,864 | 67,108,864 | 67,108,864 | 1,024 |

patterned 的编码占原文约 0.421%，缩减约 99.579%，这是同一输入的**编码字节比**，不是跨系统因果收益或实际 RAM 节省。fill 的 1 byte 是 Fill 编码再叠加对象复用，不能说整个 pool 只占 1 byte。random 使用 raw fallback，没有 payload 空间收益，仍付出 hashing 和压缩尝试成本。

所有编码数排除对象/index、allocator、输入/输出、scratch、服务和 VM backing。输入未释放；本实验不能得出净物理内存收益，尤其不能将 64 MiB 减编码字节直接当作已回收内存。

### 已验证：恢复完整性与底层成本

单位 seconds，debug build，三次执行的观察范围，无生产延迟或尾延迟结论。

| 内容 | put 墙钟范围 | restore1 墙钟范围 | restore2 墙钟范围 |
| --- | ---: | ---: | ---: |
| fill | 0.910–0.918 | 0.743–0.746 | 0.729–0.744 |
| patterned | 1.474–1.505 | 0.744–0.757 | 0.735–0.751 |
| random | 1.492–1.622 | 0.740–0.745 | 0.732–0.737 |

- 9/9 次输入均通过；18/18 次恢复均通过全字节与 SHA-256 校验，每次恢复 64 MiB。
- 总完整恢复字节 1,207,959,552。两次恢复的是相同对象，不是两次 pager 淘汰/缺页周期。
- getrusage user+system CPU 数据单独保存在 raw JSON；本实验没有 ksmd 或 guest CPU 成本。
- 实际私有编码 payload 无公共读取接口，其直接 SHA-256 字段保留为 null；另有按冻结 codec 重构的编码 manifest 摘要，不冒充直接读取内部 payload 的证据。

采样后验证：两个 example integrity tests 和 14 个定向 portable-cold/resident/IPC/config tests 通过，rustfmt 检查通过。Linux 无副作用拒绝测试覆盖 metrics off/on、零 store RPC、store 正常析构、reservation 未改变、quiescence/fault callback 不运行。测试通过不表示未接入的 pager 已可用。

### 执行命令和来源

构建：

```sh
CARGO_BUILD_JOBS=4 cargo build --locked -p pvisor --example cold_storage_probe
```

实际来源冻结、构建与九次顺序执行由保留的 coordinator 完成：

```sh
python3 benchmark/pvisor/.data/cold-storage-20261006/coordinator.py prepare
python3 benchmark/pvisor/.data/cold-storage-20261006/coordinator.py build
python3 benchmark/pvisor/.data/cold-storage-20261006/coordinator.py measure
```

单次 frozen binary 执行形态：

```sh
taskset -c 0 /home/reiase/workspace/worktrees/pvisor/stocky-osprey/pvisor/benchmark/pvisor/.data/cold-storage-20261006/cold_storage_probe --pattern patterned --trial 1
```

完整顺序、所有命令、原始 JSONL、stdout/stderr、source archive、compiler/build receipts 和验证日志保存在 `benchmark/pvisor/.data/cold-storage-20261006/`；复测需新目录，不能覆盖原证据。首次编译的 sha2 hex 格式问题已修复，失败源码/日志保留在 `failed-build-01/`，没有测量失败。

- Frozen binary SHA-256：`83933d6f549632737b7696b0e7102ea25bb2ae04e2996c52768eee723e54efa3`。
- Raw JSONL SHA-256：`553eb53590060eace7dc6c38fc31e64bc43e1d24ca71f6285df5344d02ea6f53`。
- Source manifest SHA-256：`6f504fd8b3bd835445d2171662a74b72bd88f7dd8955ec9b827371696605c07d`。
- rustc 1.98.1；HEAD `99e086f5` 加未提交源码，不能把制品归因于 clean HEAD。

### 真实运行态收益仍需验证

1. 自动 pager：需要 Apple Silicon/HVF；测试 pool off/on、持续活动热区与冷区、重复/独有随机内容、真实回收计数及 CPU/device 访问恢复，至少两周期，全内容和变更隔离检查。
2. 实例内路径：先实现独立 ColdRamStore 适配与 pager 接入，证明对象已可靠持有后才回收；不能把独占 CompressedPool 诊断称作该后端已交付。
3. 收益指标：整个 runner+pool/backing 的物理占用、临时峰值、编码 CPU、首次恢复和业务延迟；预留恢复余量。最多四 VM，成功退出/清理也必须通过。

**这两项真实 VM 收益的最终状态是未测/阻塞，不是零收益，也不是功能测试通过。**
