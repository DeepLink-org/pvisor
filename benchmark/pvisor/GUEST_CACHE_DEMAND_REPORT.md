# 按需加载能消除目录预取退化吗？

后续有界自适应预取及三组同批对照见 [GUEST_CACHE_ADAPTIVE_REPORT.md](GUEST_CACHE_ADAPTIVE_REPORT.md)。本页保留仅枚举加载版本的原始取舍。

## 主要结论

**单文件 lookup 不再装入整目录，并发 lookup 不消费装载中的未完成目录项；不支持目录镜像的挂载也不再逐目录重试。目录扫描缓存和惰性 open 保留。代价是从不枚举目录的逐路径 stat/read 首次访问失去预取收益，不能将这项修复称为所有负载都更快。**

## Motivation

区分 Agent 扫描目录与只访问少量已知路径的需要，避免用整目录装载放大稀疏访问成本，同时检查惰性 open 和扫描缓存是否仍然有效。

## 实验设计

- B-FS-ENG；相同冻结 CLI、SDK driver、guest workload 和原始只读输入，只替换旧固件 `561b4ccf` 与含补丁 0044 的新固件 `d55fb807`。两臂 guest cache 均为 1，writeback 均为 0；内核实际配置摘要相同，没有修改宿主缓存策略或 lower 资格。
- macOS Apple Silicon/HVF，2 vCPU、4096 MiB；宿主热缓存，新 guest/upper。普通 `--safe` CLI 与显式 Immutable lower 的 SDK share 分开统计；保留现有 Host listener。没有宿主 CPU affinity/cgroup 或无外界干扰的保证。
- 每臂 3 次预热、30 次正式样本，每轮四条件 seeded shuffle（20261017），不剔除慢有效样本；配对 bootstrap 5000 次，先检查分布；不报告 P99。95% CI 包含 0 时判为未检出差异，没有多指标校正。
- 负载与旧开关实验一致：2048 文件、深路径、32 目录枚举、64 MiB 真实读取、1000 次 open/fstat 与 open/read、2048 项宽目录单文件访问、空闲 1.3s 后访问。stat 与 read 组不预先枚举；scan 组明确枚举。全部内容、元数据、正常退出、CLI Bundle 与输入/制品摘要检查通过。
- 重复 stat 保留原生 1s TTL；未枚举组的新固件也回到原生 TTL，旧固件的目录镜像属性有效期为 24h。过期组差异包含有效期策略变化。热 open/fstat 的 prime 在计时外。
- 输入使用原始规范路径，未改变内容、权限或链接关系。失败的准备预检（符号链接 lower 根、硬链接与 --safe 冲突）独立保留，不进入正式数据；最终四条件预检通过。
- B-FS-DIAG 另跑 40 个 VM：既有 9 个 prime-only 模式和完整 scan 模式；其耗时不混入正式样本。所有 profile instance 均有 final_record，并执行请求数断言。含导入引用保护之前的中间计时批次保留在 `.data/guest-cache-demand-formal-20261016/`，没有拼入最终数据。

## 实验数据和分析

单位 ms；每臂 n=30，普通数值为中位数。若分布分离，则列各簇样本比例和中位数。CI 为新固件减旧固件。

### 普通 --safe CLI

| 负载 | 旧固件 | 新固件 | 差异 95% CI | 判断 |
|---|---:|---:|---|---|
| 2048 路径首次 stat | 134.947792 | 129.593896 | [-14.862896, 1.096874] | 未检出差异 |
| 立即重复 stat | 0.782604 | 0.758937 | [-0.061541, 0.01727] | 未检出差异 |
| 深目录首次 stat | 229.261625 | 207.886354 | [-30.113896, -9.349771] | 新固件更快 |
| 深目录重复 stat | 1.071708 | 1.060188 | [-0.030854, 0.009125] | 未检出差异 |
| 首次枚举及 stat | 84.122292 | 81.857604 | [-6.0405, 1.652874] | 未检出差异 |
| 重复枚举及 stat | 68.669125 | 69.418188 | [-1.306438, 2.995708] | 未检出差异 |
| 首次读取 64 MiB | 665.553771 | 668.365438 | [-20.719896, 68.963833] | 未检出差异 |
| 重复读取 64 MiB | 504.938833 | 504.428167 | [-6.091813, 7.417583] | 未检出差异 |
| 1000 次热 open/fstat/close | 0.66923 | 0.669354 | [-0.004375, 0.007437] | 未检出差异 |
| 1000 次热 open/read/close | 175.524417 | 174.525375 | [-4.939437, 2.094083] | 未检出差异 |
| 宽目录只 stat 一个文件 | 0.285292 | 0.186479 | [-0.116313, -0.086104] | 新固件更快 |
| 立即重复 stat 同一文件 | 0.00075 | 0.000792 | [-0.000021, 0.000104] | 未检出差异 |
| 空闲 1.3s 后逐路径 stat | 154.515812 | 153.096125 | [-5.37973, 0.538187] | 未检出差异 |

### 显式 Immutable share

| 负载 | 旧固件 | 新固件 | 差异 95% CI | 判断 |
|---|---:|---:|---|---|
| 2048 路径首次 stat | 62.853792 | 136.324646 | [65.999354, 77.801125] | 新固件更慢 |
| 立即重复 stat | 0.82752 | 0.813667 | [-0.058104, 0.035146] | 未检出差异 |
| 深目录首次 stat | 122.905146 | 193.367542 | [67.262501, 73.053959] | 新固件更慢 |
| 深目录重复 stat | 1.104667 | 1.11675 | [-0.012396, 0.043479] | 未检出差异 |
| 首次枚举及 stat | 61.916125 | 62.800083 | [-0.472125, 3.09302] | 未检出差异 |
| 重复枚举及 stat | 0.998979 | 0.973812 | [-0.063604, 0.018125] | 未检出差异 |
| 首次读取 64 MiB | 406.395562 | 492.337688 | [76.045605, 141.098229] | 新固件更慢 |
| 重复读取 64 MiB | 473.555834 | 468.109313 | [-13.535354, 4.571] | 未检出差异 |
| 1000 次热 open/fstat/close | 0.690229 | 0.689979 | [-0.008126, 0.00975] | 未检出差异 |
| 1000 次热 open/read/close | 160.22298 | 160.970459 | [-2.18225, 2.483708] | 未检出差异 |
| 宽目录只 stat 一个文件 | 54.377916 | 0.166333 | [-54.556646, -53.974438] | 新固件更快 |
| 立即重复 stat 同一文件 | 0.002771 | 0.000855 | [-0.002041, -0.001771] | 新固件更快 |
| 空闲 1.3s 后逐路径 stat | 4.887417 | 148.949563 | [140.849854, 148.344625] | 新固件更慢 |

### 请求数与行为边界

以下为独立 profiling 的完整请求计数断言；sparse 的请求增量相对仅启动的 empty VM，open/fstat 相对 prime-only VM。完整 VM 差分可能包含少量 rootfs LOOKUP 噪声。

| cohort / 固件 | sparse 新增 READINDEX / LOOKUP | 1000 次 open/fstat 新增 OPEN / READ / FLUSH / RELEASE | 完整负载各连接 READINDEX |
|---|---|---|---|
| sdk / 旧 | 2 / 0 | 0 / 0 / 0 / 0 | [358, 1] |
| sdk / 新 | 0 / 2 | 0 / 0 / 0 / 0 | [33, 0] |
| cli / 旧 | 1 / 2 | 0 / 0 / 0 / 0 | [358, 9] |
| cli / 新 | 0 / 2 | 0 / 0 / 0 / 0 | [1, 0] |

新固件在两个 cohort 的 sparse VM 均没有 READINDEX；CLI 完整扫描的每个连接至多一次能力探测；SDK 的完整负载只有 scan 组的 33 次 READINDEX。open/fstat 不新增宿主句柄请求。显式枚举仍按原有分页预算装入目录；提前停止枚举巨型目录的进一步优化不在此实验内。

逐路径 stat/read 的首次退化是明确取舍：这些负载没有表达目录枚举需求，新固件不猜测后续会扫描所有兄弟项。若要同时保留这类批量访问收益，应另测有界的批量元数据接口或需求提示，不能恢复首个 lookup 无条件预取全部目录。

### 验证与来源

- `just test pvisor-vm` 原生环境：282 通过、1 跳过。沙箱内虚拟化信息查询/socket 限制的失败日志保留；原生重跑通过。`just test-py benchmark/pvisor/test_guest_cache.py`：5 项通过。
- 8 组最终固件真实客机矩阵通过（并发扫描/读取纳入其中）：guest cache/writeback 组合、扫描后 unlink/rename/copy-up、异步写错误、外部 Mutable create 可见性、300 目录镜像的正常卸载。
- 首次最终固件矩阵在外部 Mutable 创建测试中读到宿主正写入的空文件；夹具改用写完临时文件后原子 rename，保留原内容断言，完整八组重跑通过。原失败日志保留。并发读取测试保持真实 guest/host 请求，没有人为停顿内核发布路径。
- 固件和 release 构建完成；`target/release/libkrunfw.5.dylib` 与新固件摘要一致，匹配 SOURCE 收据。macOS 使用随程序分发的 dylib。
- `just fw test` 1 通过、1 失败：未修改的 flat bundle ABI 测试在 macOS 链接器将 64 KiB 段对齐降为 16 KiB 后，指针对齐断言失败。该失败未被忽略或改弱，也不计作通过；真实 ARM64 VM 矩阵独立通过。
- 未测 Linux/KVM、磁盘冷缓存、完整 Agent 任务或其他容器实现。
- [复现方法](README.md#guest-directory-images-and-deferred-opens)、加工 CSV（本地归档：`.data/derived-tables-20261018/guest-cache-demand-summary.csv`）。原始正式报告 `.data/guest-cache-demand-final-formal-20261016/report.json`；独立请求报告 `.data/guest-cache-demand-final-profiles-20261016/report.json`；断言结果 `counter-checks.json`。
- 冻结制品、harness、内核源码补丁和来源收据 `.data/guest-cache-demand-final-build-20261016/`；共享输入和未变动的宿主源码来自 `.data/guest-cache-build-20261016/`，前后均验摘要。最终预检 `.data/guest-cache-demand-final-preflight-20261016/`；失败及中间预检 `.data/guest-cache-demand-preflight{,2,3}-20261016/` 单独保留；验证日志、报告生成器及 counter 检查器在 `.data/guest-cache-demand-validation-20261016/`。

| 制品 / 数据 | SHA-256 |
|---|---|
| guest | `1377db178c97f94cbce0f1553609625c7a752928cdc165d613e87f40773c1efd` |
| libkrunfw.5.dylib | `4caadb481da344f29cb8807cebb037215dababddba4e28345759a75a44dfa2ee` |
| pvisor | `04ec88f417f8c6e8a572a20e3acebf7c34a13d1e6d096273cc45d1abc16331e3` |
| sdk | `2f08d389f8723d981e0505e5eee38f1e8bc4ff6d2320d4793afc6f9b4c5a3611` |
| formal/report.json | `fe18636c58c732e8d9341d26a5b6a8a92a5a992c020e9c5a85dab5bd7b1ff913` |
| formal/summary.json | `b58850e3d8027bb0294a7af2694d07e2538e24ae1eeb92c3ea4eb76b61c040ea` |
