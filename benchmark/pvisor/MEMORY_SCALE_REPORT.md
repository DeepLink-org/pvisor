# 最多四 VM 的内存去重和卸载：预检结果

## 主要结论

**共同 inode 基线共享、独立 COW 写入、单实例退出和 raw 两周期卸载/恢复已在真实 1/2/4 VM 中通过预检。KSM advice 接入正确，但宿主扫描器始终关闭，实际 KSM 合并为零。四 VM 压缩卸载两周期的摘要和恢复进度检查通过，完整生命周期仍因退出阶段达到期限而未通过。**

这不是五轮正式 benchmark：每格只有一个新组，组内 VM、阶段和卸载 cycle 不是独立统计样本。所有数字限于冻结 debug 制品、固定预算及本批条件，不报告 P95/P99、统计显著性、生产密度或整机净节省。不同批次分别展示，不拼接样本。

## Motivation

内存去重设计优先共享不可变 RAM 基线，再由 KSM 发现私有匿名重复页；写入和退出必须保持独立。扩面预检确认这些机制在最多四个真实 VM 下是否成立，并把 RAM-VMA 的共享观察与包含 backing/cache 的完整 cgroup 计费区分开。

实验方案：[MEMORY_SCALE_PLAN.md](MEMORY_SCALE_PLAN.md)。注册 ID 为 B-MEMORY-SCALE，角色 engineering A/B；结果不填入用户 benchmark 页面。

## 实验设定与完成范围

- Linux `7.2.8-200.fc44.x86_64`，Btrfs disk backing，可用 KVM/FUSE。
- 每 VM 256 MiB RAM、1 vCPU、64 MiB 完整 SHA-256 校验 payload；显式无网络。
- 完整组：四核 CPU quota、2 GiB memory.max、零 swap；不是四个专属绑核。
- 同时最多 4 VM。restored 模式生产者已 suspend/reap 后才启动恢复组，不存在第五个并发 VM。
- 基线模式同一 checkpoint 恢复；逐 runner 用 FD 200 与 smaps 的 device/inode 对齐，核实共同 RAM inode，而非仅比较路径或摘要。
- `ksm` 模式额外写遍全部 payload，生成动态私有 COW 页；advice off/on 对照，固定 2 秒观察窗。
- raw/compressed 采用独立 fresh-live 分组、advice off，各两次 offload/resume。全部 VM 在共同停驻 barrier 取样，500 ms settle。
- 完整组 memory.current、anon/file/kernel、CPU、PSI、memory.peak、events 和每 runner 原始 smaps 留存。没有连续监控器；peak 是整个 batch 累计高水位，不是阶段峰值。
- 宿主始终 `KSM run=0`、`full_scans=0`、`pages_shared=pages_sharing=0`。没有修改全局扫描配置。

| 独立批次 | 条件 | 通过 | 其他结果 |
| --- | --- | ---: | --- |
| `s4r` | 1/2/4 VM；repeated；baseline/ksm advice off/on，raw off | 15/15 | 无失败、无未执行格 |
| `s4s` | 4 VM；random-shared/random-unique；baseline/ksm advice off/on | 7/8 | 最后一格用户中断，未产生 ready 或后续阶段测量 |
| `s4q` | 包含 4 VM compressed 预检 | 分开分析 | compressed 达到 150 秒实验期限；不是有效成功样本 |

`s4s` 原协调器报告尚记录最后一格 `attempting`，其计数不是最终状态。独立 `interruption-audit.json` 记录用户中断、无最终结果及 owned unit 已 inactive/not-found。原始报告未改写；该格既不算成功，也不算产品失败。

## 实验数据与分析

### 1. 基线共享随 VM 数扩展，COW 写入使共享退化

下表取 `s4r` baseline、advice off 的单次观察；单位 MiB。PSS 仅累加 device/inode 与 RAM backing 对应的 VMA，不包括共享库或协调器。

| VM 数 | ready RAM-PSS 合计 | 25% mutation 后 RAM-PSS | 100% mutation 后 RAM-PSS | ready 完整组 memory.current | 100% 后完整组 memory.current |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 84.609 | 85.207 | 85.266 | 545.766 | 608.148 |
| 2 | 99.324 | 114.820 | 159.898 | 561.738 | 688.426 |
| 4 | 109.917 | 157.850 | 299.037 | 591.715 | 849.355 |

全部 restored 组都核实各 runner 映射共同 RAM inode，每 runner 对应 RAM VMA 长度合计 297,533,440 bytes。这里的配置 RAM 与包含内核布局的实际 mapping/backing 长度不是同一个数。

ready 的 RAM-PSS 增长明显不是按 VM 数线性复制；但 ready 并不是整个 VM 无私有页。四 VM 写入全部 payload 后，RAM-PSS 从 109.917 增至 299.037 MiB，符合共享被写入拆分的观察。完整组仍有 snapshot/capture/restore 页缓存和其他计费，不能把 109.917 MiB 当作四 VM 的总宿主占用。

单 VM 的 PSS 在写入前后近似不变，不表示没有 COW：其 RAM Private_Dirty 从 9.996 增至 70.203 MiB，而旧基线 page cache 仍可能在 cgroup 内计费。PSS、private dirty 和完整组计费需要共同解释。

`s4r` advice-on 对应 RAM-PSS 的 ready/cow25/cow100 单次值分别为：n=1 `84.582/85.105/85.281`，n=2 `99.078/114.605/159.727`，n=4 `110.262/158.172/299.627` MiB。`mg` 只在 advice-on 的 RAM VMA 出现；所有 RAM KSM 字段均为 0。这些微小差异不能当作 KSM 收益或回归。

### 2. 相同随机内容与实例独有随机内容区分了共享资格

下表仅取 `s4s` 四 VM、baseline 模式的完整格，单位 MiB；每格一个观察。

| 内容 | Advice | ready RAM-PSS | cow25 RAM-PSS | cow100 RAM-PSS | ready 完整组 memory.current |
| --- | --- | ---: | ---: | ---: | ---: |
| 相同随机内容 | off | 110.053 | 158.307 | 299.520 | 596.887 |
| 相同随机内容 | on | 109.892 | 158.072 | 299.532 | 592.078 |
| 实例独有随机内容 | off | 299.131 | 299.143 | 299.474 | 850.258 |
| 实例独有随机内容 | on | 299.585 | 299.585 | 299.935 | 846.086 |

相同随机内容和重复内容都能利用共同基线；共享不依赖“页很容易压缩”。`random-unique` 的 prepare 在 ready 前已经把全部 64 MiB payload 写成实例独有内容，所以 ready 已私有化，不能把它当作 0% COW 的共同 payload 对照。后续指定 mutation 主要是重写已私有页面。

所有这七个完成格的 RAM-VMA 身份都与 FD200 对应，各四 VM 共享同一 backing；producer shared mapping 建议被跳过，advice-on 恢复实例的三段私有 RAM 建议均接受。接受长度 297,533,440 bytes/runner，不是节省或合并字节。

### 3. 动态私有重复页已覆盖，但没有实际 KSM 合并

`s4r` repeated、ksm、advice on，实际动态窗口 before/after 的 RAM-PSS 为：n=1 `84.859 → 85.262`，n=2 `159.586 → 159.777`，n=4 `298.249 → 298.276` MiB。RAM KSM 均为 0。

`s4s` 四 VM 相同随机内容、ksm、advice on 的两个动态 barrier RAM-PSS 为 `298.996 → 299.492` MiB，KSM 同样为 0。两表分别使用动态窗口快照与命名 barrier，不能混作同一测量时刻。

动态写遍使 payload 成为私有匿名候选，但 scanner 停止，不会仅因 advice 合并。**目前只能确认建议安装、重复私有页工作负载和完整性，无法确认开启扫描后的合并率、扫描 CPU、收敛时间或密度收益。** 最后一格 `random-unique/ksm/advice on` 被中断，尚无阶段数据。

### 4. raw 卸载在 1/2/4 VM 中两周期均通过

下表取 `s4r` raw，单位 MiB，完整组 memory.current，advice off。

| VM 数 | ready | offloaded0 | resumed0 | offloaded1 | resumed1 | 生命周期 peak |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 172.305 | 37.883 | 169.391 | 39.293 | 168.137 | 174.180 |
| 2 | 326.098 | 57.582 | 317.402 | 59.695 | 318.414 | 329.098 |
| 4 | 634.031 | 95.129 | 612.676 | 100.523 | 618.512 | 638.531 |

raw 四 VM 的目标组计费从 ready 634.031 降到 offloaded0 95.129 MiB，读回后回升；不能把这当作整机净减少或保证下次也是该值。

| VM 数 | 两周期内每实例 offload 范围（ms） | resume＋完整摘要＋heartbeat 校验范围（ms） |
| ---: | ---: | ---: |
| 1 | 15–21 | 135–145 |
| 2 | 16–19 | 134–190 |
| 4 | 16–20 | 134–168 |

这是各组内实例/cycle 的观测范围，不是多轮延迟分布。14 份 offload 收据的 sampled resident_after 都为 0，全部完整 payload、mutable scratch、heartbeat 恢复检查通过；完整组仍有 VMM/helper/缓存等内存。

### 5. 压缩卸载存在尚未完成的退出问题

`s4q` 四 VM compressed 的阶段数据必须与 `s4r/s4s` 分开看；单位 MiB。

| 阶段 | Elapsed（s） | 完整组 memory.current |
| --- | ---: | ---: |
| ready | 8.527 | 1036.930 |
| offloaded0 | 28.052 | 105.871 |
| resumed0 | 38.645 | 530.375 |
| offloaded1 | 48.922 | 111.625 |
| resumed1 | 59.492 | 541.824 |
| 期限触达、清理后 | 150.264 | 118.215 |

两周期八个 offload 收据、八个 resume full-digest/heartbeat 检查均通过。最后成功检查是实例 4 的 resume 进度；随后代码进入实例 1 的取消/回收流程，但没有持久化 `one_vm_cancelled_and_reaped` 或 `after_exit`。150 秒实验期限触达后 cleanup 记录全部 reap。

生命周期 peak 为 1040.930 MiB；所有留存 memory events（含 max/oom/oom_kill）为 0，memory PSI 总量为 0。因此不能直接归因于 2 GiB OOM。现有证据也不能确定退出停滞的根因；需要单列 cancellation/reap 诊断，不能据此宣称压缩组完整通过或发布压缩容量结论。

## 正确性、命令与证据

`s4r`：521/521 checks、78/78 phase barriers 通过；28 个 restored private offload 拒绝后完整读回，56 个 COW 摘要与 56 个 peer-isolation 检查，20 个 survivor/full-exit 摘要均通过。所有 owned unit quiescent、全部 VM reap、无 phase inventory 错误。n=1 退出后无 survivor，不把空组当作幸存者证据。

实际运行的完整重复内容预检命令，从当前 `pvisor` 仓库根执行：

```sh
python3 benchmark/pvisor/memory_scale.py \
  --example target/debug/examples/vm_memory_scale \
  --build-receipt benchmark/pvisor/.data/memory-scale-build5-20261006/build-receipt.json \
  --rootfs /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/density-env4/rootfs \
  --firmware /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/firmware \
  --output /home/reiase/workspace/pvisor/benchmark/.data/s4r \
  --concurrencies 1,2,4 --modes baseline,ksm,raw \
  --patterns repeated --dedup off,on --preflight
```

随机批次使用同一二进制、收据和输入，output `s4s`，`--concurrencies 4 --modes baseline,ksm --patterns random-shared,random-unique --dedup off,on --preflight`。压缩失败批次为 `s4q`，完整命令保留在 report arguments 和每 attempt 的 command 中。所有输出必须新建，复测不覆盖这些目录。

本地原始目录为 `/home/reiase/workspace/pvisor/benchmark/.data/{s4r,s4s,s4q}/`，包含 report、逐格 raw、完整 stdout/stderr、smaps、输入/源码/制品清单；`.data/` 不进入 Git。历史 harness 问题和早期失败批次 `s4a/s4b/s4c/s4p` 保留，不与修复后的数据合并。

- 实际冻结二进制 SHA-256：`56311da9e4df691c7145597efa6b297e47a0dd3f1098e8e0cfc5e771e747a048`。
- 构建源码 manifest SHA-256：`bbd923a30b9f920b44e2eea3cf3f592863dbae1392649bccf4c62f54784091ff`。
- HEAD 为 `99e086f50568e135b40c3d717afd2f932890747b` 加本次实验工具未提交源码；HEAD 单独不是完整制品来源。构建前后核对源码摘要，receipt 匹配制品；harness 当前源码 manifest 与 build-time manifest 分开保留，不混淆。

**下一步：** 优先诊断 compressed cancellation/reap；如需真实 KSM 收益，由管理员预先配置扫描开启的受控宿主，再执行独立 cohort。还未完成独立 inode 基线控制、全部 54 格预检和五轮 270 batches 正式计划；现有单格观察不能替代这些验证。
