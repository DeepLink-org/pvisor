# VM 内存实验：方法与计量边界

[使用决策](index.md) · [完整结果](results.md)

## 本轮范围 {#scope}

2026-10-03 固定构建快照后执行，使用 `just build release` 构建并签名的
真实 `pvisor run` 和 `pvisor-memory-pool`，不使用旧 SDK fixture 的结果替代
CLI 测量。每个 VM 运行同一个静态 Linux 程序，实际分配并访问 64 MiB 数据。
这是内存机制的受控负载，不是 Ubuntu、Python Agent、LLM 推理或编译项目的
端到端基准。镜像下载、构建和 rootfs 准备不计入运行样本。

| 环境 | 本轮条件 |
|---|---|
| 宿主 | Apple M4，24 GiB RAM；macOS 27.0.1 / 26A434 |
| pVisor | 0.3.0；记录提交号、工作树运行源码与签名二进制 SHA-256 |
| VM 后端 | 本地修补 libkrun / krun-hvf 1.19.3；HVF |
| 固件 | 已有 libkrunfw 5.5.0 缓存；记录实际 dylib SHA-256 |
| guest 程序 | Rust 1.98.0，aarch64 Linux musl，优化级别 2 |
| 宿主页 | 16 KiB；pager 块 64 KiB |
| 观测 | 每秒采样进程与宿主；所有成功 case 同样开启实验诊断 |
| 压力控制 | 不增加额外压力分配器；观测真实 NORMAL / WARN，critical 时停止 |

测量针对本轮固定的构建快照。并行工作的后续源码修改或重新构建不进入这组数据；二进制摘要、构建时 runtime 工作树差异与实验源文件保存在 evidence 中，不能把本报告称作后续全部代码的验证。

## 两组数据分别回答什么 {#datasets}

| 数据集 | 运行数 | 要回答的问题 | 主要观察窗口 |
|---|---:|---|---|
| 35 秒参数矩阵 | 40 | 相同任务时长下，不同参数与负载怎样影响回收和恢复 | ready 后 18–33 秒 |
| 2 GiB 延长观察 | 4 | 大容量 VM 在扫描启动后是否继续回收 | ready 后 60–90、120–150、150–175 秒 |

两组使用相同固定 CLI、池与固件，不合并成一个节约率。175–178 秒的末段在查看首轮曲线后作为描述性补充，每次只有三个采样，不能代替预设窗口或稳态验收。首页采用四舍五入后的代表性结果；完整证据保留原精度。

## 参数与负载矩阵 {#matrix}

每个配置比较共享池关闭与开启，各重复两次；第二次反转配对顺序。
正式计划为 10 个配置、20 对、40 个成功 case。重复次数不够支持可靠的
跨运行 p95/p99 或置信区间，因此保留两次配对的范围，不把多个秒级采样点
当作独立重复实验。

| 变量 | 设置 |
|---|---|
| 每 VM 内存 | `--memory 256MiB` / `512MiB` / `2048MiB` |
| vCPU | `--cpu 1` / `2` |
| VM 数 | 单 VM / 同时两个 VM；后者共享同一个池 |
| 共享池 | 未指定 / `--vm-memory-pool SOCKET` |
| 池 payload 上限 | 默认 16 MiB / `--max-bytes 1048576` |
| RAM 文件 | 自动临时 backing / `--vm-ram-backing FILE` |
| 重复冷数据 | 64 MiB 周期性字节序列，两 VM 初始内容相同 |
| 随机冷数据 | 各 VM 使用独立种子的 xorshift 内容，不假设可压缩或可共享 |
| 热数据 | 每 50 ms 扫描各 4 KiB 页，使用编译器屏障避免读取被消除 |
| 网络 | 相同冷数据＋OverlayNet TCP loopback echo；每 VM 三次 32 MiB，校验内容 |

所有 guest 就绪后，第 18–33 秒作为固定观测窗口。它是统一时间窗口，
**不保证所有配置已进入稳态**；大 VM 的扫描与回收可能仍在推进。之后
完整读取 64 MiB 三次，每次间隔两秒，并保留首次与后续访问的耗时。
后续读取也可能再次触发冷恢复，不能把它们自动称为“热读”。

校验主体是单线程；1／2 vCPU 对比覆盖参数可运行性与该负载代价，不是多线程应用的 CPU 扩展性结论。每台 VM 的 64 MiB 首次读先求 VM 间中位数，再求两次运行中位数；倍率按每对计算，可能不同于汇总耗时相除。

本轮是**相同任务时长下的启动阶段比较**，不是所有容量的稳态比较。2 GiB 配置在统计窗口内池 payload 为 0，首次非零采样约在 ready 后 34.3 秒；[结果页](results.md#scan-startup)给出扫描机制、时间证据和后续稳态测量要求。

## 内存、压力与性能口径 {#metrics}

| 指标 | 计算 / 范围 | 不代表什么 |
|---|---|---|
| RAM＋池代理 | 各 runner 的 RAM 驻留诊断＋待文件回收字节＋暂存快照字节＋池编码 payload | 不是全部进程或全机唯一物理页；不含所有元数据 |
| 进程组 footprint 账目 | 各 CLI、runner、池的原生 `proc_pid_rusage` footprint 相加 | 文件缓存和共享归因与 RAM 代理不同，不能互相替代 |
| 进程组 RSS 总和 | 同一组进程的 resident size 相加 | 共享页可能重复计量，不是唯一物理内存 |
| 全机压力 | `kern.memorystatus_vm_pressure_level`，1 NORMAL / 2 WARN / 4 critical | 不是只由 pVisor 造成的压力 |
| 宿主压缩器 | `vm_stat` 的 occupied pages × 实际页大小 | stored pages 是逻辑未压缩页数，不能替代物理占用 |
| swap 变化 | 每 case 首尾的全机 swap used 差 | 不可直接归因给当前配置 |
| CPU 秒 | CLI＋runner＋池的原生 CPU 计数，经 Mach timebase 转为秒 | 采样可能漏掉退出前尾段；不包含 Python 观测器 |
| 首次读取 | guest 完整读取 64 MiB 的校验耗时 | 不是单页 fault 或一般 shell 命令延迟 |
| 维护屏障 | 两阶段发布的同轮屏障调用合计；保留各运行 P95 与最大值 | 不包括屏障外发布，也不是总访问延迟 |
| 冷恢复最大值 | pager 记录的单块 restore 最大耗时 | 不是整段 64 MiB 的完整恢复时间 |

CPU 原始值不能直接当作纳秒。本机 Mach timebase 为 125/3；使用自有
短时 CPU 负载与 `time.process_time()` 校准。内核填充字段的位置见
[Apple XNU `fill_task_rusage`](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/kern/bsd_kern.c)。
正式记录同时保存原始 ticks、换算后的 ns 和校准比值。

节约率按每对 `1 - pool / baseline` 计算，再汇总两次配对。运行内的 P95
来自该运行的维护事件；两次运行的 P95 汇总不能解释为全体事件的 P95。
保留中位数、采样最大值、首次唤醒和恢复峰值，避免只展示安静期的最佳数字。

屏障指标测量两次调用的墙钟耗时，可能包含调度和等待，不是 vCPU 停止时间的精确累计。单／双 VM 还改变每 VM 可分的共享预算，不能独立识别去重贡献。

每秒观测不是跨进程原子快照，RAM 诊断、池 payload 与宿主计数存在采样时间差；RAM 记录要求可用且不超过三秒旧。所谓峰值都是观测到的采样最大值或日志事件最大值，不是未观测间隙的硬上界。

## 复现 {#reproduce}

先设置已有固件目录；输出目录必须不存在，避免覆盖旧数据。命令只适用于
可运行 HVF 的 Apple Silicon macOS，需要允许宿主 socket 与 Hypervisor。

```bash
just build release
FIRMWARE_DIR=/path/to/pvisor/firmware/5.5.0/macos-aarch64
EXPERIMENT_BIN=$(mktemp -d /tmp/pvisor-memory-bin.XXXXXX)
cp target/release/pvisor target/release/pvisor-memory-pool "$EXPERIMENT_BIN/"
codesign --verify --strict "$EXPERIMENT_BIN/pvisor"
python3 tools/experiments/macos-memory/cli_decision_matrix.py \
  --firmware "$FIRMWARE_DIR" \
  --binary-dir "$EXPERIMENT_BIN" \
  --output target/memory-cli-matrix-new \
  --repeats 2
```

原始记录使用独立 `pvisor-memory-cli-matrix/v1` schema，不伪装成现有 host
microbenchmark 的 `pvisor-benchmark/v1`。每个 case 保留完整参数、stdout、
stderr、进程身份与时钟计数、宿主 `vm_stat`、池统计和校验结果。
汇总器重新核对配对、输入内容、CPU 单位、RAM 代理和引用回收，失败样本
留在原目录中，不参与成功样本的性能比较。

复制签名后的可执行文件是为了隔离同时发生的构建。正式案例启动与结束均核验 CLI SHA-256，所有配对必须匹配矩阵记录中的同一摘要。不要从正在被构建覆盖的 `target/release` 持续启动样本。

## 限制与停止条件 {#limits}

同一台宿主还有其他应用，后台负载没有被隔离。宿主压缩器、swap 和压力
变化只作为环境与停止依据；本轮不宣布完整物理内存验收通过。两次重复也
不能证明长期稳定性、应用覆盖或生产 SLA。

守卫在 critical、swap 相比本轮起点增长至少 2 GiB，或可用磁盘低于 4 GiB
时停止。没有关闭其他应用、索取管理员密码、自动启用 macFUSE，或降低
已有物理收益验收门槛。

配对在同一宿主顺序执行，但不保证压力等级或后台负载完全一致；压力表逐模式、逐重复公开环境变化，不能把这个实验当作隔离主机上的因果压力实验。

## 2 GiB 延长观察复现 {#long-idle-2048}

在独立输出目录运行同一固定二进制，保留两次配对与反转顺序。默认仍为 35 秒；补测只增加等待时长和与之匹配的超时。下列路径需替换为实际的固定二进制、固件及输出目录。[结果页](results.md#long-idle-2048)区分预设窗口和描述性末段。

```bash
python3 tools/experiments/macos-memory/cli_decision_matrix.py \
  --binary-dir /path/to/frozen-binaries \
  --firmware /path/to/firmware \
  --output /path/to/evidence/cases \
  --only cold-2048-dual --idle-seconds 180 --repeats 2
python3 tools/experiments/macos-memory/long_idle_report.py /path/to/evidence
```
