# 宿主 KSM 开启时，Firecracker 默认运行是否获得去重收益？

## 主要结论

**现机 Firecracker v1.13.1 的默认 guest RAM 未登记为可合并，四 VM 的三种内容负载在 20 秒窗口中均为 RAM-KSM=0、RAM-PSS 基本不变。宿主开启 KSM 不会自动使这些映射参与去重。** 安装版本的 API/schema/CLI 未提供 guest-memory-merging 开关，因此 advice-on 三格为 unsupported；不能据此说 Firecracker 经改造启用建议后也不能去重。

这是 B-MEMORY-SCALE 工程对照预检，单条件一个新组。pVisor 的 scanner-on 数据来自独立批次，只能并列观察，不能给出纯 VMM 胜负、跨批因果百分比或生产容量结论。

## Motivation

部署多个相似 VM 时，需要知道宿主开启 KSM 是否足够，还是还需 VMM 将 guest RAM 登记为可合并。对照默认 Firecracker 与显式 advice-on 的 pVisor，可区分“扫描器开启”与“实际参与并产生合并”，避免将全局设置当成内存收益。

## 实验设计

- 同一宿主，KSM 配置只读：`run=1`、`pages_to_scan=100`、`sleep_millisecs=20`；不改变宿主配置，也不采用 ptrace/LD_PRELOAD 注入去重建议。
- 最多四个并发 VM，每 VM 256 MiB RAM、1 vCPU；实际 worker group 为四核 CPU quota、2 GiB memory.max、零 swap。前后 VM process audit 为空，未与 pVisor VM 同时运行。
- 三种 64 MiB payload：`repeated`、`random-shared`、`random-unique`。页面生成器、种子、随机字 little-endian 编码和 mutation identity 与 `vm_memory_scale.rs` 相同。
- Firecracker PCI、独立 fresh boot；静态 Rust PID-1 guest worker 通过串口执行 ready、20 秒观察后的读回、25/100% 写入、peer 和退出后的 survivor 校验。每次回复前逐字节验证全部 payload，并返回宿主独立计算验证的 SHA-256。
- 匹配每个成功启动的 VM 唯一一段恰好 256 MiB 的匿名、私有、可写 RAM VMA；保留完整 smaps，并单列 RAM-PSS/KSM/mg，不用全进程 PSS 代替 RAM。
- 准备好的 kernel/ext4 输入、guest worker、Firecracker binary/schema、源码和摘要收据冻结到证据目录。磁盘/缓存计费可能在 worker cgroup 之外，不能将组计费解释为完整生命周期整机物理内存。
- 与 pVisor 的不同：参考内核、最小 PID-1 Rust worker、独立 boot、匿名 backing；没有共同 checkpoint inode、Python runtime、额外 scratch、恢复 heartbeat、offload 或冷回收。pVisor 匹配 RAM VMA 每实例为 283.75 MiB，而这里是 256 MiB。
- 正确性、OOM、VMA 歧义和清理失败不得作为有效结果；不支持的条件不计为通过。每格一个样本，不报告置信区间或尾延迟。

## 实验数据和分析

### Firecracker 默认运行：没有实际合并

单位 MiB，四 VM RAM VMA 累加，单格 n=1；ready/observed 包围固定 20 秒等待，另含读回工作。

| 内容 | RAM-PSS ready → observed | PSS 变化 | observed RAM-KSM | RAM mg |
| --- | ---: | ---: | ---: | --- |
| repeated | 348.262 → 348.371 | +0.109 | 0 | 无 |
| random-shared | 348.266 → 348.375 | +0.109 | 0 | 无 |
| random-unique | 348.266 → 348.375 | +0.109 | 0 | 无 |

变化量来自未舍入数据。写入 25% 和 100% 后 RAM-PSS 仍为对应 observed 水平，RAM-KSM 仍为零；这里没有已合并页可供拆分，不是已启用 KSM 的 COW 成本测量。

### 与 pVisor 的独立批次并列

单位 MiB，四 VM；pVisor 来源为 [KSM_EFFECT_REPORT.md](KSM_EFFECT_REPORT.md) 的独立 `s4k` cohort，Firecracker 为 `f4k/run1`。观察 barrier、RAM VMA 大小、内核/runtime/backing 不同，不能据此计算性能排名。

| 内容 | pVisor advice-off PSS 变化 | pVisor advice-on PSS 变化 | pVisor on 窗口后 RAM-KSM | Firecracker 默认 PSS 变化 | Firecracker RAM-KSM |
| --- | ---: | ---: | ---: | ---: | ---: |
| repeated | +0.637 | −156.927 | 161.977 | +0.109 | 0 |
| random-shared | +0.476 | −138.020 | 204.434 | +0.109 | 0 |
| random-unique | +0.190 | −0.066 | 1.434 | +0.109 | 0 |

这些数据支持“pVisor 显式登记后观察到正对照合并；现机默认 Firecracker 不参与”的结论。KSM 字段跨映射累计，不是唯一物理占用或直接节省量。两者 RAM 都包含 OS/runtime，不能将全部 KSM 归因于 payload。pVisor 20 秒时尚未充分收敛。

### Firecracker 组计费和正确性

单位 MiB，完整受限 worker cgroup，n=1；observed peak 是采样捕获的 memory.peak，不是包含准备阶段的整机生命周期峰值。

| 内容 | ready | observed | mutated25 | mutated100 | observed peak |
| --- | ---: | ---: | ---: | ---: | ---: |
| repeated | 365.523 | 365.863 | 365.957 | 366.414 | 366.469 |
| random-shared | 365.859 | 365.680 | 366.945 | 366.453 | 367.195 |
| random-unique | 366.898 | 366.797 | 367.047 | 368.023 | 368.273 |

组计费不与 pVisor checkpoint/runtime 组直接相减。random-shared/unique 的微小计费下降没有相应 RAM-PSS 下降，不是去重收益。

- 三个 advice-off 组通过，每组 58 个全 payload SHA-256 acknowledgement，共 174 个，guest 另有逐字节检查；peer 写入隔离和退出后的 survivor 校验通过。
- advice-on 三格 unsupported，计划六格只有三格可执行，runner 返回 exit code 1，**不能报告六格全通过**。
- 全部 12 个 guest VM 进程已 kill/reap，owned systemd units quiescent；没有记录到 OOM 或 CPU throttling。
- 全局 `full_scans=358`、`pages_shared=0`、`pages_sharing=0` 在该批次采样中不变。只有 run=1，不表示非 mergeable RAM 被扫描。
- 窗口及读回 group CPU：repeated 0.655222 s、random-shared 0.678552 s、random-unique 0.685397 s；不包含宿主 ksmd，不能报告 scanner CPU 效率。

### 去重开关的支持情况

安装的 Firecracker v1.13.1 help 与冻结 API schema 没有 guest-memory-merging 接口。实际 `memory_mergeable` machine-config probe 返回 HTTP 400：

```text
unknown field `memory_mergeable`, expected one of `vcpu_count`,
`mem_size_mib`, `smt`, `cpu_template`, `track_dirty_pages`, `huge_pages`
```

这不是声称所有版本或外部集成都不能启用 KSM。对等 advice-on 实验仍未测；需另行明确选择增加 guest RAM `MADV_MERGEABLE` 的实验构建或受控启动机制，作为独立 cohort 验证后再比较。

### 执行命令、来源与失败保留

从 `pvisor` 仓库根执行：

```sh
python3 -m unittest discover -s benchmark/pvisor -p test_firecracker_ksm.py -v
python3 -m py_compile benchmark/pvisor/firecracker_ksm.py benchmark/pvisor/test_firecracker_ksm.py

python3 benchmark/pvisor/firecracker_ksm.py \
  --assets /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/assets \
  --output /home/reiase/workspace/pvisor/benchmark/.data/f4k

python3 benchmark/pvisor/firecracker_ksm.py \
  --assets /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/assets \
  --output /home/reiase/workspace/pvisor/benchmark/.data/f4k/run1
```

第一次因 coordinator HTTP client 等待 persistent connection EOF 失败，没有启动 guest；API process 已 reap。修正为按 Content-Length 读取并加入回归测试后，第二个独立目录保留上述结果。失败未删除、未并入成功样本。目标 Python 测试 11/11 通过，静态 musl worker 编译/ELF 验证、SHA-256 self-test 及三种 native worker 检查通过；三种模式随后在实际 guest 中完成校验。

证据保存在 `/home/reiase/workspace/pvisor/benchmark/.data/f4k/`：`execution-notes.json`、`run1/report.json`、`run1/phases.csv`、`run1/audit-summary.json`、冻结源码/制品/收据、完整 smaps、serial logs、配置及清理记录。收据摘要已核对；安装 Firecracker binary/schema 冻结不等于源码重建证明。`.data/` 不进入 Git；复测必须换 NEW 输出目录。
