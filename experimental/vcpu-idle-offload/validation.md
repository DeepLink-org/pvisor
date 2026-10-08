# EXP-001 第一版实现与验证

## 当前结论

**已交付 M0 observe-only 观测原型和真实 VM 实验，不是自动 offload 第一版闭环。** KVM 实测证明当前用户态接点不足以识别等待：sleep/busy/短 timer/SMP 条件几乎都为 Unknown。不能为了完成自动策略而把低 CPU、未返回的 KVM_RUN 或 shutdown HLT 改作 idle。

HVF 等待路径已接入，但本 Linux 工作区没有实际 macOS 编译或运行验收。M0 未完成全部退出门槛；M1/M2 未实施，不新增自动 offload CLI 开关。

## 实际代码与 API

- `crates/pvisor-vm/src/api.rs`：`VcpuObservationControl`、状态/拒绝原因、逐 CPU 记录与聚合快照，公共契约集中于该文件。
- `crates/pvisor-vm/src/vcpu_observation.rs`：默认关闭的单 VM collector，无后台线程和事件队列，内存 O(vCPU 数)，session/sequence/topology generation/idle epoch、等待累计与拒绝原因。
- `crates/pvisor-vm/src/vmm/{builder,mod}.rs`：collector 生命周期与 CPU 注入。
- `crates/pvisor-vm/src/vmm/{linux,macos}/vstate.rs`：后端观测，人工停驻与停止保持原控制语义。
- `crates/pvisor/examples/vm_vcpu_observe.rs`：真实 SDK VM、有界采样和 guest 正确性负载。

```rust
use pvisor_vm::api::VcpuObservationControl;

handle.set_vcpu_observation(true)?;
let snapshot = handle.vcpu_observation()?;
handle.set_vcpu_observation(false)?;
```

此 API 不调用 pause/resume/offload。中途启用不 kick CPU，状态保守 Unknown，直到下一接点。KVM 的 Executing 是执行入口接点，KVM_RUN 内 Unknown，返回后 HandlingExit；HLT 仍为停止。HVF WaitingForEvent 表示 wait/select 接点窗口，可能立即消费已就绪事件，不等价于精确睡眠或 Linux runqueue 事实。

全配置 CPU 的 online 指后端生命周期，不证明 guest Linux CPU-online。停止/尚未启动的槽不冒充全 CPU idle。即使全等待成立，仍以 WakeDeadlineUnavailable 拒绝自动操作。

## 实验与结果

注册 **B-VCPU-IDLE-ENG**，工程 A/B，不进入用户 benchmark 正文。完整[协议](../../benchmark/pvisor/vcpu_idle_plan.md)、[报告](../../benchmark/pvisor/vcpu_idle_report.md)及[复现入口](../../benchmark/pvisor/README.md#vcpu-observation-m0)。

矩阵：sleep、busy、2 ms 短 timer 分别使用 1/2 vCPU；另设 2 vCPU 仅 CPU 0 busy。每格 fresh 256 MiB VM、3 秒 workload、10 ms 采样，observer off/on seeded 随机配对，每 case 5 pairs，共 70 VMs。guest multiprocessing 独立进程与亲和性验证防止 GIL 串行化，逐 worker 校验 SHA-256、工作区间、退出与输出。

| 项目 | 实际结果 |
|---|---|
| 原始预检 | 0/14，通过失败均保留；Python 3.14 forkserver bootstrap 错误 |
| 显式 fork 修复后独立预检 | 14/14 有效，0 失败 |
| 正式批 | 70/70 有效，0 失败、0 timeout，35 完整 pairs |
| 周期采样 | 10,613 snapshots，16,666 条 CPU 状态 |
| Unknown | 16,657 条，99.946% |
| WaitingForEvent / 全等待 epoch | 0 / 0 |
| 配对任务 wall 差异 | 全部 case 的 95% CI 包含 0，未检出差异 |
| 配对 VM CPU 差异中位数 | +48.253～+88.676 ms；2-vCPU busy CI 包含 0，其余为正 |
| snapshot + JSON Value 构造时间 | 描述性中位数 24.566 µs，不包含 JSONL 写入 |

开销包含 collector、sampler 和证据 I/O，非纯接点成本。GNU debug、单宿主、每 case n=5，无 cgroup 限额或连续宿主干扰检测，不推导 release、P95/P99、生产 density 或整机节省。未调用 offload，没有内存回收效果或恢复延迟结果。

## 验证与制品边界

- 最终 `just test pvisor-vm`：345 passed、8 skipped；含真实 KVM busy-loop/OUT/HLT 特殊测试、SMP/session/并发采样/退出/容量/统计一致性回归。
- Python runner：16 个单测通过，包含 Python 默认 forkserver 下的显式 fork 回归、超时、来源、拓扑和输出拒绝。
- example debug 构建、修改文件格式检查通过；Linux VM 库 Clippy 通过。tests Clippy 被依赖的现有 pvisor 告警阻断，未改无关代码。
- macOS/HVF 编译与真实运行未验收；无 lost-wakeup、完整 timer/deadline 或自动恢复测试。

正式测量绑定冻结 binary SHA-256 `af6830774dfd9fef042893c7491c38077377cf7032afeb1add3af433ca83f239`；report SHA-256 `036094ed87fcc5ffd7d7c73d28cd932350bbd16499fae6b8ae28f4655e5bf339`。来源与资产完整摘要在报告，原始证据在 `benchmark/pvisor/.data/vcpu-m0-fork-pairs5-20261007/`。

**实测后修复了快照时间戳锁内一致性，以及 disable 截断逐 CPU 等待记账两项问题。** 最终单测包含修复，但冻结 binary 未替换、真实 A/B 未重跑；上述实验只属于修复前冻结版本。工作区同时有外部开发，旧 receipt 会拒绝当前源码；下一次测量需 NEW build/cohort，不修改历史结果。

## 下一版实施门槛与实验

1. **KVM 信号问题优先**：验证宿主 KVM halt/wake 观测接口是否能按 VM/vCPU 关联、区分抢占并控制权限/开销。不先修改 irqchip、HLT shutdown 或全局 sysctl；无可靠来源则保持 Unknown。
2. **HVF 实机验收**：同矩阵核实 wait 接点、短 timer、单 CPU busy SMP 与 pending event。补 host 拥塞负对照及更低开销的 aggregate-only 采样，不把接点窗口当未来 deadline。
3. **M1 正确性实验**：wake-before-RAM、候选/排空/提交/停驻四阶段注入事件；timer 重编程/取消、人工 pause、cancel、失败，证明不丢事件、不越权恢复。
4. **M2/M4 回收 A/B**：上述闭环通过后，在普通 file-backed raw、单 vCPU、明确网络/timer profile 对比 off/on。独立登记/补全协议，测等待 0.1/1/5/30/60 秒、计算/streaming/短 timer/恢复风暴，重复/随机工作集；固定组预算、完整 cgroup/PSS/cache/helper、memory-time integral、CPU/IO/peak、完整任务延迟和有效结果。当前均未测，不预设节约倍率。

这次实验的价值是及时暴露 Linux 路线的观测缺口，而非给尚不存在的自动卸载策略提供漂亮收益数字。
