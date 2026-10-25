# 沙箱生命周期管理

Agent 等待外部响应或数据时，CPU 可能空闲，保存任务上下文的内存却持续驻留。pVisor 在受支持的路径上由 Host 接管等待期间的 I/O，结合 guest 空闲状态管理暂停、内存卸载与恢复，使沙箱资源管理跟随 Agent 的任务进展。

<a id="agent-native"></a>

## Host 如何接管等待中的 I/O {#host-management}

在 VM 路径中，OverlayCore 和 OverlayNet 将文件与网络交互接到 Host，Runtime 负责 vCPU 与设备执行。Host 可以在 guest 暂停期间继续推进部分后端工作：

| 等待场景 | Host 可以继续推进什么 |
| --- | --- |
| **模型等待** | 等待关联本次执行的模型响应，并在支持的配置下有界收集响应数据。 |
| **网络下载** | 在受支持的授权下载路径上接收数据、维持背压，准备向 guest 交付。 |
| **Lazy images 文件读取** | 在受支持的只读路径上按需获取、校验文件内容，暂存在 Host 缓冲中。 |

三种场景提供不同的等待信号，共用下述 Pause、Offload、Resume 操作。暂停需要同时满足 guest 空闲、设备内存访问协调和唤醒条件，不能仅凭一个请求正在等待；数据就绪或其他唤醒事件到来时恢复 guest，继续执行工具逻辑。各路径的启用条件见[自动等待管理的设计与验证说明](../design/experimental/vcpu-idle-offload/index.md)。

## Pause、Offload 与 Resume 如何配合 {#live-lifecycle}

| 操作 | 作用 |
| --- | --- |
| **Pause：暂停执行** | 暂停 vCPU，通常仍保留 guest RAM 的驻留。 |
| **Offload：卸载内存** | 在执行与设备访问满足静默条件后，将 RAM 同步到后备存储并请求回收驻留页。 |
| **Resume：恢复执行** | 恢复执行与设备访问；已卸载页面可在后续访问时缺页读回。 |

根据等待情况，可以选择两条路径：

- **仅暂停执行：**运行 → Pause → Resume。
- **进一步回收内存：**运行 → 暂停并满足卸载条件 → Offload → Resume。

Offload 是可选步骤，是否卸载取决于等待长度与卸载、恢复成本。三个操作围绕同一个存活 VM 展开，依赖原 owner／VMM 和可靠的内存恢复来源，见[内存卸载机制](../design/memory-optimization/offload.md)。

<a id="checkpoint-boundary"></a>

Live Resume 继续原存活实例；从保存状态重建实例的语义见[VM 执行检查点](resource-reuse.md#execution-checkpoints)。

<a id="prerequisites"></a>
<a id="start-task"></a>
<a id="pause-resume"></a>
<a id="offload-resume"></a>
<a id="collect-results"></a>

## 进一步了解 {#cli-example}

- [暂停、卸载与恢复 VM](../guides/infrastructure/lifecycle.md)：CLI 操作示例。
- <a id="availability"></a>[接入方式与能力矩阵](../guides/infrastructure/integration.md#capabilities)：各接口支持的生命周期操作。
- [镜像与内存优化](resource-optimization.md#lifecycle-and-cost)：生命周期管理与资源优化如何协同。
