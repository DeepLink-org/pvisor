# 最多四个 VM 的内存共享、COW 与卸载恢复：工程 A/B 协议

## 问题、范围与交付

**在固定四核、2 GiB、零 swap 的完整组预算内，1/2/4 个 VM 的不可变基线共享、动态 KSM 与 fresh-live raw/compressed offload 分别如何影响物理内存和恢复成本，且是否保持完整数据、私有写入和独立退出？** 预期交付是按条件、轮次与阶段分解的工程对照及证据缺口，不是生产密度结论。

本协议服务 [B-MEMORY-SCALE 注册条目](../README.md#b-memory-scale)，角色为 engineering A/B。入口是 [memory_scale.py](memory_scale.py) 与 SDK [vm_memory_scale example](../../crates/pvisor/examples/vm_memory_scale.rs)。协议和后续父协调者报告留在 `benchmark/pvisor/`；原始证据留在本地 `.data/`，不填入用户 benchmark 页面。本文是计划，**没有测量结果**；路径、矩阵数量和实现行为不是实验成功记录。

当前默认 example 是 debug 构建。计时仅用于该冻结 debug 制品的工程观察，不能当作 release/生产延迟，也不能与其他构建批次拼接。未来源码版本 A/B 必须分别冻结并构建，只改变目标实现；本协议的模式/advice 对照本身不是“新版本胜过旧版本”的证据。

## 固定矩阵与运行单位

| 维度 | 固定取值或约束 |
|---|---|
| 同时 VM 数 `n` | `1,2,4`；含准备生产者的实际存活上限为 4 |
| restored 模式 | `baseline,ksm`，每格从一个新生产者的同一份 raw checkpoint 恢复 |
| fresh-live 模式 | `raw,compressed`，各自创建新 VM，不复用 restored COW RAM |
| payload | `repeated,random-shared,random-unique`，确定性算法和 seed |
| restored advice | `--dedup off,on`；两种 restored 模式都保留此对照 |
| fresh-live advice | 始终 off；不是再乘以两种 advice |
| VM 配置 | 每 VM 256 MiB RAM、1 vCPU、64 MiB 完整触碰/校验的 payload |
| 完整组预算 | `cpu.max` 四核配额、`memory.max=2147483648`、swap max/current 为 0、TasksMax 128 |
| 时间窗 | `--settle-ms 500`；`--ksm-wait-seconds 2`，固定并记录，不按结果调整 |
| 轮次 | 预检 1 轮、0 warmup；正式 5 轮、0 warmup，独立输出目录 |

**每轮精确 54 格：** `3 n × (2 restore × 3 patterns × 2 advice + 2 offload × 3 patterns) = 54`。其中 restored 36 格、fresh-live 18 格。正式五轮计划 **270 个 batch**，每格五个独立新组；预检另计划 54 个 batch，首个失败即停止，剩余格标未测。失败可能使实际完成数小于计划数，不得把计划数写成有效样本数。

一格的一次 batch 是统计单位。组内 VM、COW 阶段和两次 offload cycle 都是相关重复观测，不能扩充成独立样本。每轮以 seed 打乱 54 格顺序，每次只运行一个 batch，上一 UUID-owned systemd unit 已确认静止后才运行下一格。不并行启动预检、正式、其他 cohort 或构建。

生产者先启动、完整校验、suspend，再取得终态 reaping 收据并移除其 fresh-live backing 名称，**之后**才启动最多四个恢复实例。生产者曾经存在不等于第五个并发 VM；反之，仅声明 `max_live_vms=4` 不足以证明上限，应核对生产者结束和恢复实例开始的时间戳、Run/Attempt 身份及 cleanup。Python `--concurrencies 5` 和 example `--vms 5` 均必须被参数门禁拒绝；不能绕过协调器扩大并发。

四核是完整 cgroup 的 CPU quota，不是自动绑定四个专属物理核。当前脚本记录宿主允许的 affinity，没有专用 affinity 参数；保留实际 affinity、宿主负载和控制文件，不能声称未设置的 cpuset 隔离。预算覆盖内层协调器、runner、VMM/FUSE/helper、backing 和本组计费的页缓存；外层编排及预备输入缓存可能在父 cgroup 计费，不能由组内差值推导整机净节省。

## 机制对照及必要证据

### 基线共享不是内容摘要共享

- `baseline` 的 `ready` 是 0% **指定 COW 写入**阶段，不表示整个 VM 无任何私有页或控制状态写入。`repeated` 和 `random-shared` 保持 producer payload；恢复后的完整原始摘要必须先通过，再进行实例 prepare。
- `random-unique` 在 prepare 时把全部 payload 写成实例独有内容，因此 `ready` 已有全 payload 私有化，不能冒充“尚未 COW 的共享随机基线”。其后 25/100% 是指定 mutation 比例，不是新发生的物理 COW 比例。
- 同一 `snapshot_id`、路径或 SHA-256 **不能证明映射同一 inode**。报告共享结论前，逐 runner 查原始 `/proc/PID/smaps` 的 RAM VMA 地址范围、权限、device/inode，并与 `large_file_fds` 的 `dev,inode,bytes,target` 对齐；将 checkpoint backing 身份、runner PID/Run/Attempt 和相应 phase 关联。FD 的十进制 device 与 smaps 的 major/minor 要正规化后比较。
- 需要证明对应 RAM 范围为同一 backing inode 的 private 映射，并结合完整触碰、该 VMA 的 PSS/shared/private 数据描述实际驻留。不能仅以“所有 runner 都有某个大 FD”替代 VMA 关联，也不能把 Python/共享库的 PSS 当 RAM 共享。映射可能关闭 FD，此时 smaps 身份仍有价值；读取权限不足或关联不明明确记为缺失，不补零。
- 当前协调器保存 smaps 和大文件 FD 身份，但**不自动验证共同 inode 或逐 VMA advice**。其 `successful` 只表示当前自动门禁通过；父协调者必须另做上述证据审查，未证实就不得写共同 inode 共享结论。
- 注册条目要求的“相同内容、不同 inode”独立基线对照是**计划项、当前不支持/未测**。example 的 `--private-baselines true` 显式拒绝；不得以 clone checkpoint 引用、换 snapshot ID、复制路径或同摘要文件伪造独立 capture。后续实现需要独立捕获并核对不同 `(dev,inode)`、内容一致和缓存条件，另立 cohort，不加入当前 54 格。
- 相同内容而不同 inode 不会自动产生文件页共享。共同 inode 基线共享与 KSM 合并匿名/私有页是两条机制，不能互相替代证据。

### 动态 KSM 与扫描宿主分组

`ksm` 模式在 ready 后执行 `duplicate`，即便字节相同也写遍全部 payload，得到动态 private 页；采样 `dynamic_private_before_wait`，等待固定扫描窗，完整读回，再采样 `dynamic_private_after_wait`。前后都有完整组 barrier；扫描窗中的 before/after accounting 是运行中的快照，不等同于暂停 barrier，也不是 scanner 的原子观测。

| cohort | 宿主 KSM 条件 | 可解释范围 |
|---|---|---|
| 扫描关闭 | 管理员已设置 `run=0`，全批保持不变 | advice 安装、共同 inode/COW 与无扫描对照；不声称实际 KSM 合并 |
| 管理员预启用 | 受控宿主预先 `run=1`，冻结扫描配置 | 在固定窗内观察到的每 runner/VMA KSM 与内存变化 |
| 未知或其他状态 | sysfs 不可读、`run=2` 或配置变化 | 保留观测/缺口，不当作启用扫描或关闭扫描的替代 |

脚本对 KSM sysfs **只读，绝不更改全局设置**。不开启、关闭、调速或清理宿主 KSM；需要扫描 cohort 时由管理员在测量前另行配置。两 cohort 分目录、分别做 1 轮预检和 5 轮正式；每 cohort 的正式计划都是 270 batch。给定 `s4p/s4f` 只用于当前一个已核实的 cohort，第二 cohort 必须选新的短路径，不能覆盖或合并。跨宿主配置 cohort 只能并列展示，不能作为同批随机配对百分比收益。

保留 `run,pages_to_scan,sleep_millisecs,full_scans` 和全局 pages counters 的前后值及窗口。**实际 KSM 证据以每进程 smaps 为准，并尽量定位 RAM VMA**；advice 请求/`VmFlags` mergeable 标记仅证明登记，不证明合并。全局 `pages_shared/pages_sharing` 只作宿主扫描上下文，可能来自其他进程，不能归因到本组或换算本组节省。缺少 KSM 字段不等于零。

`repeated` 含大量相同页，单 VM 也可能内部合并；`random-shared` 是跨实例同页内容、页间不同的正对照，`n=1` 是其重要控制。`random-unique` 是跨实例 payload 合并负对照，但相同 OS/解释器/零页仍可能合并，不能要求全进程 KSM 恰为零。只在 `n≥2` 的 shared-random 条件讨论跨实例动态合并，不能将 repeated 的全部变化归因于跨 VM。

2 秒是 bounded wait，不是合并完成 deadline，也不是产品保证。窗内未观察到合并记“该窗内未观察到”，不能判为产品故障。`dynamic_private_before_wait` 在采样前已经过暂停/settle，可能已有扫描，不能叫严格零合并起点。若需更长窗，预先制定新的 cohort、固定 `--ksm-wait-seconds`（当前 0..60），重做预检，不挑选直到出现预期收益。

### offload 与去重互斥边界

`raw/compressed` 使用各自 fresh-live backing、advice off、两次完整 offload/resume cycle。fresh-live 的 `MAP_SHARED` RAM 不参与当前去重建议：应记录 skipped 状态，不能把“请求 advice”或 backing 文件变小写成共享/物理节省。压缩 backing 与自动 live cold-page pool 不是同一条件；去重与压缩/冷页 pool 互斥，不能启用二者来创造额外性能格。

协调器拒绝 offload 格 dedup on，example 显式拒绝 dedup+compressed；restored private COW 的 offload 必须因 `restored private COW RAM` 被拒绝，并完整读回证明拒绝没有损坏运行状态。当前矩阵不实际开启自动 pool；pool 互斥属于配置契约审查，不能将未执行的 pool probe 算作已通过。拒绝错误原因也要核对，不能用任意失败代替预期保护。

## 每 batch 的阶段与正确性门禁

### 同时性最低门槛

所有 n 个独立 runner 已启动且完成 fresh token/full digest 后才采样 ready。每个活跃阶段逐实例 pause，等待 300 ms 处理可能尚未落盘的 device 写入，再至少等待 100 ms（本协议 500 ms），确认整个组 heartbeat 不变才取样，之后 resume。它证明采样时共同停驻，不证明同时启动或正在同时执行有用任务；不得称为生产活跃吞吐。offloaded 阶段则要求全部 VM 已 suspended 且 heartbeat 稳定。

每阶段核对实例列表、paused/offloaded 标记、heartbeat 稳定、阶段顺序、同一 cgroup。`after_exit` 应只有 2..n；n=1 时幸存者为空，不能称“幸存者校验已覆盖单 VM”，但仍需采样空组残留和终态清理。

### restored：0 → 25 → 100% 写入 → 退出

1. 新 producer 完整 payload SHA-256、suspend 收据、唯一 Run/Attempt、reaped 终态及保存 heartbeat；producer 必须在任何恢复启动前结束。
2. n 个恢复实例先验证原始 producer 全摘要和保存状态，再 prepare 实例身份并校验 ready 全摘要。核对 backing/inode，原 checkpoint 保持不可变。
3. `ksm` 独有动态 private 写遍及 before/after bounded wait；`baseline` 无此额外写遍。两模式差异含 workload 差异，不能把其总量差直接叫纯 KSM 收益；主要 advice A/B 应在同 mode/pattern/n 内比较。
4. 私有 restored offload 拒绝及拒绝后完整读回。
5. 依次令各实例写指定前 25%、100% 页，使用实例专属 mutation 内容，每次完整摘要核对；先前实例已 dirty 时，尚未本轮改写的 peer 仍须保持 prior 摘要。采样 `cow25/cow100`，记录 PSS/KSM/private dirty 退化而非用写入比例估计节省。
6. cancel 实例 1 并 reap，幸存实例完整读回 100% 状态，采样 `after_exit`；幸存者有序退出且退出摘要正确，最终所有 owned VM reap。

### fresh-live：两次卸载与恢复

1. 新 n 个 VM，ready 校验全部 64 MiB payload、实例身份和 mutable 状态。
2. 每个 cycle 逐实例 offload，核对 RAM backing 收据覆盖至少 256 MiB 和 suspended 状态；全部完成后 `offloaded0/1` 共同 barrier。
3. 逐实例 resume，校验全部 payload、fresh token、heartbeat 前进和 mutable 状态，采样 `resumed0/1`。两个 cycle 分开保留，不平均为两个独立 batch。
4. cancel/reap 实例 1；幸存者完整读回，`after_exit`，然后有序退出和最终 cleanup。

独立 Rust oracle 计算整个 payload 的 SHA-256，不接受 guest 自报摘要作为期望值；校验覆盖未写页和已写页，不以首尾抽样替代。guest 每次循环验证 1 MiB scratch 与递增 heartbeat，host 核对 token、op、instance、percent、摘要和状态前进。完整输入清单与 checkpoint 不可变性需父协调者审查：脚本启动时冻结 rootfs/firmware 清单，不自动做所有输入的终了重哈希；后续报告若主张基线/输入未变，必须补齐前后哈希证据，没有则标未验证。不能仅由 peer checks 推导 backing 从未被写入。

pin 生命周期审查要关联生产者 reaping、恢复实例持有的 checkpoint/backing 引用、smaps/FD 和最终释放：producer 退出不应使恢复失效，cancel 一个 runner 不应释放其他 runner 所需 pin；最后一个 runner 退出后仍留在磁盘的证据不是自动泄漏。当前 `after_exit` 在 100% payload mutation 后，不能替代“仍在读取未改写基线时 owner 退出”的专门 race。进程/FD 消失、缓存计费残留及最终 `after` 分开记录，不保证页缓存立刻归零。

这些是受控同源 VM 的数据/COW/生命周期检查，**不是跨租户安全隔离证明**，也不排除 KSM 时序侧信道。

## 资源、成本和计量边界

| 证据 | 报告口径与限制 |
|---|---|
| `memory.current`、`memory.stat` anon/file/kernel | 主要物理计费指标；包含本组 backing/cache/helper，不把 file 当免费存储；这些计数并非严格同一时刻原子快照 |
| 每 runner `smaps` | 原始 VMA 身份、RSS/PSS/KSM、private/shared 字段与 advice；进程汇总仅作辅助，不能将 RSS 相加当去重后物理量 |
| advice | 请求 off/on、对应 RAM VMA mergeable/skipped、实际 KSM 分别列示；登记字节不是节省；当前没有保证完整独立 advice 状态字段，无法由 smaps 确认时标缺口 |
| `cpu.stat`、CPU/memory PSI | 保存累计值，用阶段前后差描述整组 CPU/压力；包含控制、全摘要、heartbeat、等待/采样成本，不是纯压缩或 KSM CPU |
| `memory.peak`、events、swap、pids | 每 batch 新 cgroup 的生命周期高水位、OOM/max 等事件与任务数；OOM 后不继续把数据当成功内存结果 |
| ready/offload/resume/read timing | 区分 batch elapsed、每实例 offload ACK、resume 到完整校验和 guest `read_ms`；guest read_ms 是完整 hash 扫描，不是首次 page-fault 延迟 |
| `before/after` 与阶段快照 | 记录空组/启动、所有阶段、清理后内存/CPU；对照能显示包含采样器的总开销，但不能精确扣出未单独测量的观察者成本 |

当前只有 barrier 与扫描窗快照，**没有连续监控器**。`memory.peak` 是 cgroup 自建立以来的累计高水位，含 producer/capture/restore；后续 phase 的 peak 不是该 phase 专属峰值，峰值差也不是 phase 最大增量。快照不捕获瞬时分布/峰值发生时间，不能宣称连续 50 ms 监控或精确每阶段 transient peak。需要该信息时另做预先设计的插桩 cohort，不把其计时混进当前批次。

offload 回收量按同 cycle 的 ready/resumed 与 offloaded 的完整 cgroup 计费差观察，连同 anon/file/kernel、压缩存储和 CPU 成本报告；磁盘 backing 文件大小不是 RAM 净节省。恢复顺序固定为一个实例一个实例，预算中的 recovery reserve 必须容纳已恢复实例、仍 offloaded 的 backing/cache、解压临时空间和 helpers，**不能把 parked 水位用满 2 GiB 后假定仍能恢复**。本矩阵记录恢复后的资源与生命周期 peak，只能描述观察到的余量，不能保证瞬时最坏情况 reserve；2 GiB 减去快照值不是保证可用的恢复额度。

OOM 保留原始 events、日志、失败和不完整报告，检查匿名/文件/内核计费及恢复临时开销，不增加宿主全局预算、不自动提高当前 2 GiB。预算不足就是本预算条件的失败；需要别的预算必须事先批准并单列 cohort，不拿新预算补旧格。

## 前置步骤与复现命令

所有命令从当前 `pvisor` 仓库根执行。本文只提供复用步骤，不在文档任务中构建或测量。

1. 确认 Linux x86-64、可访问 KVM/FUSE、原生执行所需 namespace 能力、cgroup v2/systemd user service 及 memory/CPU/zero-swap 控制可用。不得通过放宽 namespace/安全设置让预检假通过。
2. 确认 rootfs 与同级 firmware 已准备，输出在真实磁盘且不是 tmpfs；空间足以保存每次失败、backing、checkpoint 与日志。canonical worker 路径最多 70 bytes，使用下方短路径。
3. 构建必须在测量前结束。从冻结的实际源码（含本地修改）构建 example，保留 compiler、命令、日志、源码清单和二进制 SHA-256。现有本地收据 `benchmark/pvisor/.data/memory-scale-build-20261006/build-receipt.json` 的构建命令为：

   ```sh
   cargo build --locked -p pvisor --example vm_memory_scale
   ```

   **不要在已有收据之后盲目重建再复用旧收据。** 若重建，先冻结构建输入，重新生成匹配 receipt 与同目录 `source-manifest.json`。收据至少含 `example_sha256`、`source_manifest_sha256`、`source_identity.head`、boolean `source_identity.dirty` 和非空 `build_command`。脚本校验制品/manifest 字节，但不独立重现父协调者所述构建，当前源码快照与 build-time 来源也不能混为一谈。外部 registry 源码没有被当前 harness 完整冻结，应列明依赖来源限制。
4. 只读核对 KSM cohort 状态，记录宿主配置；不使用写 sysfs 的命令。冻结同一 binary/harness/rootfs/firmware、seed、扫描窗与预算。停止其他构建/测试/采样任务，然后才预检。

```sh
cat /sys/kernel/mm/ksm/run /sys/kernel/mm/ksm/pages_to_scan /sys/kernel/mm/ksm/sleep_millisecs

python3 benchmark/pvisor/memory_scale.py \
  --example target/debug/examples/vm_memory_scale \
  --build-receipt benchmark/pvisor/.data/memory-scale-build-20261006/build-receipt.json \
  --rootfs /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/density-env4/rootfs \
  --firmware /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/firmware \
  --output /home/reiase/workspace/pvisor/benchmark/.data/s4p \
  --concurrencies 1,2,4 --modes baseline,ksm,raw,compressed \
  --patterns repeated,random-shared,random-unique --dedup off,on \
  --seed 20261006 --settle-ms 500 --ksm-wait-seconds 2 --preflight
```

只有完整预检 54 格均通过自动门禁，且人工 inode/advice/生命周期证据审查无影响目标结论的缺口，才启动正式批次。预检不是正式 warmup，也不计入统计。输出必须 NEW；当前指定 `s4p/s4f` 不存在，但运行前仍需检查，存在则选择新的短目录，不删除历史数据。

```sh
python3 benchmark/pvisor/memory_scale.py \
  --example target/debug/examples/vm_memory_scale \
  --build-receipt benchmark/pvisor/.data/memory-scale-build-20261006/build-receipt.json \
  --rootfs /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/density-env4/rootfs \
  --firmware /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/firmware \
  --output /home/reiase/workspace/pvisor/benchmark/.data/s4f \
  --concurrencies 1,2,4 --modes baseline,ksm,raw,compressed \
  --patterns repeated,random-shared,random-unique --dedup off,on \
  --seed 20261006 --settle-ms 500 --ksm-wait-seconds 2 \
  --samples 5 --warmups 0
```

当前 worker 实验期限 150 秒、cleanup 20 秒，内层 subprocess 185 秒、service RuntimeMaxSec 200 秒、外层等待 220 秒；这些是资源与清理护栏，不是产品延迟承诺。超时保留失败/unknown 证据，缺失 raw/终态不补为成功。正式批次可继续不受影响的格，但任何 owned unit 清理无法证明就停止整个 sweep，防止剩余进程使实际并发突破四个。

## 分析、拒绝规则与父协调者报告

- 预先规则：当前没有事后异常值剔除，保留所有有效慢样本。错误摘要/状态、预算逃逸、swap、OOM、缺阶段、KSM 状态变更、缺终态/cleanup 均不计成功计时；仍计 attempted 与失败。宿主干扰记录为上下文，不以“太慢”删除；需要重跑就新建完整 cohort，不能挑格替换。
- 对每格给 planned/attempted/successful/failed/unmeasured 及各阶段数据；成功条件与机制证据充分性分开。五轮是五个独立 batch，而不是可靠性或容量保证。
- 按相同 cohort、mode、pattern、n 的轮次配对 advice off/on；raw/compressed 在同 pattern/n/round 内配对。以每轮完整组指标为单位，报告中位数差和预先固定方法/seed 的 95% paired bootstrap 区间。五对样本区间不稳定，明确仅工程探索；区间含 0 写“未检出差异”。不要由同源 checkpoint 名称推导可比内存。
- 展示全部五个点与范围；出现双峰时列簇比例和各簇中位数，不用一个 P50 掩盖。五样本不报告 P95/P99，不用单轮峰值当持续 worst-case。不同构建、日期、预算、扫描 cohort 不合并；跨 cohort 不写同批收益百分比。
- 输出父协调者审查的阶段表：完整 cgroup anon/file/kernel/current/生命周期 peak、CPU/PSI/events、每 runner RAM VMA PSS/KSM/advice/身份、COW 曲线、退出后残留、两个恢复 cycle 的摘要和状态证据。报告同时列注册需求中未覆盖的独立 inode 与 race 项。
- 保留 `report.json`、每 attempt config/result/raw、worker/service stdout/stderr、stop/final-unit 日志、完整 worker scratch/backing/checkpoint、源码/dirty patch/harness、rootfs/firmware 清单、二进制和 build/source receipts。失败预检、部分正式报告与 deadline 记录原样保留，不从旧批补格，不在冻结测量期间编译。
- 任何“共享”“回收”“可恢复”的工程陈述必须绑定 phase、cohort 和上述完整性证据；没有实际数据只写计划/未测。父协调者之后生成报告，本计划不预填改善比例、成功率或内存结果。

## 后续可选正确性扩展：当前不支持或未测

以下不属于 54 格，也不能由当前顺序协议冒充已覆盖。先实现明确注入点/拒绝原因与最终完整性门禁，另开短路径、独立 cohort 和收据，再报告：

| 可选 case | 所需新增证据 |
|---|---|
| 独立 inode、相同内容基线 | 独立 capture、不同行为前后 inode、全摘要、统一缓存条件；不是克隆同一 checkpoint |
| corrupt content | 明确损坏 checkpoint/backing 的注入时点，恢复拒绝或错误暴露，不静默接受坏数据 |
| hash mismatch | 分离来源摘要/兼容性门禁错误与运行后内容错误，保留精确拒绝收据 |
| cache owner exit / pin race | 在幸存者仍读未修改基线时让 owner 退出，验证 pin 不提前释放、最终引用可释放 |
| slow peer races | 对确定的恢复/读取/COW/offload 阶段注入慢 peer，验证其他 peer 数据、退出/取消顺序和资源上限 |
| pool 互斥/连续资源观察 | 自动 cold pool 的独立配置拒绝证据；另立插桩 cohort 观察 transient peak 与恢复 reserve |

当前 producer reaped-before-restore、顺序 peer COW 检查、cancel1 与最终清理提供有限生命周期证据，不等同这些故障/竞态注入。任何扩展也必须保持实际最多四个 VM、完整组固定预算，并且不得修改全局 KSM 或声称跨租户隔离。
