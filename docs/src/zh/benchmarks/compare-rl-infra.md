# 对比：Agent RL rollout 基础设施

pVisor 适合作为 rollout 的执行与证据层：每个尝试有独立环境、改动、轨迹和恢复入口。训练算法、模型服务、reward 与集群调度仍由训练系统负责。第一版能判断本机执行成本和回放契约，还不能给出完整 RL 训练吞吐提升。

## 比较范围（2026-10-04）

| 方案 | 主职责 | 与 pVisor 的关系 |
|---|---|---|
| OpenHands sandbox/runtime | 执行 Agent 工具、提供工作环境；官方 Docker sandbox 可挂载本地仓库 | 可以比较执行与工作区边界；pVisor 的 OpenHands 回放适配器有固定版本契约，不能推断最新 OpenHands 全兼容 |
| SWE-Gym | 真实仓库任务、可执行环境和测试验证，训练 Agent 与 verifier | 提供任务和 reward 语义；pVisor 可管理每次尝试的执行与证据，本版未完成 SWE-Gym 实验集成 |
| verl 等训练框架 | 训练、rollout、模型与计算资源协调 | pVisor 可由 rollout worker 调用；不替代优化器、采样器或训练调度，本版无经验证的专用 verl connector |
| pVisor | Job 生命周期、stage、Gateway 轨迹、回放、VM snapshot/fork | 提供有记录的执行单元，训练方负责模型、任务、reward 和批量编排 |

对照依据 [OpenHands Docker sandbox](https://docs.openhands.dev/openhands/usage/sandboxes/docker)、[SWE-Gym 项目](https://github.com/SWE-Gym/SWE-Gym)、[verl 文档](https://verl.readthedocs.io/en/latest/)。这些能力比较没有采用不同论文的任务成功率来排名。

## 一次 rollout 的集成路径

调度方准备固定任务与 rootfs → 创建独立 pVisor Run → 将模型请求经过 Gateway（如需轨迹）→ 执行测试并保存 reward → 保留 Bundle 和 stage → 失败时选择工具回放或 VM checkpoint fork → 将结果与训练样本绑定。所有子进程、共享目录和模型 endpoint 都应进入清楚的边界配置。

工具回放重新执行历史操作并获取新观察；VM checkpoint 恢复 CPU/RAM 与相应设备、文件状态。二者代价和兼容范围不同，不应把任意 API 请求、远程服务连接或模型状态都称为可恢复。[回放保真度](replay-fidelity.md)区分前缀重建与真实模型下一动作；[VM 快照](vm-memory/index.md)给出保存与恢复代价。

## 性能规划

先用[并发密度](density.md)估算空闲环境成本，再加入真实编译、测试、模型等待、轨迹 I/O 与任务文件规模。1 秒占用探针的并发数不等于每秒有效 rollout 数。需要记录任务成功率、失败重试、每个有效样本耗时和总资源，才能判断对训练预算的实际收益。

当前可用性应分为：本机执行/暂存与快照有实测；适配器契约有测试；完整 SWE-Gym/verl 训练 throughput 未测。已有成熟的 OpenHands/SWE-Gym 管线可以继续使用，需要跨 Agent 统一恢复与证据时再评估 pVisor。

## 更正

通过 [pVisor issues](https://github.com/DeepLink-org/pvisor/issues) 提交训练框架版本、模型、任务集、并发配置和原始样本；新增集成需附执行边界与 reward 记录。
