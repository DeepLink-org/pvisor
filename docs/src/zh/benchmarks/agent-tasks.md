# 修复与测试任务要多久，Agent CLI 能否完成？

## 主要结论 {#conclusions}

**pVisor staged 的短工具任务接近原生，且在已测完整修复任务中快于 Docker；pVisor VM 的工具执行慢于 Docker 和最小参考 VM。** 同工具环境修复/测试 P50 为 staged **0.70 s**、原生 **0.50 s**、Docker **0.90 s**；VM **3.97 s**、QEMU microvm **1.85 s**。需要暂存审查时，staged 的额外等待约为数百毫秒。

无镜像 VM 比完整 Ubuntu 的新环境更早返回短任务结果，但进入环境后的工具执行更慢。**Codex 的受控工具闭环通过；Claude 在 pVisor VM 初始化超时**，选型还需要核对客户端兼容性。

| 需求 | 选型含义 |
|---|---|
| 可信任务，需要暂存审查 | 优先评估 host staged |
| 需要独立 guest kernel | 预留 VM 的工具执行时间 |
| 使用 Claude Code / VM | 先核对指定版本兼容性 |

## Motivation {#motivation}

裸启动时间不能代表一次 Agent 修改、安装依赖、跑测试的总等待。固定工具计划和模型响应，可以先判断环境本身的开销及 CLI 是否正常工作，再考虑真实模型的质量与波动。

## 实验设计 {#interpretation}

使用真实 Claude Code 2.1.128 / Codex CLI 0.160.0，本地受控响应与假凭据，无模型推理。任务检查仓库、搜索、修复 Python，再运行 Python/Rust/Node 测试、离线安装 32 个 npm 包和生成 diff。要求实际测试结果回传模型服务、客户端完成、暂存原目录未改动。

Linux 同工具环境各格 3 次预热、30 次测量；完整 Ubuntu 各格 N=10、3 次预热；均两核预算、VM 2 vCPU / 16 GiB、热宿主缓存。工具版本与存储路径在完整 Ubuntu 对照中不同，两表独立呈现。任务计时从启动到校验结果，不含镜像/工具准备；worker 只计内部工具与校验。Codex 内层统一为 `danger-full-access`，不验证默认嵌套沙箱。对应 macOS 与真实模型任务尚未测量。

固定制品与测量日期按表注明。失败与校验不通过的样本不计入成功耗时，失败数量单列；既有数据没有事先的宿主干扰剔除规则，所有通过校验的慢样本保留。30 次及更少采样的 P95 仅为观察参考，不给 P99 或稳定尾延迟承诺。

## 实验数据和分析 {#results}

### 同工具环境：原生、Docker 与最小 VM {#reference-env}

| Backend | Tool self-check P50/P95 s | Repair/tests P50/P95 s | Claude loop P50/P95 s | Codex loop P50/P95 s |
|---|---|---|---|---|
| Native | 0.15 / 0.17 | 0.50 / 0.78 | 0.82 / 0.88 | 1.97 / 2.49 |
| pVisor host | 0.16 / 0.17 | 0.50 / 0.61 | 0.85 / 0.91 | 1.94 / 2.25 |
| pVisor staged | 0.17 / 0.19 | 0.70 / 1.10 | 1.07 / 1.27 | 2.25 / 2.44 |
| pVisor VM | 1.40 / 1.52 | 3.97 / 6.79 | FAILED / N=0 | 10.93 / 12.93 |
| Docker rootless | 0.46 / 0.53 | 0.90 / 1.45 | 1.23 / 1.33 | 6.26 / 6.59 |
| Firecracker PCI | 1.48 / 1.59 | 2.25 / 3.13 | 3.03 / 3.21 | 7.83 / 8.47 |
| QEMU q35 | 0.92 / 1.09 | 1.98 / 3.11 | 2.67 / 3.67 | 7.69 / 8.46 |
| QEMU microvm | 0.85 / 1.07 | 1.85 / 2.48 | 2.71 / 3.29 | 7.67 / 8.34 |

staged 比 Docker 的修复任务少约 **0.20 s**，但它与 Docker writable bind 的修改和隔离语义不同。VM 的修复任务约为 Docker **4.4 倍**，npm 安装是主要等待阶段，约 **1.74 s**；启动快没有消除工具路径成本。

Claude 在原生、staged、Docker 和三个参考 VM 上各 30/30 通过，pVisor VM 初始化超过 90 s，正式 N=0。Codex 八组各 30/30 通过。不同 CLI 的绝对耗时不代表模型速度；固定响应也不能证明真实模型成功率不变。

### 无镜像 VM 与完整 Ubuntu {#full-ubuntu}

| Backend | Tool self-check P50/P95 s | Repair/tests P50/P95 s | Claude loop P50/P95 s | Codex loop P50/P95 s |
|---|---|---|---|---|
| Native / Fedora | 0.16 / 0.17 | 0.52 / 0.55 | 0.92 / 1.07 | 2.03 / 3.12 |
| pVisor staged | 0.18 / 0.19 | 0.72 / 0.78 | 1.15 / 1.58 | 2.33 / 2.44 |
| pVisor VM / host | 1.21 / 1.77 | 4.61 / 5.09 | FAILED / N=0 | 11.39 / 13.73 |
| Firecracker / Ubuntu | 7.79 / 10.13 | 8.51 / 9.11 | 9.78 / 9.87 | 10.81 / 13.49 |
| QEMU q35 / Ubuntu | — | 8.12 / 8.23 | 9.50 / 10.78 | 10.20 / 11.63 |
| QEMU microvm / Ubuntu | — | 10.33 / 10.47 | 11.59 / 14.07 | 13.04 / 14.06 |

pVisor VM 修复任务从启动到结果约 **4.61 s**，Firecracker/Ubuntu **8.51 s**；只计内部工具则分别 **4.02/2.30 s**。减少开机等待有利于一次性短任务，长期复用环境时工具速度更重要。QEMU 两行是同一 Ubuntu 模板的独立 N=10 批次，不合并分布。原始报告保留阶段计时与内存范围。

### 数据范围 {#acceptance}

这些对照使用各自固定的 pVisor 制品，未随当前文件系统制品全部重测。大型仓库、真实推理、公网依赖、长期池化吞吐与 SWE-bench 成功率没有对应结果。[当前文件系统](filesystem.md)另给最新本地操作数据。

