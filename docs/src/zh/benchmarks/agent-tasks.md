# Agent 工具闭环：真实 CLI、受控响应

完整 Python/Node/Rust 环境的修复测试任务，中位耗时为原生 **0.50 秒**、pVisor staged **0.70 秒**、Docker **0.90 秒**、pVisor VM **3.97 秒**。暂存增加约 0.20 秒；VM 仍明显落后于容器与参考 VM。真实 Codex CLI 闭环通过，Claude Code 在本轮 pVisor VM 制品上初始化超时。

## Motivation

想知道 pVisor 是否干扰 Agent，需要先固定工具动作，排除模型和公网波动，再测真实模型任务。第一步给出可重复的工具闭环基线。

## 实验设计 {#interpretation}

固定客户端版本、项目输入与本地模型响应，比较原生、暂存、容器和 VM 的环境及工具成本。要求文件修复正确、测试零退出、实际工具结果回传、客户端完成；暂存还要求原目录保留错误版本。使用假凭据，无模型推理或付费调用。完整环境为每格 30 次、3 次预热、随机顺序；历史算术批次为 6 个输入、每任务 3 次、无预热、固定顺序。分别报告，不合并分布。

## macOS

本轮在 Linux 实测，以下工作负载没有 macOS 样本；macFUSE/FSKit 开销与并发容量均未测。既有 macOS/HVF 数据继续保留在 [VM 启动时间](startup.md)与[VM 内存报告](vm-memory/index.md)，不移作本页结果。

## Linux：2026-10-04 {#results}

### 完整 Agent Env：同机、同工具、熟悉基线 {#reference-env}

这里部署完整环境，再运行真实客户端：Python 3.14.7、Node 24.18.0/npm、Rust/Cargo 1.98.1、Git、rg、Claude Code 2.1.128、Codex 0.160.0。环境约 3.58 GiB、7.6 万文件；原生、Docker 与 VM 使用相同工具制品、项目与校验。每格 30 次、3 次预热，共两核预算，VM 为 2 vCPU / 16 GiB。完整[参数与边界](methodology.md#reference-env)单独列出。

任务从检查项目、搜索错误、修复 Python 开始，再执行 Python/Rust/Node 测试、安装 32 个本地依赖、生成 diff。模型响应固定为这一工具计划；CLI 必须把真实通过的测试结果传回服务并正常结束。它覆盖环境部署与工具执行，尚不能代表大型仓库、真实模型解题成功率或公网依赖安装。

![完整工具环境与真实 CLI 的 P50/P95](../../assets/benchmarks/reference-env-20261004/reference-workflows.svg)

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


版本自检只说明工具可以启动；修复列计入环境启动、修复、测试和结果校验。CLI 列再计入客户端初始化、本地协议往返与工具回传。表中终点是宿主收到校验通过的结果，随后还检查进程零退出、Run Bundle 与文件；退出/卸载耗时另存，避免用清理时间冒充工具时间。图中条形为 P50、横线为 P95，失败没有延迟样本。各阶段的中位数不能相加得到总中位数。

**暂存路径已接近可交互工具预算，VM 路径仍有显著优化空间。** staged 修复任务比原生多约 0.20 秒，比 Docker 少约 0.20 秒；它提供独立改动视图与 Run Bundle，Docker 对照使用可写挂载，工作流并不完全相同。pVisor VM 的 3.97 秒约为 Docker 的 4.4 倍、QEMU microvm 的 2.1 倍：约 86 ms 的启动成绩，不能推出任务也同样快。

| Phase | Native ms | Docker ms | pVisor staged ms | pVisor VM ms | QEMU microvm ms |
|---|---|---|---|---|---|
| inspect | 2.9 | 4.2 | 18.4 | 60.3 | 20.4 |
| search | 2.3 | 2.3 | 2.4 | 18.9 | 8.6 |
| python-tests | 25.5 | 53.3 | 31.9 | 118.3 | 57.6 |
| rust-tests | 99.5 | 99.1 | 213.2 | 835.8 | 533.8 |
| node-install | 183.6 | 230.3 | 219.1 | 1736.6 | 603.6 |
| node-tests | 135.8 | 125.4 | 145.7 | 466.3 | 167.8 |
| diff | 1.0 | 0.9 | 2.9 | 16.2 | 1.1 |


VM 的 npm 阶段约 1.74 秒，是本任务中最大的耗时阶段；Rust 编译约 0.84 秒。配合[文件操作对照](filesystem.md#reference-fs)，密集小文件与离线安装是下一步应优先分析的路径。不同 VM 文件系统、guest 配置与初始化方式都有影响，表格定位耗时，不证明唯一原因。

**兼容性也是结果。** Claude 在 native/staged/Docker/Firecracker/QEMU 上各 30/30 通过；pVisor VM 前置检查在初始化阶段超过 90 秒，未收到工具闭环结果，正式 N=0。日志和诊断保留，根因尚未定位。Codex 在全部八组各 30/30 通过，但采用统一的 `danger-full-access` 内层模式，由外层运行时提供所声明的边界；这不证明默认 `workspace-write` 沙箱在所有组中可用。原生和 staged 仍能访问宿主视图外路径。

Claude 与 Codex 的绝对时间不宜互相排名。Codex 还存在约秒级的客户端等待路径，Docker 约 6.26 秒、pVisor VM 约 10.93 秒；这比裸启动数字更贴近本轮完整客户端体验。差异属于这些固定版本与受控任务，不能直接推广到模型速度。

**部署成本另列。** 从已安装工具离线复制、导入镜像、制作 ext4 环境约 109 秒，之后增加 CPU affinity helper 约 5.3 秒；不含工具下载和内核编译。每次的工作区/私有磁盘准备时间记录在 `prepare_ms`，不计入表中任务耗时。这些是已准备环境的短任务预算，不是首次安装到结束的耗时。内存审计见[方法](methodology.md#reference-resources)，不由配置的 16 GiB 推导每个 Agent 实占 16 GiB。

[逐样本 CSV](../../assets/benchmarks/reference-env-20261004/samples.csv) · [分布与阶段计时](../../assets/benchmarks/reference-env-20261004/summary.json) · [运行证据](../../assets/benchmarks/reference-env-20261004/evidence.tar.gz) · [兼容性矩阵](../../assets/benchmarks/reference-env-20261004/compatibility.json)

### 首版受控算术任务：独立历史批次

以下 72/72 是更小任务、另一 GNU 制品与固定顺序的旧批次，与完整环境的统计分别保留。

| CLI | Backend | Passed/planned | Wall P50/P95/P99 ms | P50 vs native |
|---|---|---|---|---|
| claude | native | 18/18 | 302.15 / 318.58 / 340.44 | +0.0% |
| claude | staged | 18/18 | 345.69 / 365.95 / 366.29 | +14.4% |
| codex | native | 18/18 | 1433.75 / 1543.04 / 1546.18 | +0.0% |
| codex | staged | 18/18 | 1401.23 / 1521.71 / 1521.86 | -2.3% |

### 分析

每次检查修复后的文件、测试结果回到了下一次模型请求、CLI 正常结束；staged 原目录仍是错误版本。按 CLI 分别比较 native 与 staged，不能把两个不同客户端的启动时间解释成模型速度差。Codex staged P50 为 -2.3%，但本轮 native/staged 顺序固定、共享桌面有后台负载，不能据此宣称 pVisor 加速，也没有对等性置信区间。输入与模型服务随归档脚本公开。

控制实验没有成功率置信区间，也不能据 100% 通过宣称 SWE-bench 性能不变。它只支持本机这两个版本、Bash/exec_command 与该修复路径。
## 边界与下一轮 {#acceptance}

真实模型 SWE-bench Lite 子集、真实 tokens、复杂多工具任务和模型输出波动未测；未授权使用付费账户。后续固定 task IDs、仓库 commit、模型、预算、工具/网络权限，随机 native/staged 顺序并保留所有失败与原始轨迹。第一版不编造这部分数字。

## 复现与证据 {#run}

从仓库根目录运行；输出目录必须是新目录。动态 firmware 入口需要 GNU/Linux 版 CLI；静态 musl 制品使用另一套 firmware 入口，不能混用。Linux、KVM/FUSE/user namespaces、Python 3.14、Rust/GCC、Git/rg、Node 24/npm、Podman/crun 是本机工具环境；Agent 套件另需 Claude/Codex CLI。

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --suites agent --samples 3 --warmups 0
```

先用 `--samples 1 --warmups 0` 检查环境。脚本、输入定义和正确性断言在 `benchmark/pvisor/v1/`；报告会复制二进制、firmware 和当轮脚本并记录摘要。失败不会进入性能分布；准备、策略拒绝和工作负载错误分别保留。每个页面写明有效样本数；小样本 P95/P99 只描述本批次，不估计长期尾延迟。

[环境、制品与采样方法](methodology.md#product-v1) · [批次清单](../../assets/benchmarks/product-v1-20261004/manifest.json) · [逐样本汇总 CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [原始报告与日志归档](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz)。报告保留源码的未提交状态；当轮可执行制品 SHA256 是身份依据。归档不含数 GB 的 rootfs、二进制和可重建工作区；输入摘要及每轮脚本保留。
