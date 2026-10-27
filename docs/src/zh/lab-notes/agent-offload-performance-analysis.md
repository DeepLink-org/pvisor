# Agent 等待期间的内存卸载与恢复

这组工程实验回答：Agent 等待模型响应时，暂停和卸载 VM 内存能减少多少资源占用，又会增加多少执行成本？实现来自 [PR #33](https://github.com/DeepLink-org/pvisor/pull/33)，数据来自冻结版本的 SWE 轨迹回放，不代表当前版本或生产集群的性能承诺。

## 当前问题：CPU 空闲，内存仍然驻留 {#motivation}

Agent 的模型请求与工具执行交替发生。等待外部响应时，VM 仍保留工具进程、文件缓存和执行上下文，因此较低的 CPU 使用率不意味着较低的内存占用。并发增加时，内存可能先于 CPU 成为容量约束。Overcommit 能提高资源分配的灵活性，但不会自动识别 Agent 的等待窗口，也不能替代驻留内存回收。

以下保留三组 SWE 任务的初步观察；组别对应的任务、样本数和聚合口径待补齐。这些读数不与后面的固定轨迹回放合并，也不用于推导通用瓶颈结论。API 等待包含服务排队、模型生成和网络时间，并非纯模型推理时间。

| 指标 | Group 1 | Group 2 | Group 3 |
| --- | ---: | ---: | ---: |
| API 等待占 Agent 执行时间 | 76.00% | 69.00% | 44.80% |
| API 等待期间 VM cgroup 平均 CPU（核） | 0.235 | 0.267 | 0.295 |
| API 等待期间 VM cgroup 平均内存（MiB） | 763 | 591 | 2252 |

## 三种等待场景与 Host 接管 {#wait-scenarios}

OverlayNet 与 Gateway 提供网络、模型请求的等待线索，OverlayCore 对接的 virtio-fs 路径提供文件后端读取线索。Host 可以独立推进这些 I/O；但请求尚未完成只是线索，暂停还必须取得 guest 空闲证据并通过设备访问检查。

| 场景 | Host 如何识别 | 暂停期间继续的工作 | 恢复时机 |
| --- | --- | --- | --- |
| 模型等待 | 绑定本次执行的 Gateway 请求尚未交付 | 等待上游响应；按配置有界缓冲流式字节 | 数据可交付、批次达到大小或时间限制、EOF 或错误 |
| 下载等待 | 已授权 GET 等待 headers／数据块，或 CONNECT 等待下行数据 | 接收数据、准备转发并维持背压 | 下一块数据就绪、EOF 或错误 |
| Lazy images 读取 | virtio-fs READ 或受支持的只读 OPEN 等待 Host 后端 | 获取、校验内容并暂存于 Host 缓冲 | 后端完成后先恢复 VM，再写入 guest 响应 |

三条路径向同一个 WaitCoordinator 提供等待开始和数据就绪事件。下载中的 guest 写入、文件写入与 copy-up 不自动满足卸载条件。下述性能实验只启用模型等待管理，未测量下载、lazy images 或流式缓冲的综合收益。

## Pause、Offload 与 Resume {#operations}

| 操作 | 作用 | 边界 |
| --- | --- | --- |
| Pause | 暂停 vCPU | 通常不回收驻留内存，也不代表设备已停止访问 RAM |
| Offload | 同步 RAM 到后备文件并请求回收驻留页 | 先暂停，并阻止、排空相关设备 RAM 访问 |
| Resume | 恢复原 VM 的执行与设备访问 | 后续访问可能触发缺页回读；不是从检查点创建新 VM |

共同路径为：Host 识别等待 → guest 交接空闲状态 → 暂停 → 条件允许时卸载 → 数据就绪或真实事件到达 → 恢复并交付结果。短等待可只暂停；卸载还需要足够的 timer 预算和冷却间隔。Host 继续 I/O 并不允许设备在 RAM 访问被隔离时直接写 guest 内存。

文件级管理提供的是识别和接管部分后端 I/O 的操作边界；RAM backing 是保存 guest 内存内容的后备文件，两者不是同一种文件状态。机制细节见[内存卸载](../design/memory-optimization/offload.md)。

## Host 与 guest 内核的配合 {#kernel-support}

控制策略在 Host，实际空闲事实由 guest 内核提供，无需 Agent 增加特殊 header 或改变模型消息格式。这里记录 PR 的 Linux x86_64/KVM、单 vCPU 路径；它是显式配置、默认关闭的实验能力。

| 位置 | 实现作用 | 保留的语义与限制 |
| --- | --- | --- |
| Host 协调器 | 等待生命周期、多轮检测、冷却、恢复前预登记与交付等待 | 尊重人工暂停所有权，检查取消与旧 epoch |
| VMM 与 virtio-fs | RAM gate；长 READ 释放 RAM lease，由 Host 暂存结果 | 完成后重新获取访问权；仅覆盖受支持的只读路径 |
| 0050 空闲交接 | 报告实际 clockevent，区分有 timer 与无 timer | 真实中断和 timer 仍能唤醒 |
| 0051／0054 fair server | 修复空闲停止与 timer 回调再次排队 | 保留调度预算、时间记账和正常唤醒 |
| 0052／0053 PSI | 限制无压力空闲后的无效重启，安全时延后聚合 | 保存压力记账与触发器，按原时间补算；受限单核条件 |
| 0055 网络 watchdog | 延后确认无未完成发送时的空检查，交接结束恢复到期时间 | 不关闭网卡、不丢包、不延后 TCP 或应用 timer |

[PR #33 的实现与验证说明](https://github.com/DeepLink-org/pvisor/pull/33)保留代码与内核修改的具体边界；这些机制并不意味着三种等待场景已经完成同等性能验证。

## SWE 轨迹回放：配置与结果 {#results}

### 实验配置 {#configuration}

两组实验均使用同一条 `qutebrowser-667` 轨迹：固定 44 次模型回复和合计 232.749 秒的等待，工具命令仍在真实 VM 中执行，不调用真实大模型。校验命令、返回码、最终 patch 与原轨迹一致，并运行任务验证。回放通过不代表新的模型解题率结果。

| 配置 | 单 VM | 16 VM 并发 |
| --- | --- | --- |
| Host | gpu02，Linux/KVM | 同一台 Host |
| 每台 VM | 1 vCPU、7 GiB guest RAM | 相同 |
| 每台服务预算 | 一核 CPU 配额、9 GiB 内存上限、禁止 swap | 相同；父 cgroup 64 GiB，禁止 swap |
| 对照与样本 | baseline／pause 各 1 次，后续 SSD offload 3 次 | baseline／pause／offload 各 1 批，每批 16 台 |
| 启动与缓存 | 新 VM、私有写层；复用只读基础文件，不清整机缓存 | 全部准备并启动观测器后统一放行；独立写层与 Gateway |
| 存储 | baseline／pause RAM backing 在 HDD；offload 在 SSD | 相同；工作区、overlay 与日志均仍在 HDD |
| 二进制摘要前缀 | `cd7a875a7202` | `430fa7bde1e58`，控制与日志修复版本 |

模型请求等待约 20 ms 后尝试空闲交接并暂停；约 50 ms 后可尝试卸载，但要求真实 timer 还有至少 1 秒预算，且满足 5 秒冷却。响应交付、真实 timer、外部事件或故障保护可触发恢复；同一请求可有多轮交接。下载、文件读取触发和流式缓冲关闭。

16 VM 服务共享 Host CPU 16–31，每台限制一核配额，并非独占绑核。Host Agent／回放进程使用 0–7、10–15，BPF 使用 14–15；避开其他评测使用的 8、9、40、41，但磁盘和内存带宽仍可能受共享负载影响。

内存统计覆盖 VM 服务 cgroup，另外测量 guest RAM 映射的 PSS；不含 Host Agent、回放服务器和 BPF。内存×时间是占用曲线的积分，表示整个执行窗口的累计资源成本，不是峰值或瞬时占用。响应交付延迟也不等于单独的 Resume 指令耗时。

### 单 VM：暂停与卸载的区别 {#single-vm}

下表 baseline／pause 来自后续诊断对照，各 N=1；SSD offload 为另一个批次的三次运行，各指标分别取中位数，括号为最小—最大值。不是三组都重复三次，也不是同一轮随机交替对照。

| 指标 | 关闭管理 | 仅暂停 | SSD offload，中位数（范围） |
| --- | ---: | ---: | ---: |
| Agent 总耗时（s） | 286.2 | 286.5 | 303.8（299.1–304.1） |
| Guest RAM PSS×时间（GiB·s） | 115.2 | 114.9 | 24.3（21.5–25.9） |
| VM 服务内存×时间（GiB·s） | 146.0 | 147.6 | 63.2（62.3–97.4） |

仅暂停时内存积分接近关闭管理；SSD 卸载组的内存积分较低，但耗时更长，服务内存积分也有明显波动。由于样本少、批次与存储介质不同，表格只并列呈现观察值，不把比例差异作为隔离出的因果收益或稳定性能承诺。

### 16 VM：内存占用与完成窗口 {#concurrent-vms}

以下为 10 月 22 日修复日志阻塞后的批次，每种策略仅一批，48/48 次回放功能校验通过。完成窗口从统一放行到最后一项 Agent 任务结束；吞吐为 16÷窗口分钟数，不含后续独立验证阶段。它不是平均单任务时延。

| 指标 | 关闭管理 | 仅暂停 | SSD offload |
| --- | ---: | ---: | ---: |
| 整批 Agent 完成窗口（s） | 381.50 | 443.23 | 447.36 |
| Agent 吞吐（任务/min） | 2.52 | 2.17 | 2.15 |
| VM 服务平均总内存（GiB） | 8.89 | 9.40 | 4.88 |
| VM 服务总内存采样峰值（GiB） | 14.68 | 16.02 | 11.77 |
| 服务内存×时间／任务（GiB·s） | 211.89 | 260.30 | 136.45 |
| Guest RAM PSS×时间／任务（GiB·s） | 162.68 | 171.67 | 57.04 |

卸载组记录的平均内存和内存积分更低，同时完成窗口更长、吞吐更低。64 GiB 上限未形成容量瓶颈，没有测出最大可承载 VM 数，因此不能据此宣称密度或吞吐提升。SSD 与 HDD 配置差异也包含在结果中。

**功能通过与测量完整性需要分开：原始 `validation.json` 的 `measurement_integrity_passed` 为 false。** baseline／pause 的 BPF 分别丢失 36,185／51,169 个事件；记录的最大单 VM 资源采样间隙分别为 4.24／21.65 秒，offload 为 0.85 秒。BPF 诊断不能用于完整归因，采样峰值与 PSS 积分也需要结合缺口解释；cgroup 服务计费与 guest PSS 并非同一指标。这些历史读数保留供复核，不作为已完成测量验收的性能基线。

### 来源与后续验证 {#sources}

实验对应已登记的 SWE resident 工程对照；[注册说明](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/REGISTRY.md#b-swe-resident-eng)与 [PR 冻结版本的运行说明](https://github.com/DeepLink-org/pvisor/blob/7b9110b7a66ec8e2b7b1b97414ea42fce3bb61ca/benchmark/pvisor/sweeval/README.md)提供背景。下列原始文件保留在本地 outputs 归档，不随网站发布，也未被修改：

- `pvisor-swe-reload-diag-20261021/cohort/summary.json`：表中单 VM baseline／pause；最初 `pvisor-swe-replay-20261021` 是另一批，不混入此表。
- `pvisor-swe-ssd-offload-20261021/cohort/receipt.json` 与 `summary.json`：SSD 三次对照。
- `pvisor-swe-concurrent-fixed-20261022/experiment-plan.json`、`summary.csv`、`summary.json` 与 `validation.json`：16 VM 配置、统计和测量限制。

后续需要补齐三组初步观察的来源，并在相同存储条件下重复交替采样、消除观测缺口，再测试内存受限的并发容量。下载与 lazy images 的性能需要独立实验，不能由模型等待回放代替。
