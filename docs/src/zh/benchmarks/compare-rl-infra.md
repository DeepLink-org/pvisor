# pVisor 能为现有 Agent RL 管线提供什么？

## 主要结论 {#conclusions}

**pVisor 可提供每次尝试的执行、暂存、轨迹与恢复单元，但没有完整 RL 训练吞吐领先的证据。** 本机启动、工具任务成本可用于预算；它们不能推导每秒有效 rollout 或训练成本降低比例。

已有 OpenHands/SWE-Gym/verl 管线可以继续使用；需要跨 Agent 统一工作区与执行证据时评估 pVisor。模型服务、任务、reward 和训练调度仍由训练系统负责。

| 需求 | 选型含义 |
|---|---|
| 已有 OpenHands / SWE-Gym / verl | 保留任务与训练层 |
| 需要统一执行记录与暂存 | 将 pVisor 作为执行层评估 |
| 关心训练吞吐和成本 | 目前没有完整训练对照 |

## Motivation {#motivation}

一次 rollout 不仅是创建 sandbox，还包括工具执行、模型等待、测试验证、失败重试及保存样本。比较基础设施需要看每个有效结果的总成本。

## 实验设计 {#interpretation}

已测本机执行与回放契约，没有执行完整 SWE-Gym/verl 训练对照，也没有比较不同论文成功率。官方项目用于说明职责，专用集成未验收就不称为兼容。

| 工具 | 职责与比较范围 |
|---|---|
| OpenHands runtime | Agent 工具环境；Docker sandbox 可挂载本地仓库 |
| SWE-Gym | 仓库任务、可执行环境、测试验证及 Agent/verifier 训练 |
| verl | 训练、rollout 与模型资源协调 |
| pVisor | Job、stage、轨迹、回放与 VM checkpoint/fork；不替代训练算法 |

依据 [OpenHands](https://docs.openhands.dev/openhands/usage/sandboxes/docker)、[SWE-Gym](https://github.com/SWE-Gym/SWE-Gym)、[verl](https://verl.readthedocs.io/en/latest/)。

固定制品与测量日期按表注明。失败与校验不通过的样本不计入成功耗时，失败数量单列；既有数据没有事先的宿主干扰剔除规则，所有通过校验的慢样本保留。30 次及更少采样的 P95 仅为观察参考，不给 P99 或稳定尾延迟承诺。

## 实验数据和分析 {#results}

以下只列 pVisor 的本机支撑数据，各主题的样本数、固定制品与测量日期见链接。OpenHands、SWE-Gym、verl 的完整训练吞吐和有效样本成本未测，不做速度排名。

| 成本项 | 可用证据 | 可支持的用途 |
|---|---|---|
| [启动](startup.md) | 本地 VM 约 0.1 s | 估算一次性环境等待 |
| [工具任务](agent-tasks.md) | 修复到退出，staged 0.68 s；VM 3.25 s；N=60，2026-10-06 | 按执行边界估算工具预算 |
| [前缀准备](replay-fidelity.md) | 固定六种格式约 5–5.5 ms | 准备已记录历史；不证明模型下一动作相同 |
| [并发密度](density.md) | 空闲环境探针 | 估算基础占用；不等于有效 rollout 吞吐 |

工具回放重新执行操作，VM checkpoint 恢复 CPU/RAM 及对应设备/文件状态，不能把所有远端连接都视为可恢复。真实训练仍需测任务成功率、失败重试、总资源与每个有效样本耗时；当前数据没有回答 pVisor 是否比完整 RL 管线更快。

### 数据下载与复现 {#run}

[整理后的表格 CSV](compare-rl-infra.csv) · [证据来源摘要](evidence-sources.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
