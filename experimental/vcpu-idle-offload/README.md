# EXP-001：vCPU 空闲观测与自动内存卸载

| 项目 | 当前状态 |
|---|---|
| 阶段 | **M0：观测原型；尚未通过全部退出门槛** |
| 核心目标 | 利用 VM 等待窗口降低驻留内存，宿主事件到来时自动恢复 |
| 默认行为 | 保持现有行为；尚无自动 offload 开关 |
| 首选路径 | pVisor vCPU/宿主观测，不修改 Agent 或 guest 内核 |
| HVF | 已接入 wait/timeout 观测与全 vCPU 聚合；未实际编译/运行验收 |
| KVM | 真实 VM API 与实验通过；KVM_RUN 内 Unknown，尚无可靠 idle 信号 |
| 现有基础 | pause/resume、RAM offload、device memory gate、Host backing commit |
| 主要新增 | 等待窗口观测、全 vCPU 聚合、策略 owner、wake/deadline 与 lost-wakeup 防护 |
| 尚未完成 | 完整 idle 来源、wake/deadline、自动 offload、HVF 验收与卸载收益实验 |

## 用户价值

Agent 等待模型响应、输入或外部工具时，VM可能不需要执行，但仍保留大量驻留RAM。短等待让CPU自然休眠；有足够长的等待窗口时，评估自动offload，减少内存时间成本。唤醒后继续同一个live Attempt，不将其冒充持久checkpoint或重新启动。

选择是否卸载由宿主决定，依据窗口长度、内存压力、设备安全、恢复预算和支持范围；不对每次线程阻塞立即卸载。

## 阅读顺序

1. [设计](design.md)：host-driven路线、后端差异、状态机、wake和安全边界。
2. [实施计划](implementation-plan.md)：从观察器到受限PoC、目标代码归属、验收矩阵与证据门。
3. [第一版实现与验证](validation.md)：实际 API、平台状态、真实实验及后续门槛。

## 当前约束

- 第一版不要求guest内核通知；仅保留PV通知作为将来精度增强。
- 自动策略的观察是机会信号，不是“所有Linux进程都阻塞”的证明；安全卸载仍依赖CPU/设备quiescence与backing提交。
- 初期只支持明确的普通file-backed RAM；现有cold pager/pool与whole-VM offload互斥，private restored COW不支持该discard路径。
- deadline/wake支持不完整的平台或设备，只观察，不自动卸载。
- 不解除人工pause、snapshot freeze或失败停驻，不授予guest Host控制权限。

现有手动offload数据只支持基础成本分析，不能宣称自动策略已经提升生产密度。当前观测结果不能授权自动卸载，设计不是验收结果。
