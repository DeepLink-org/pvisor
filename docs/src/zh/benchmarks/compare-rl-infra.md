# pVisor 能降低强化学习 rollout 的哪些成本？

## 主要结论 {#conclusions}

**对于本机工具密集型尝试，stage 的实测修复成本更低、通过验证的活跃容量高于 pVisor VM；需要独立 guest 内核时选择 VM。前缀准备和镜像按需读取分别覆盖其他 rollout 成本，尚无训练吞吐、成功率或 reward 提升的证据。**

| 场景 | 选型含义 |
|---|---|
| 大量短仓库任务 | 评估 stage 的改动保留与活跃容量；其边界是宿主进程 |
| 每次尝试需要独立 guest 内核 | 即使启动等待短，也要预算 VM 工具成本与较低的实测容量 |
| 历史上下文或未缓存镜像 | 在已测范围内评估前缀准备或按需读取 |
| 已有 RL 栈 | 保留任务、模型和训练层；专用集成仍未验证 |

## Motivation {#motivation}

一次 rollout 创建环境，交替进行模型推理与工具调用，执行验证、赋予 reward，再保留或拒绝样本。失败尝试可能需要准备上下文、恢复环境并重新执行。选择 executor 时，需要分别预算这些阶段，并保留任务要求的执行边界。

## 实验设计 {#interpretation}

场景分析复用已登记的本机 Linux/x86_64 实验，不新增测量。所属主题定义制品、校验与控制条件：

- [活跃容量](density.md)：共享 2 CPU / 2 GiB / 零 swap，每格五个新批次；Python/Git 任务触碰 32 MiB 私有数据，到共同 barrier 后释放；失败保留在容量分母中。
- [修复任务](agent-tasks.md)：预备离线工具、热缓存、三次预热，每个后端 60 次随机交替的固定工具计划；completion 包含启动与退出，排除推理和准备。CPU 亲和性受控，但宿主/容器内存无上限，工具 VM 配置 16 GiB。
- [回放](replay-fidelity.md)：每个适配器二十条合成短前缀，三次打乱顺序重复，无预热或缓存清除；要求历史参数/观测准确、工具执行为零且工作区不变。
- [启动](startup.md)与[按需镜像](lazy-image-startup.md)：已准备 shell 与冷/热客户端分别成批；lazy 服务使用本机 loopback，宿主页缓存保持热。[隔离](isolation-tests.md)检查最终宿主/stage 状态；[VM 内存](vm-memory/index.md)使用独立 N=1 静态读数。

输出无效或执行失败不进入成功耗时，所有有效慢样本保留；保留样本数与分离时间簇。各批次的内存、缓存和负载控制不同，不能合并、相加中位数或据此形成完整 RL 成本排名。

| 组件 | 互补职责与限制 |
|---|---|
| [OpenHands runtime](https://docs.openhands.dev/openhands/usage/sandboxes/docker) | Agent 工具环境，包括 Docker sandbox；回放格式校验不验证 runtime 集成 |
| [SWE-Gym](https://github.com/SWE-Gym/SWE-Gym) | 仓库任务、可执行环境、验证及 Agent/verifier 训练；没有同条件训练对照 |
| [verl](https://verl.readthedocs.io/en/latest/) | 训练、rollout 与模型资源协调；没有已验证的 pVisor 训练集成 |
| pVisor | 每次尝试的执行、暂存文件与记录；模型推理、GPU 分配、reward 设计和集群调度由外部负责 |

## 实验数据和分析 {#results}

以下观测支持环境选型，不能换算为每秒有效训练样本。

| rollout 成本 / 检查 | 当前证据与统计量 | 决策范围 |
|---|---|---|
| 活跃工具容量，2026-10-06 | Stage 32：5/5 批次、160/160 任务；Podman 32：5/5、160/160；VM 16：5/5、80/80；VM 32：3/5、138/160 | 共享 2 CPU / 2 GiB；空闲数量不能证明活跃容量 |
| 固定修复到退出，2026-10-06 | Completion P50：staged 0.64 s、Docker 0.81 s、VM 3.25 s、QEMU microvm 1.40 s；各 60/60 有效 | 已准备工具计划；内核、文件系统和缓存不同，不能作纯 VMM 排名 |
| 已准备 shell 启动 | Ready P50：staged 25.13 ms、VM 100.91 ms；各 N=60 | 首次输出不包含后续工具工作；与修复和 lazy 批次分开 |
| 前缀准备，2026-10-06 | 七种格式各 60/60；六种的低簇中位数为 6.07–6.59 ms、高簇为 11.15–16.24 ms；Mini-SWE-Agent 未分簇 P50 为 6.75 ms | 不重新执行工具、不启动 Agent、不写工作区 |
| Lazy 冷客户端 shell，2026-10-07 | Ready P50 183.7 ms；内容 2,580,417 字节（2.58 MB），Docker 完整 OCI 内容为 41,842,292 字节（41.84 MB）；各 N=30 | 服务在本机预备；交付格式不同，未测真实 WAN 或训练负载 |
| 工作区边界，2026-10-06 | Staged/safe/VM 保留写入并阻止已测视图外访问，各 3/3；已测 OCI/Podman 可写挂载直接写宿主 | 路径/socket 夹具，不是全面逃逸审计或远端副作用回滚 |

[修复对照](agent-tasks.md#reference-env)报告 staged 减 Docker 的 completion 中位数差为 −175.84 ms（95% CI −178.85 至 −174.55 ms），VM 减 microvm 为 +1849.00 ms（1834.43 至 1855.78 ms）。Stage 的优势伴随宿主进程边界；VM 提供 guest 内核，但该工具计划成本更高，完全成功的活跃批次容量更低。两者都不能证明模型质量或普遍 rollout 速度。

真实客户端兼容性是独立批次：[VM 中 Codex](agent-tasks.md#cli-compatibility)完成本地受控响应工具闭环，P50 9.01 s、N=30。Claude Code VM 在预检中达到 90 s 初始化期限，有效样本 N=0，没有可用耗时估计；固定工具计划不能证明所有 Agent 均兼容。

Lazy 冷客户端 Ready 排除镜像服务首次准备；镜像已缓存时，Docker 通常更快。内容字节不包含完整网络流量，也不能预测大型仓库任务的传输量。[VM 内存选项](vm-memory/index.md)可辅助预算等待模型的阶段，但 offload 需要恢复，压缩收益取决于内容；N=1 静态读数与快照大小不能证明停驻或活跃 rollout 容量。

原生 Agent resume 负责客户端会话/上下文。Prepare-only 提供经过校验的历史前缀，实际文件保持不变，任务环境需另行恢复。VM checkpoint/fork 负责捕获的执行状态及关联资源，受生命周期与 backing 前提约束。没有同条件原生 resume 速度排名、完整远端连接恢复或远端 API 副作用回滚的证据；工具重新执行需单独校验副作用与正确性。

训练吞吐、有效样本成功率、reward、重试和端到端资源成本仍未测。OpenHands、SWE-Gym 与 verl 集成需要独立验收，适配器前缀保真不足以证明集成成立。

### 数据下载与复现 {#run}

[场景证据 CSV](compare-rl-infra.csv)保留主题、来源、批次、统计量、样本数、数值与限制。来源摘要：[密度](density-summary.csv)、[修复/CLI](agent-tasks.csv)、[启动](startup.csv)、[回放](replay-fidelity.csv)、[lazy 启动/内容](lazy-startup-summary.csv)、[隔离](isolation-tests.csv)、[VM 内存](vm-memory/memory-choices.csv)。制品摘要与完整控制条件保留在所属主题的 provenance 下载中；原始证据保留在本地 `.data/`。

[比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
