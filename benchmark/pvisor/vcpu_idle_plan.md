# EXP-001 M0：真实 guest 等待机会与观察者成本

## 主要结论

**当前是可构建、可运行的实验实现，不是已测结果。** 真实 VM 的窗口命中、负对照和 observer 开销均待用户选择资产后测量。卸载收益未测；M1 wake/deadline 闭环、M2 自动 offload 未实现。结果属于 **B-VCPU-IDLE-ENG / engineering A/B**，不进入用户 benchmark 正文。

## Motivation

问题：真实 guest 等待、计算、SMP 和短 timer 下，现有 vCPU observation 能发现哪些等待窗口，观察者开销多少？这些数据用于决定是否继续研究，不能用低 CPU、没有 KVM exit 或单测通过推导自动卸载安全性。

## 实验设计

### 实现与后端语义

`vm_vcpu_observe` 直接使用 `pvisor_vm::api` 的 `VmBuilder` / `VmConfiguration` / `VmRuntime`。ready callback 接收真实 `VmmHandle`，只启动一个有界采样线程；线程在 guest ready 后调用 `VcpuObservationControl::set_vcpu_observation(bool)` 和 `vcpu_observation()`。没有产品 CLI 控制端点，也没有模拟快照代替真实 API。

- guest 使用现有 pvisor-guest Linux PID 1 init 的 JSON 协议；init 为显式输入资产，通过 SDK synthetic file 提供。不修改 guest 内核或 init。
- guest ready → observer 初始化 → go → guest workload/done → 保存最终样本与完成记录 → release → guest 正常退出。宿主 runner 校验退出码及 `vcpu-guest-ok`，异常不能仅凭 done 判为成功。
- snapshot 完整保留 backend、enabled、session、topology generation、sequence、时间、idle epoch、全等待累计时间/起点、拒绝原因和全部 CPU 当前记录/计数。
- KVM_RUN 内 `Unknown` 是预期的诚实结果；不要求 sleep 命中等待，也不从低 CPU 推导等待。HVF 的 `WaitingForEvent` 是实际 backend wait，不证明 Linux runqueue 空闲。
- 全等待只是一种机会，必须是所有 registered online CPU 都 WaitingForEvent；`WakeDeadlineUnavailable` 阻止授权卸载。CPU online 不等价于 guest Linux online；本负载另外校验 guest CPU 亲和性与 worker 并发。

### 矩阵、计时与负对照

每个 pair 两个 fresh VM，256 MiB；一台 VM 在运行，配对轮次及 case 顺序固定 seed 随机化，off/on 顺序也随机化并在启动前保存 `schedule.json`。

| guest 负载 | vCPU | 用途 |
|---|---:|---|
| sleep | 1、2 | 每个 pinned worker 长 sleep，等待机会 |
| busy | 1、2 | SHA-256 不断计算，计算负对照 |
| short-timer | 1、2 | SHA-256 校验与 2 ms sleep 交替，短 timer 窗口 |
| smp-one-busy | 2 | CPU 0 busy，CPU 1 sleep，不能以某一 CPU 等待推导全 VM 空闲 |

Python guest 使用 multiprocessing（不是受 GIL 串行化的两个线程），分别 pin 到 guest CPU 0/1。每个 worker 计算/验证固定 64 KiB payload 的 SHA-256，返回 iterations、digest、affinity、guest 单调起止与 CPU 时间；所有 worker 必须完整返回、exit=0，时间覆盖预定时长，且共享工作区间至少为预定时长的一半。CPU 预算或 host 抢占可以影响工作量，不能转成确定 idle。

默认每个 worker 3 秒、10 ms 采样、5 个 pair：7 cases × 2 conditions × 5 = 70 fresh VM。`--pairs 1` 是独立预检 cohort，不与正式批合并。所有参数有上限；样本容量 `(seconds+60)*1000/interval_ms+2`，外部 VM 进程组 timeout 默认 90 秒，内部线程 deadline 为 ready callback 后 seconds+60 秒。超时 kill 整个 VM 进程组并 wait4 回收。根文件系统复制、制品冻结在 VM wall timer 外。

off 使用同一 guest 与握手轮询线程，只读一次 disabled 初始 snapshot；不做周期 API 采样。on 开启 collector 并执行周期 API 调用、JSON 编码及 JSONL 写入。A/B 测的是**collector + sampler + 证据 I/O 的总开销**，不隔离纯 collector 接点成本；`sample_call_and_encode_ns` 只计 snapshot + JSON Value 构造，不含 JSONL 文件写入，也不是纯锁耗时。

`wall_s` 从 VM 子进程启动到 reap，包含启动、握手、任务及退出；`cpu_s` 来自 wait4 的 VM 进程/全部线程与已回收 guest host-side helper CPU，不含协调者 Python/rootfs 复制。协调者仍有轮询成本；这不是全机预算或生产 density 实验。guest elapsed/CPU、iterations 独立保留，以便审查总 wall 的启动噪声和固定时长 busy 的吞吐变化。

### 窗口解释与限制

`sampled_wait_epochs` 是采样命中的 epoch 数；`idle_epoch` 和 `completed_all_waiting_ns` 是 collector 内的 session 累计值，可包含采样间隙中发生的短窗口。最后尚未结束的等待时间单独列出。不要把采样次数当窗口数或把未采到当作未发生。

采样覆盖 go 到 done 的握手范围，含 multiprocessing 启动、worker 完成到 done 的间隙；不能将此范围内任意等待直接判为 busy 稳态误报。guest monotonic 与 host observation origin 不做未经实现的 clock conversion。SMP/busy 若出现机会，须审查原始状态、时间与 worker 起止，不能自动宣称“不应有的窗口”或策略通过。当前不检验 host 拥塞、外部 network/futex、timer 取消、lost wake、热插拔、卸载恢复、RSS/PSS 节省或 memory-time integral。

### 资产、来源与失败

显式传入已准备的同架构 Linux rootfs、firmware directory、static pvisor-guest init 和绝对 guest Python 路径。Python 要有 hashlib/multiprocessing 与 Linux sched affinity；rootfs 应适合 pvisor-guest init 挂载 `/proc`、`/sys`、`/dev`、`/dev/shm`。每次用独立 rootfs 副本，不把用户原始 rootfs 附给 VM。输入允许符号链接，不支持 device/socket/FIFO 这类 special file。原始 rootfs 保持不变；预置 vcpu 握手文件会被拒绝。宿主需 KVM 权限或 Apple Silicon HVF entitlement。

`--build` 实际运行 targeted Cargo build；保存 build log、命令、rustc、Git HEAD/dirty patch、构建环境、参与源码清单及副本、Cargo 返回的 example binary。macOS 在摘要前签名。`--describe` 不启动 VM，记录 API capabilities、动态固件名称或 build-embedded kernel SHA-256/几何；runner 拒绝当前/frozen 源码、binary、SDK identity 不匹配的收据。source inventory 覆盖 crates、scripts、.cargo、Cargo manifest/lock/toolchain 和本实验 harness/计划，不宣称可重现 toolchain/sysroot/所有外部链接依赖。

实验保存完整 rootfs/firmware 清单、init/固件文件 hash，逐次命令、stdout/stderr、guest、initial、observer、samples、失败 traceback 和报告。Linux 动态固件需出现在真实 VM `/proc/self/maps`；macOS 记录并校验指定搜索目录的资产身份，但未做 loaded-image attestation。embedded kernel 构建优先使用嵌入资产，不能假称测了传入动态固件。原始 input 与 build receipt 在批末复核；变化使整个 cohort incomplete。

禁止 global sysctl/sysfs 修改；不隐式下载资产。不与 build/test/其他测量并行。没有 cgroup cap 或自动宿主干扰检测，保存 affinity/host uname，用户应在空闲同一宿主单独运行；不能事后删除慢样本。

失败样本完整保留、status=failed，不计为零；配对任一失败则整 pair 不进入差异统计，invalid pair 数明确报告。矩阵会继续保留其余格，任何失败最终 exit 非零；cohort 来源/输入复核失败则报告 incomplete，不可使用 summary 声称收益。

## 实验数据和分析

未运行真实 VM。runner 已实现逐 case 的 wall/CPU on-minus-off 中位数与 paired bootstrap 95% CI（2000 resamples），不跨 cohort 合并，不报告小样本尾延迟。n=1 的区间只描述该 pair，不估计总体不确定性；区间含 0 时只能称“未检出差异”。分布形状、guest 吞吐、宿主干扰需结合原始数据人工审查，不能仅据汇总表排名。

复现与构建命令见 [runner README](README.md#vcpu-observation-m0)。原始证据只写 NEW `.data/` 目录；本计划不包含虚构测量数值或用户收益。
