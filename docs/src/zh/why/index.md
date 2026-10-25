# pVisor 概览 {#overview}

<span id="policyvisor"></span>

**PolicyVisor（pVisor）是一个 Agent 原生沙箱框架，提供文件级的访问与修改监管、沙箱执行生命周期管理，以及镜像与内存资源优化，共同支持安全、可控、高密度的 Agent 执行。**

**Agent-Native：** pVisor 将文件级监管、沙箱生命周期管理和资源复用贯穿 Agent 执行过程。以文件为管理粒度，限定访问与修改范围，隔离候选改动并支持审查；通过 Host 侧 I/O 管理协调沙箱暂停、内存卸载与恢复，在模型等待期间主动回收空闲沙箱的驻留内存；通过镜像按需加载、内存共享与压缩，减少分支探索和回退中的重复资源占用。这些机制共同支持安全、可控、高密度的 Agent 执行。

<span id="_3"></span>

## pVisor 在 Agent 系统中的位置 {#system-role}

pVisor 将面向 Agent 的管理能力接入沙箱执行体系，在 Host 侧监管文件与网络交互、协调沙箱生命周期，并优化镜像与内存资源。下图以 VM 执行器为例，展示 pVisor 在沙箱执行框架中的集成位置。

![上层 Agent、包含 pVisor 管理模块和运行时的 Host 沙箱执行框架、下层 guest VM](../assets/diagrams/pvisor/agent-host-guest-zh.svg)

沙箱运行时 / VMM 承担 VM 的运行与设备执行；pVisor 通过生命周期与资源管理接点协调运行时，通过 Host 侧的 [OverlayCore](../design/overlayfs.md)、[OverlayNet](../design/overlaynet.md) 管理 guest 的文件与网络 I/O。图中的模块关系表示需要相应运行时接点的集成，自动等待管理的支持范围见[实验记录](../design/experimental/vcpu-idle-offload/index.md)。Agent 或上层系统负责决策、结果接受、奖励计算及跨节点调度。

<span id="_2"></span>
<span id="_1"></span>
<span id="task-flow"></span>
<span id="when-to-use"></span>

## 使用场景 {#workflows}

**本地 Agent 工作流**

当你希望限制 Agent 对工作区的影响，并在执行后审查和接受文件改动时，从[第一次运行](../start/first-run.md)开始，完成一次执行、审查与合入。

**Agent 执行基础设施**

当你需要为推理、评测或后训练管理多个执行环境，并降低模型等待与分支探索中的资源占用时，从[基础设施总览](../guides/infrastructure/index.md)开始，选择接入方式与运行配置。

<span id="trust-ladder"></span>

!!! note "支持范围"
    文件改动隔离需启用[暂存](../concepts/staging.md)，且不撤销外部 API、数据库等副作用。隔离与资源管理能力以[执行器](../security/executor-boundaries.md)、[接口](../guides/infrastructure/integration.md#capabilities)及实验状态为准；性能收益见对应[基准测试](../benchmarks/index.md)。
