# Linux 实例内冷压缩能否真实回收 RAM 并恢复运行？

## 主要结论

**实验性 Linux x86_64/KVM 实例内冷压缩已实现，真实 VM 四条件预检全部通过：两次恢复、完整 payload、可变状态、设备 I/O、heartbeat 和退出均正确。重复内容在第一冷窗口的组计费从 377.156 降到 214.012 MiB；但启动占用、生命周期峰值和恢复成本上升，随机内容在第一冷窗口仍比关闭组多占 10.797 MiB。** 不能将启用组内部下降直接当作相对默认模式的收益。

B-COLD-RUNTIME-ENG，engineering A/B；每条件 n=1，GNU 动态链接 debug 制品，单独有效 cohort。不是生产容量、正式统计、纯 pager 因果百分比或尾延迟结论。最大实际并发为一个 VM，符合最多四 VM 限制。

## Motivation

仅压缩字节不等于释放运行 RAM。需要证明原内容在丢弃前已有可靠恢复来源，KVM 与设备的真实缺页访问能恢复，并把实例内编码对象、暂存副本、运行时和文件缓存都纳入计费。优化还可能增加启动占用和访问成本，因此必须同时展示开启/关闭组的内存水平、峰值和恢复代价。

## 实现与实验设计

### 已实现的两项能力

- 默认关闭 `[vm].cold_ram_compression` / `--vm-cold-ram-compression`；仅 Linux x86_64。它同时接入自动回收与独立的实例内压缩存储，不依赖外部 pool 服务，也不是原有 FUSE `ram_compression`。
- 内核缺页 userfaultfd 覆盖 KVM、host 和内核设备 I/O；用户为 `/dev/userfaultfd` 授权。没有修改全局 KSM 或 userfaultfd sysctl，不能以 userspace-only 模式替代。
- 私有匿名 RAM，64 KiB 块，每批最多保留 4 MiB 捕获副本。CPU/设备静止窗口捕获，窗口外编码发布，第二窗口逐字节重核，内容变化则取消发布。固定持有不可变对象后才能 `MADV_DONTNEED`。
- 独立 resolver 解码并校验长度和 SHA-256，完整 `UFFDIO_COPY` 后唤醒；原文损坏/映射失败时 fail closed，不以零页替代内容。resolver 不持有 VMM/transition/device 锁。
- `LocalColdRamStore` 使用 Fill/Zstd，拒绝 raw 及不足收益块；编码 payload 最多配置 RAM 一半、对象数最多一个/64 KiB。容量/编码拒绝保留原 RAM。引用限定实例所有者，不共享另一个实例的堆。
- pager 持有映射后，balloon free-page reporting 仍确认，但不独立丢弃 RAM，避免未登记缺页/死锁。缺页计数和 generation 用于维护暂停协调；Linux pager 维护采用有界 30 秒 aggregate pause budget，普通控制仍保留原有 3 秒预算。Linux 暂不支持外部 pool RPC。
- 严格拒绝文件/shared/COW/hugetlb、已有 KSM advice/device prepare、快照/整 VM offload 组合及未守护设备 feature。固件 raw mapping 只按 builder 固定的地址/长度/host 指针身份排除，未知 raw RAM 不获豁免。
- 当前是驱逐/refault 探测，**不是真正的读热度检测**；字节稳定不表示没人读取。恢复后的块有退避，不能承诺工作负载延迟。

### 有效 cohort

- Linux `7.2.8-200.fc44.x86_64`；KVM/FUSE 可用；userfaultfd ACL 已由用户授权。
- 四个 sequential cell：`repeated/random-unique × cold off/on`，每格一个新 VM，256 MiB/1 vCPU/64 MiB payload，无网络。
- 整个 coordinator+runner+实例内 store 在同一个 cgroup：四核 CPU quota、2 GiB memory.max、零 swap；没有专属绑核保证。
- ready → 固定 20 秒窗口 → 全数据读回和设备 I/O → 修改及可变状态检查 → 固定 35 秒窗口 → 第二次读回 → 正常退出。mutation 前先验证旧内容。
- guest 返回全部 payload SHA-256，host 独立生成期望值；设备 I/O 包含全部 64 MiB 写入、fsync 和读回。heartbeat 必须推进，协议 token 和恢复状态不能以固定 sleep 代替。
- 每次启动前 30 秒 quiet window，检测同时构建/其他 VM；样本中出现干扰即拒绝。有效四格没有检测到干扰。不能据此证明不可见进程不存在。
- on 使用匿名 RAM，启动 prefault 普通 RAM；off 使用现有 shared-file 默认路径。开关同时改变映射/backing 和驻留策略，**不是仅开启编码器的隔离 A/B**。
- on 的可信普通 RAM 映射总长为 272 MiB，两段 mapping 在 smaps 中形成完整对应 union，raw 固件独立排除；off 的文件 RAM VMA 由 runner FD device/inode 验证。配置 256 MiB 不等于实际 mapping 长度，PSS 不按比例估算。
- 全 cgroup memory.current/stat、CPU、peak、PSI/events、完整 smaps 和 live metrics 留存；latest metrics 有独立时间戳与最大 5 秒新鲜度门禁。pool 编码字节排除元数据和 scratch，主要收益指标是完整组计费。

## 实验数据和分析

### 1. 第一冷窗口：实际回收，但要与关闭组比较

单位 MiB，完整 cgroup；n=1/条件。

| 内容 | Cold | ready current | cold1 current | ready → cold1 变化 | anon ready → cold1 | file ready → cold1 |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| repeated | off | 296.980 | 285.340 | −11.641 | 30.44 → 30.20 | 260.67 → 248.84 |
| repeated | on | 377.156 | 214.012 | −163.145 | 307.04 → 143.07 | 64.71 → 65.20 |
| random-unique | off | 296.328 | 297.203 | +0.875 | 30.42 → 30.21 | 260.71 → 261.12 |
| random-unique | on | 376.688 | 308.000 | −68.688 | 306.30 → 237.07 | 64.71 → 65.20 |

变化来自未舍入数值，可能与表中端点相减相差 0.001 MiB。on 的下降主要对应 anon 减少；repeated/off 的下降主要是 file 计费，不是该组执行了冷压缩。

| 内容 | on − off：ready | on − off：cold1 | on − off：cold2 |
| --- | ---: | ---: | ---: |
| repeated | +80.176 MiB | −71.328 MiB | −32.727 MiB |
| random-unique | +80.359 MiB | +10.797 MiB | −39.887 MiB |

这是各新组单次水平差，不报告因果百分比或不确定性统计。**随机内容第一窗口没有相对关闭组的内存优势**，即使启用组自己减少约 69 MiB。启动 prefault 与不同 RAM mapping/backing 使 on 起点明显更高；恢复余量不能忽略。

### 2. 两次恢复与修改后的回收

完整组 memory.current，MiB。

| 内容 | Cold | restore1 | mutation | cold2 | restore2 |
| --- | --- | ---: | ---: | ---: | ---: |
| repeated | off | 298.320 | 300.020 | 300.969 | 301.887 |
| repeated | on | 329.441 | 319.121 | 268.242 | 270.992 |
| random-unique | off | 302.211 | 303.098 | 305.445 | 306.164 |
| random-unique | on | 303.387 | 289.004 | 265.559 | 270.789 |

第二窗口 on 的 mutation→cold2 变化为 repeated −50.879 MiB、random-unique −23.445 MiB。payload 全量读回不要求所有 OS 页重新驻留，背景 pager 也继续工作，因此恢复后不会必然回到 ready 水平。

| On 条件 | RAM-PSS ready → cold1 | 最后累计 discarded | 最后累计 restored | 最后累计 put 拒绝 |
| --- | ---: | ---: | ---: | ---: |
| repeated | 272.00 → 101.75 MiB | 266.44 MiB | 147.50 MiB | 2,116 |
| random-unique | 270.38 → 195.63 MiB | 151.13 MiB | 32.25 MiB | 4,234 |

cumulative counters 不跨采样行求和，也不是唯一物理节省量。随机 payload 拒绝压缩仍可同时回收其他 guest/OS 可压缩页；没有 payload GPA 到 host page 的专门 trace，不能说随机 payload 获得了这些压缩收益。

### 3. 峰值、CPU 与恢复成本

peak 为退出/reap 后最后捕获的整个 batch 高水位；CPU 为 restore2 时完整组累计 user+system，不是单独 codec CPU。

| 内容 | Cold | 生命周期 memory.peak | restore2 累计 CPU |
| --- | --- | ---: | ---: |
| repeated | off | 304.137 MiB | 21.849 s |
| repeated | on | 378.457 MiB | 37.065 s |
| random-unique | off | 308.164 MiB | 22.910 s |
| random-unique | on | 378.102 MiB | 33.348 s |

启用组峰值和累计 CPU 更高。不能仅凭冷窗口节省量规划可多启动多少 VM；还需保留初始 prefault、编码副本及恢复 RAM 的空间。

单位 ms，guest acknowledgment 中的实测墙钟，debug；n=1。digest 为全部 payload SHA-256；digest+I/O 还包含全量写入/fsync/read回，不是 host RPC 延迟或纯缺页时间。

| 内容 | Cold | restore1 digest | restore2 digest | restore1 digest+I/O | restore2 digest+I/O |
| --- | --- | ---: | ---: | ---: | ---: |
| repeated | off | 27.254 | 109.656 | 341.429 | 490.758 |
| repeated | on | 1657.591 | 63.066 | 3365.222 | 397.804 |
| random-unique | off | 98.012 | 108.627 | 463.006 | 472.924 |
| random-unique | on | 52.343 | 84.022 | 306.027 | 308.185 |

重复内容第一次访问有明显恢复代价；样本少、不同阶段驻留程度不同，不能把第二次更低的数值当作保证，更不能给 P95/P99。

## 正确性和验证

- 有效 cohort 4/4 accepted，coordinator exit 0；每格五个 SHA-checked acknowledgment，共 20/20（ready、restore1、mutation、restore2、exit），均校验全部 64 MiB 数据及设备 I/O。
- 两个 on 条件真实发生 discard 和 fault restore；heartbeat、mutable-state、prewrite 校验与有序退出通过。
- 全部 VM native reap，owned units inactive/not-found。所有记录到的 memory.events/.local 均为零，restore2 CPU throttled counters 为零。
- 三个底层真实 UFFD/KVM/balloon 用例通过；发布写入竞态、损坏恢复拒绝、候选与原固件身份检查、门禁测试通过。
- `just test pvisor-vm`：320 passed、6 skipped；`cargo nextest run --locked -p pvisor --lib --bins`：365 passed、6 skipped。并行 test threads=2，build jobs=4。
- Python harness：21 passed；Rust worker 协议：1 passed。GNU debug 构建通过；Linux musl product check 通过（需命令内 Zig 工具及 firmware 路径）。实测制品不是 musl/release。
- 更广的 `just test pvisor-vm pvisor` 在 unrelated `documentation_json::documented_ci_failure_and_timeout_keep_reviewable_candidates` 失败后 fail-fast：期望退出 7，实际为 sandbox setup failure 的退出 1；386 passed、1 failed，其余未运行。没有修改无关测试或宣称全套通过。

## 执行命令和来源

实际有效 cohort 从 `pvisor` 根执行：

```sh
python3 benchmark/pvisor/linux_cold_runtime.py \
  --example /home/reiase/workspace/pvisor/benchmark/.data/lcr1/build5/vm_cold_runtime \
  --build-receipt /home/reiase/workspace/pvisor/benchmark/.data/lcr1/build5/build-receipt.json \
  --rootfs /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/density-env4/rootfs \
  --firmware /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/firmware \
  --output /home/reiase/workspace/pvisor/benchmark/.data/lcr1/p5q
```

等待默认值为 20/35 秒，没有覆盖。复测使用 NEW 短路径，冻结完整 dirty sources、build receipts 和 harness，不覆盖输出。

- Binary SHA-256：`4aac8751c7bc7c528dcfdfbb2d716c11ecc297f85bd87e15ebe04c03c562c04b`。
- Source manifest SHA-256：`e22214f83604873569a63f53880dc7ef4bc06ee928a12d8d4a681acb79564d85`。
- 完整 raw reports、逐 phase smaps/cgroup、kernel metrics、quiet guards、ack/stream、命令、sources、receipts 和失败在 `/home/reiase/workspace/pvisor/benchmark/.data/lcr1/`。
- build5 为有效制品；p5q 为有效 cohort。之前 eligibility、sandbox 及干扰失败和另一个部分通过 cohort 保留，**没有拼入 p5q**。来源断言不是独立重建证明，外部 registry 源码未冻结。
- 回归日志另存 `benchmark/pvisor/.data/cold-runtime-validation-20261006/`。初次 musl check 因 firmware 路径未设置失败，配置已有 firmware 路径后的 retry 通过，失败日志保留。

## 使用及剩余边界

```toml
[vm]
cold_ram_compression = true
```

或在现有 VM 命令加 `--vm-cold-ram-compression`。设备权限由管理员授权，pVisor 只在显式启用 runner 的 Landlock allowlist 加入 `/dev/userfaultfd`，不扩大普通 VM 权限。该 ACL 不替代产品沙箱和实例存储隔离。

当前是实验性单 VM 预检；还未验证多 VM 正式重复、长期热点 thrashing、业务吞吐/尾延迟、复杂设备和生产密度。优先补充多轮及真实热/冷工作集，再评估是否值得默认启用。不能把第一次窗口的组内下降作为普适净收益。

## 独立 optimized GNU release cohort

[Linux 压缩收益与成本报告](MEMORY_COMPRESSION_REPORT.md)单独记录 GNU release（opt-level `z`、thin LTO）的四格 n=1 干净工程预检，以及 SDK whole-VM offload 的污染/未完成 cohort 资格边界。该 release cohort 不替换、合并或重新解释本页历史 `lcr1` debug 数据；第二冷窗口已经过 100% 随机 mutation，不能当作重复内容保持不变的第二次测量。
