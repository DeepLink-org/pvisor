# 完整修复任务要多久，Agent CLI 能否正常完成？

## 主要结论 {#conclusions}

**所测 rootless host/staged 修复与测试等待短于 pVisor VM；较快的 VM 启动不能消除工具成本。固定版本 CLI 测试中 Codex 受控闭环通过，Claude 在所测 pVisor VM 配置中初始化失败。**

| 需求 | 选型含义 |
|---|---|
| 本机工具与保留改动 | 评估 rootless host/staged |
| 需要独立 guest 内核 | 预算完整 VM 工具时间 |
| 已有容器/Git 工作流 | 比较成本与审查语义 |

## Motivation {#motivation}

Agent 启动后还要改文件和运行测试。固定修复计划隔离环境成本；真实客户端闭环另外检查兼容性，再考虑真实模型波动。

## 实验设计 {#interpretation}

共享 Linux/x86_64 宿主，AMD Ryzen 7 9700X，Fedora 内核 7.2.8-200.fc44.x86_64。启动进程树及专用 Docker daemon 固定到宿主 CPU 0,1；guest 为 2 vCPU。host/staged 使用 rootless_process。Shell VM 为 128 MiB，工具 VM 为 16 GiB。原生/Docker 不限内存：控制 CPU 与 guest 配置内存，不是相同资源限制的对照。工具与输入已准备，每次新建工作区、热缓存，3 次预热、60 次正式采样；按固定种子随机交错后端。构建、下载及输入复制不计时。

Docker Engine 29.7.2 使用专用 rootless VFS daemon 与可写 bind mount，结果不代表 overlay2 或 Docker Desktop。Firecracker 1.13.1 PCI 不使用 jailer；QEMU 10.2.2 分别使用 q35/microvm、私有 ext4。pVisor VM 使用 virtio-fs 和不同内核。内核、存储、设备及暂存语义均有差异，不能把差距单独归因于 VMM 或 FUSE。

计划检查/搜索仓库、修复 Python、运行 Python/Rust/Node 测试、安装 32 个离线 npm 包并生成 diff。Result 截止到校验后的结果返回，Completion 含进程退出；测试与预期改动须通过。真实模型成功率、大型仓库及持久池吞吐未测。

## 实验数据和分析 {#results}

### 固定修复与测试计划 {#reference-env}

测于 2026-10-06，每后端 60/60 有效、正式失败 0。检查输出与退出；暂存模式额外检查原文件未修改、改动完整保留。通过校验的慢样本全部保留，不按耗时剔除。P95 仅为观察参考。按[比较方法](methodology.md)中的预定规则识别出分离簇时，展示各簇中位数及占 60 次的数量，替代单个 P50。

| Backend | Valid / failed | Result P50 s | Completion P50 s | Completion P95 s |
|---|---|---|---|---|
| Native | 60 / 0 | 0.45 | 0.45 | 0.47 |
| pVisor host | 60 / 0 | 0.46 | 0.47 | 0.56 |
| pVisor staged | 60 / 0 | 0.57 | 0.68 | 0.73 |
| pVisor VM | 60 / 0 | 3.11 | 3.25 | 4.14 |
| Docker rootless / VFS | 60 / 0 | 4.03 | 5.15 | 6.38 |
| Firecracker PCI | 60 / 0 | 2.01 | 2.06 | 3.00 |
| QEMU q35 | 60 / 0 | 1.29 | 1.33 | 1.65 |
| QEMU microvm | 60 / 0 | 1.23 | 1.27 | 1.77 |

完整完成包含工具执行与退出，Docker VFS 创建计入总时间。单项工具见[文件系统对照](filesystem.md)。

### 真实 CLI 兼容性 {#cli-compatibility}

独立的 2026-10-04 样本组：Claude Code 2.1.128 / Codex CLI 0.160.0，可用组各 N=30、3 次预热、两核 / 16 GiB。单位为启动到结果 P50 秒。本地确定性响应与假凭据排除模型推理。Codex 使用 `danger-full-access`，不是默认嵌套 sandbox 行为。

| Backend | Claude loop P50 s | Codex loop P50 s |
|---|---|---|
| Native | 0.82 | 1.97 |
| pVisor host | 0.85 | 1.94 |
| pVisor staged | 1.07 | 2.25 |
| pVisor VM | FAILED / N=0 | 10.93 |
| Docker rootless | 1.23 | 6.26 |
| Firecracker PCI | 3.03 | 7.83 |
| QEMU q35 | 2.67 | 7.69 |
| QEMU microvm | 2.71 | 7.67 |

Codex 八组均通过，各 30/30。Claude 可用组均 30/30，但 pVisor VM 预检超过 90 秒初始化期限，无正式耗时样本。这是固定版本观察，不代表所有新版客户端。

### 完整 Ubuntu 部署 {#full-ubuntu}

独立的 2026-10-04 数据，两核 / 16 GiB、N=10、3 次预热。OS 初始化/工具/存储不同，单位为启动到结果 P50 秒，不进行纯 VMM 排名。

| Deployment | Repair result P50 s |
|---|---|
| pVisor VM / host tools | 4.61 |
| Firecracker / Ubuntu | 8.51 |
| QEMU q35 / Ubuntu | 8.12 |
| QEMU microvm / Ubuntu | 10.33 |

### 数据下载与复现 {#run}

[整理后的表格 CSV](agent-tasks.csv) · [运行时统计](runtime-summary.csv) · [来源与制品](runtime-provenance.csv) · [证据来源摘要](evidence-sources.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
