---
description: pVisor 是什么、在 Agent 系统中的位置、支持的本地与基础设施两类工作流，以及当前可用的监督层级。
---

# 简介 {#overview}

<span id="policyvisor"></span>

pVisor 为单机上的 Agent 提供受控执行、文件改动交付和可查记录。你可以用原生 Job 跑完任务再审查，用 Docker 风格命令运行独立 Linux VM，或用 OpenSandbox/E2B SDK 驱动本地 daemon。共享镜像与可选内存机制减少重复资源；实际收益按工作负载和平台测量。

**PolicyVisor（pVisor）是一个 Agent 原生沙箱框架，提供文件级的访问与修改监管、沙箱执行生命周期管理，以及镜像与内存资源优化，共同支持安全、可控、高密度的 Agent 执行。**

**Agent-Native：** pVisor 将文件级监管、沙箱生命周期管理和资源复用贯穿 Agent 执行过程。以文件为管理粒度，限定访问与修改范围，隔离候选改动并支持审查；通过 Host 侧 I/O 管理协调沙箱暂停、内存卸载与恢复，在模型等待期间主动回收空闲沙箱的驻留内存；通过镜像按需加载、内存共享与压缩，减少分支探索和回退中的重复资源占用。这些机制共同支持安全、可控、高密度的 Agent 执行。

<span id="_3"></span>

## pVisor 在 Agent 系统中的位置 {#system-role}

pVisor 将面向 Agent 的管理能力接入沙箱执行体系，在 Host 侧监管文件与网络交互、协调沙箱生命周期，并优化镜像与内存资源。下图以 VM 执行器为例，展示 pVisor 在沙箱执行框架中的集成位置。

![上层 Agent、包含 pVisor 管理模块和运行时的 Host 沙箱执行框架、下层 guest VM](../assets/diagrams/pvisor/agent-host-guest-zh.svg)

沙箱运行时 / VMM 承担 VM 的运行与设备执行；pVisor 通过生命周期与资源管理接点协调运行时，通过 Host 侧的 [OverlayCore](../design/overlayfs.md)、[OverlayNet](../design/overlaynet.md) 管理 guest 的文件与网络 I/O。图中的模块关系表示需要相应运行时接点的集成，自动等待管理的支持范围见[实验记录](../design/experimental/vcpu-idle-offload/index.md)。Agent 或上层系统负责决策、结果接受、奖励计算及跨节点调度。

<span id="_2"></span>
<span id="_1"></span>
## 入口与范围 {#interfaces}

原生 Job 的执行器通过宿主 namespace/Seatbelt、OCI runtime 或 VM 安装控制，由运行时管理生命周期、暂存与记录。另有两个面向环境的入口：Docker 风格 CLI 管理逐容器 VM；daemon 为已测试的 OpenSandbox/E2B 协议子集提供本机 VM 生命周期、命令和文件接口。

Job 的 stage/apply、checkpoint 与 Gateway 捕获有各自合同，不能由 sandbox ID 调用。选择入口和平台见[CLI](../reference/cli.md)与[平台矩阵](../reference/platforms.md)。跨节点放置、业务重试和工作流由外部编排器负责。

<span id="task-flow"></span>
<span id="when-to-use"></span>

## 使用场景 {#workflows}

**本地 Agent 工作流**

当你希望限制 Agent 对工作区的影响，并在执行后审查和接受文件改动时，从[第一次运行](../start/first-run.md)开始，完成一次执行、审查与合入。

**Agent 执行基础设施**

当你需要为推理、评测或后训练管理多个执行环境，并降低模型等待与分支探索中的资源占用时，从[基础设施总览](../guides/infrastructure/index.md)开始，选择接入方式与运行配置。

## 今天，以及之后 {#trust-ladder}

监督方式与运行规模是两个独立维度：多个 Job 可以并发运行，SDK 也可以管理多个沙箱；这不自动建立免审、批量合入或分布式调度合同。

| 级别 | 人怎么介入 | 当前状态 |
| --- | --- | --- |
| L0 | 执行前逐条批准命令 | 可使用 Agent 自带审批，或 Job 的 `--ask` 通道 |
| L1 | 执行后审查改动，再选择合入 | 原生 Job 已有 stage、review、apply/drop 与冲突检查 |
| L2 | 策略与证据决定免审，人只抽查 | 可由调用方组合现有检查；统一免审策略与自动合入门禁仍待验收 |
| L3 | 持续审计，人只处理例外 | 证据汇聚和外部编排集成方向，尚无完整产品合同 |

原生 Job 的人工审查路径、独立工作区并行示例和 CI 接入今天可用，见[并行 Agent](../guides/parallel-agents.md)与[CI 指南](../guides/ci.md)。[Daemon](../guides/daemon/index.md) 的 SDK 接口用于沙箱生命周期与进程/文件操作，不继承 Job 的文件合入流程。

[RL rollout](../guides/rl-rollouts.md) 可复用现有记录、分叉和工具前缀回放；训练框架集成与端到端吞吐仍是独立验收项。

!!! note "自动化门禁与容量证据"
    「每单位 Agent 工作的人工监督成本」尚无实测，见[监督成本](../benchmarks/supervision-cost.md)；自动化门禁的缺口见[路线图](../community/roadmap.md)。资源容量以[并发密度](../benchmarks/density.md)的具体配置和证据为准。

!!! note "支持范围"
    文件改动隔离需启用[暂存](../concepts/staging.md)，且不撤销外部 API、数据库等副作用。隔离与资源管理能力以[执行器](../security/executor-boundaries.md)、[接口](../guides/infrastructure/integration.md#capabilities)及实验状态为准；性能收益见对应[基准测试](../benchmarks/index.md)。
