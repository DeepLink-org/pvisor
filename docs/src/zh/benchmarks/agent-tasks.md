# Agent 工具闭环：真实 CLI、受控响应

无镜像 pVisor VM 的完整修复测试任务约 **4.61 秒**，Firecracker / 完整 Ubuntu 约 **8.51 秒**；staged 约 **0.72 秒**。Ubuntu 的开机等待影响短任务，pVisor 的工具执行仍有明显成本；启动快不等于工具路径领先。Claude/VM：初始化超时 / N=0，Codex 的真实工具闭环结果见下表。

## Motivation

想知道 pVisor 是否干扰 Agent，需要先固定工具动作，排除模型和公网波动，再测真实模型任务。第一步给出可重复的工具闭环基线。

## 实验设计 {#interpretation}

固定客户端版本、项目输入与本地模型响应，比较原生、暂存、容器和 VM 的环境及工具成本。要求文件修复正确、测试零退出、实际工具结果回传、客户端完成；暂存还要求原目录保留错误版本。使用假凭据，无模型推理或付费调用。完整 Ubuntu 对照每个可用格 10 次；历史同工具环境每格 30 次，均 3 次预热、随机顺序；历史算术批次为 6 个输入、每任务 3 次、无预热、固定顺序。分别报告，不合并分布。

## macOS

本轮在 Linux 实测，以下工作负载没有 macOS 样本；macFUSE/FSKit 开销与并发容量均未测。既有 macOS/HVF 数据继续保留在 [VM 启动时间](startup.md)与[VM 内存报告](vm-memory/index.md)，不移作本页结果。

## Linux：2026-10-04 {#results}

### 无镜像 pVisor 与完整 Ubuntu：完整 Agent Env {#full-ubuntu}

pVisor 复用宿主已安装的 Python/Node/Rust/Git/rg/Claude/Codex，不制作系统镜像；Firecracker/QEMU 在完整官方 Ubuntu 26.04.1 LTS 中安装发行版工具及相同 Rust/Agent CLI。每次新建环境，再执行同一修复计划与测试。VM 2 vCPU / 16 GiB，所有组相同两核预算、热宿主缓存；每格计划 10 次、3 次预热，随机顺序；有效 N 不足 10 时在表内标明。Ubuntu 的 Python/Node/Git 版本与宿主不同，完整版本和内核差异见[方法](methodology.md#full-ubuntu)。

![Complete Ubuntu and image-free pVisor tool loops](../../assets/benchmarks/full-ubuntu-qemu-20261004/ubuntu-workflows.svg)

| Backend | Tool self-check P50/P95 s | Repair/tests P50/P95 s | Claude loop P50/P95 s | Codex loop P50/P95 s |
|---|---|---|---|---|
| Native / Fedora | 0.16 / 0.17 | 0.52 / 0.55 | 0.92 / 1.07 | 2.03 / 3.12 |
| pVisor staged | 0.18 / 0.19 | 0.72 / 0.78 | 1.15 / 1.58 | 2.33 / 2.44 |
| pVisor VM / host | 1.21 / 1.77 | 4.61 / 5.09 | FAILED / N=0 | 11.39 / 13.73 |
| Firecracker / Ubuntu | 7.79 / 10.13 | 8.51 / 9.11 | 9.78 / 9.87 | 10.81 / 13.49 |
| QEMU q35 / Ubuntu | — | 8.12 / 8.23 | 9.50 / 10.78 | 10.20 / 11.63 |
| QEMU microvm / Ubuntu | — | 10.33 / 10.47 | 11.59 / 14.07 | 13.04 / 14.06 |

QEMU 两行使用同一完整 Ubuntu 模板，新增 60/60 个正式任务样本全部通过，各格 N=10、3 次预热；版本自检未另列计时（—）。QEMU 与前面 pVisor/Firecracker 是独立批次，图不合并分布。

**修复任务的端到端等待更短，进入环境后的工具并未同样更快。** pVisor VM 修复从启动到结果为 4.61 秒，Firecracker/Ubuntu 为 8.51 秒；单独计工具与校验则分别为 4.02 / 2.30 秒。前者减少完整 OS 开机等待，后者暴露了工具与文件路径的成本。若把 VM 留着执行多个任务，启动成本会摊薄，应优先看 worker；此处未测长驻池或长期吞吐。

| Backend | Repair worker P50/P95 s | Peak tree RSS P50/P95 MiB |
|---|---|---|
| Native / Fedora | 0.48 / 0.50 | 169.28 / 190.85 |
| pVisor staged | 0.67 / 0.72 | 209.52 / 248.59 |
| pVisor VM / host | 4.02 / 4.49 | 729.46 / 756.93 |
| Firecracker / Ubuntu | 2.30 / 2.69 | 1743.02 / 1766.67 |
| QEMU q35 / Ubuntu | 2.75 / 2.79 | 1862.13 / 1876.98 |
| QEMU microvm / Ubuntu | 2.81 / 2.88 | 1844.82 / 1881.06 |

| Phase | Native P50 ms | staged P50 ms | pVisor VM P50 ms | Firecracker Ubuntu P50 ms | QEMU q35 P50 ms | QEMU microvm P50 ms |
|---|---|---|---|---|---|---|
| inspect | 3.1 | 18.6 | 74.4 | 17.1 | 16.4 | 18.7 |
| search | 2.2 | 2.4 | 20.8 | 12.9 | 7.6 | 7.4 |
| python-tests | 28.0 | 34.2 | 149.7 | 45.3 | 47.2 | 47.8 |
| rust-tests | 96.5 | 206.7 | 822.3 | 981.6 | 1299.9 | 1327.5 |
| node-install | 221.9 | 260.9 | 2210.9 | 876.7 | 956.7 | 975.9 |
| node-tests | 126.3 | 140.3 | 595.2 | 389.6 | 421.9 | 423.1 |
| diff | 0.9 | 2.8 | 18.6 | 1.2 | 1.7 | 1.4 |

pVisor VM 中最大阶段为 `node-install`，中位约 2.21 秒。这给出下一步优化的具体路径；各阶段中位数不能相加得到总中位数，也不能把 ext4/virtio-fs 的差异单独解释为全部原因。

修复列包括检查项目、rg 搜索、修复 Python、Python/Rust/Node 测试、32 个本地 npm 依赖安装和 diff。CLI 列使用真实客户端与固定本地模型响应，要求实际通过的测试结果回到模型请求并正常完成，不是模拟 CLI 或真实模型推理。staged/VM 每次还检查原工作区没有被修改、Bundle completed 且实际执行器符合请求。所有组统一使用工作区私有临时目录，避免 hostroot 的只读 `/tmp` 干扰工具；最初错误配置的链接失败保留在诊断归档中，不混入正式分布。

Claude/VM 预检仍在初始化超过 90 秒，正式 N=0；没有通过工具闭环，不能写成完整兼容。Firecracker/Ubuntu 的 Claude/Codex 数据来自修复串口提示及终端控制序列干扰后的同参数补测，各 10 次。补测单独归档，不合并主批次中的旧客户端样本；原采集失败记录保留。

Codex 使用统一内部 `danger-full-access`，外层运行时提供表明的边界，默认双层沙箱兼容性未测。每格 N=10 仅提供首版预算与重复性，P95/P99 接近最大值，不支持长期尾延迟或真实模型成功率保证。RSS 每 20 ms 求进程树之和，Firecracker 包含独立 DNS/NAT 辅助进程，QEMU 使用进程内用户态网络，可能重复统计共享页和漏掉短峰；配置 16 GiB 不等于驻留 16 GiB，也不能从短任务推算并发容量。

[逐样本 CSV](../../assets/benchmarks/full-ubuntu-20261004/samples.csv) · [分布与阶段计时](../../assets/benchmarks/full-ubuntu-20261004/summary.json) · [方法与复现](methodology.md#full-ubuntu) · [运行证据](../../assets/benchmarks/full-ubuntu-20261004/ubuntu-workflows-private-tmp-20261004/evidence.tar.gz) · [Ubuntu 客户端补测证据](../../assets/benchmarks/full-ubuntu-20261004/ubuntu-clients-console-v2-20261004/evidence.tar.gz)

完整 Ubuntu 的 QEMU 修复任务为 q35 **8.12 秒**、microvm **10.33 秒**，内部工具分别 **2.75 / 2.81 秒**。无镜像 pVisor 降低新任务总等待，但其 4.02 秒工具时间仍高于这些完整 Ubuntu 路径；长期复用环境时，这个差距值得优先优化。更换为 microvm 没有在本配置下自动改善完整任务。网络、设备和 CPU 暴露方式仍有差别，不把全部差值归因于 block device 或 FUSE。

[QEMU samples and cohorts](../../assets/benchmarks/full-ubuntu-qemu-20261004/manifest.json) · [QEMU distributions](../../assets/benchmarks/full-ubuntu-qemu-20261004/summary.json) · [QEMU evidence](../../assets/benchmarks/full-ubuntu-qemu-20261004/ubuntu-qemu-complete-20261004/evidence.tar.gz) · [QEMU method](methodology.md#full-ubuntu-qemu)

### 历史受控环境：同工具制品与裁剪参考 VM {#reference-env}

以下批次用相同工具制品隔离部分版本差异；Firecracker/QEMU 使用裁剪 Linux 6.12.109 与静态 init，不启动完整发行版。pVisor 使用工具目录，经 virtio-fs 共享，不使用镜像。保留这些数据解释 Docker 与最小 VM 的性能水位；完整 Ubuntu 的实际部署成本和无镜像 hostroot 路径见[上节](#full-ubuntu)。

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
