# EXP-001 实施计划与验收

> 已实现 M0 观测 API、后端接点和实验；自动 idle/offload 尚未实现。已完成项与未验收项分开，详见 [验证记录](validation.md)，不承诺固定排期。

## M0：基线与Observe-only

- [ ] 核对当前后端idle、IRQ、timer、offload、device gate和Host commit链。
- [x] 定义跨后端观测契约与Unknown语义，遵守VM公共API边界。
- [x] 注册vCPU身份/topology generation、enter/exit sequence、等待窗口与拒绝原因。
- [ ] HVF从现有wait/timeout路径采集；KVM先验证低成本候选观测，区分guest等待与宿主抢占。
- [x] 接入单一 VM-local collector，只记录、不调用pause/offload；Host策略owner尚未实现。
- [ ] 完成跨平台采集开销、退出清理与容量验收；KVM已完成有界采样与工程A/B，HVF尚未验收。

退出条件：能解释真实任务的等待窗口，不因无KVM exit、低CPU或人工pause误报精确idle；平台未提供可靠信息时Unknown。

## M1：可控wake闭环

- [ ] 增加独立wake latch，覆盖候选/排空/卸载/停驻全部窗口。
- [ ] 网络数据/EOF/错误/连接完成在RAM访问与有界发送等待前登记。
- [ ] 建立deadline clock domain、提前恢复margin与timer取消/重编程规则。
- [ ] 将wake交现有Host控制owner，保证runner ACK后的backing commit顺序。
- [ ] 仲裁自动、人工、snapshot、cancel与失败状态。
- [ ] 验证resume后重查queue/backend，不丢已消费边沿。

退出条件：故障注入和可控交错证明lost wake不出现；deadline未知profile拒绝自动深停驻。mock通过后仍需真实VM验证。

## M2：单vCPU受限raw offload PoC

- [ ] 默认关闭；拟议enable配置经实现评审后才命名、接CLI。
- [ ] 普通file-backed RAM支持检查，排除pager/pool/private COW等。
- [ ] 在途FS/未守护设备提前拒绝，不以真实drain失败作为常规策略。
- [ ] 初期grace、deadline guard、重入冷却、wake频率抑制。
- [ ] 真实delayed network、timer、控制wake下恢复同Attempt并校验全部内容。
- [ ] offload/store/恢复失败沿用fail-stop与unknown，终态不伪造。

退出条件：正确性、权限与生命周期负对照通过；仅对验收的平台/profile标记受限PoC。

## M3：SMP与更多事件

- [ ] 全online vCPU聚合、跨CPUwake/IPI、迁移/topology变化竞态。
- [ ] 一个CPU运行时不卸载；退出/恢复旧epoch不复用。
- [ ] 扩展vsock、终端、signal等事件，不在proxy/queue锁内同步切换。
- [ ] 多连接、streaming、backpressure、恢复风暴与deadline竞争。

退出条件：平台支持矩阵逐格记录，不以SMP mock替真实guest验收。

## M4：收益实验与是否继续

- [ ] 按`benchmark/README.md`先登记自动策略的问题、角色、runner与统计协议。
- [ ] 同任务/硬件/预算/制品，自动策略on/off随机配对，多轮；手动策略可独立对照。
- [ ] waiting/compute/短阻塞/streaming/timer-heavy/恢复风暴分开。
- [ ] memory-time integral、cgroup/PSS/cache、全组peak、CPU/IO、恢复及完整任务延迟、失败和有效吞吐。
- [ ] 计入观察者/全局扫描器等组外成本的说明，不用低PSS或配置容量估生产密度。
- [ ] 正确性失败不算收益；保留失败、干扰与完整来源，不挑样本。

晋级条件：在明确用户负载上有净资源收益且满足QoS，测试可复现、操作可解释。若thrash、CPU/IO或恢复代价抵消收益，就调整策略或退役，不为保留功能强行上线。

## 代码归属建议

| 归属 | 职责 | 验证 |
|---|---|---|
| `crates/pvisor-vm/src/api.rs`与私有后端 | 观测契约、wait/deadline、CPU控制与device gate | API boundary、状态/epoch、timer与RAM正确性 |
| `crates/pvisor/src/executor/vm/` | 观测/控制通道、backing提交、支持profile | 排空/提交/wake并发、错误fail-stop |
| `crates/pvisor/src/runtime/` | Attempt仲裁、自动策略与事件 | manual/auto/snapshot/cancel不越权 |
| `crates/pvisor-overlaynet/src/vm.rs`及后续设备事件源 | wake-before-RAM与外部socket生命周期 | 数据/EOF/错误/背压/已pending事件 |
| 原crate测试与产品集成测试 | mock交错与真实VM验证 | 分平台记录通过/失败/跳过 |
| `benchmark/` | 已登记策略实验 | 独立数据/收据/样本，不放本目录 |

这张表规定职责，不预先规定新文件名或引入依赖；实施时按已有代码结构收敛。

## 必测负对照

| 场景 | 预期 |
|---|---|
| compute busy、宿主CPU拥塞 | 不将低CPU误判为确定guest等待；策略误判也不能破坏安全边界 |
| sleep/futex deadline、timer取消 | 不丢timer，不冻结时间；未知deadline拒绝 |
| 任意卸载窗口到来的wake | 锁存并最终交付，不需guest自己重新发事件 |
| 慢FS持RAM lease | 正常提前跳过；真实transition失败必须fail-stop |
| 手动pause或snapshot freeze | 网络/自动deadline不能擅自解除 |
| backing commit中网络到达 | 等提交完成后恢复，RAM writer不早开 |
| KVM Hlt/guest shutdown | 保留shutdown语义，不变成自动idle |
| 已offloaded时cancel/exit | 清理与终态正确，不保留永远无法唤醒的owner |
| 不支持的pager/pool/COW | 明确拒绝且不改变运行状态 |

## 验证记录模板

每次推进填写：日期、平台、stage/profile、源码SHA+dirty manifest、binary/firmware/rootfs身份、命令、结果、失败/跳过、未覆盖项、证据路径、结论。实际记录见 [第一版实现与验证](validation.md) 和 [KVM 工程实验报告](../../benchmark/pvisor/vcpu_idle_report.md)。

默认使用目标包的`just test PACKAGE`；真实VM/特殊runner按已有契约运行。语义审批与PASS分开，不由AI操作真实ledger。实验实现成功后再同步用户文档和正式支持矩阵。
