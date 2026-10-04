# 基准方法与环境

所有基准遵守同一套协议：

- 写明硬件、操作系统、内核或 macOS 版本、FUSE 实现、pVisor 版本与提交号；
- 给出 p50、p95、p99 与样本数；
- 提供可一条命令复现的脚本（放在仓库 `benchmark/` 下），报告使用 `pvisor-benchmark/v1` schema；
- 每个对照组写明配置，不与「未调优的对手」比较；
- 结果按日期保留，不覆盖旧数据。

## macOS 与 Linux 数据保留

macOS/HVF 与 Linux/KVM 的数据融入对应主题的 benchmark 文档并同时保留，在[基准总览](index.md)按宿主操作系统、架构和后端列出。每轮保存日期、硬件、配置、源码状态、二进制及 firmware 摘要和原始样本；重测使用新目录，保留旧报告、图表和 JSON/CSV。摘要页面可以引用新批次，但必须保留旧批次入口。

不同平台分别计算分布，不合并样本。只有负载、计时起止、缓存状态和参数一致时才做跨平台对比；未测项目标为未测，不支持的功能写明平台限制。Linux 的 offload 数据与 macOS 的共享冷页回收数据分别解释。

当前数据：[macOS 启动延迟](startup.md)、[macOS 共享冷页回收](vm-memory/index.md)、[Linux 生命周期与完整快照](vm-memory/index.md#linux-methodology)。

## 环境清单

每份报告开头附下面这张表：

| 项目 | 示例 |
| --- | --- |
| 日期 | 2026-10-02 |
| pVisor 版本与提交 | `0.x.y` / `abc1234` |
| 硬件 | Apple M4，16 GiB；或 CPU 型号、核数、内存 |
| 操作系统与内核 | macOS 26.x；或 Ubuntu 24.04，Linux 6.8 |
| 文件系统与 FUSE 实现 | APFS + macFUSE 5.x；或 ext4 + libfuse 3.x |
| 执行器与参数 | `--executor vm --overlaynet auto` |
| 样本数与预热 | 100 次，丢弃前 5 次 |

## 对照组

- 对照组使用该方案文档推荐的配置，并写明版本与参数；
- 拆分出 pVisor 自身的开销：例如同一执行器下 `--filesystem host` 与暂存的差，而不是只给端到端总数；
- 只有部分阶段的数据（例如只测 guest init）时，标题必须写明测的是哪个阶段，不能写成端到端结论。

## 从现有测量入口开始

```bash
just benchmark
just benchmark nightly target/pvisor-benchmark/nightly
just benchmark-compare target/pvisor-benchmark/candidate/raw-report.json target/pvisor-benchmark/main/raw-report.json
# Linux：启动与资源占用矩阵
just benchmark-startup --warmups 10 --samples 100
```

`just benchmark` 测最小 host Run 与读取 Run Bundle 的进程级成本：smoke 为 2 次预热/10 个样本，nightly 为 10 次预热/50 个样本。两者使用 `pvisor-benchmark/v1`。`benchmark-startup` 使用独立的 `startup.json`/`startup.md`，不会伪装成同一个报告 schema；默认入口为 3 次预热/30 个样本，上例显式覆盖。

构建、镜像下载和 rootfs 准备不计入现有 startup 样本；报告必须标明这一边界。无法通过前置探测的 executor 单独列为 SKIP，并保留 stderr；成功测量要求命令成功且 Bundle 为 completed/零退出。不能把失败、跳过或控制降级算成更快的成功样本。

## 解释与归档

同机、同套件、同输入比较 candidate 与 baseline。`benchmark-compare` 默认以 15% 为回归阈值，除非显式启用 `--fail-on-regression`，结果只作报告。保存原始样本、摘要、完整参数、输入摘要与提交号；区分冷镜像、热磁盘缓存与热页缓存，记录后台负载和电源状态。

2026-10-04 首版提供文件系统、网络、apply、并发密度、受控 Agent 工具闭环、机器审查流程、隔离和回放前缀数据。真实模型成功率、人工监督分钟数、云端延迟与 macOS 新工作负载仍未测；每页单独解释范围。

`benchmark/pvisor/vm_ready.py` 使用 `pvisor-vm-readiness/v1`，分别测量负载输出就绪和 CLI 完成，并用独立诊断批次拆分宿主时间；完整协议见[启动延迟](startup.md)。

## 2026-10-04 产品基准首版 {#product-v1}

首版显示 host 工具运行接近原生；staged 连续读与离线 npm 增加数十毫秒，小文件访问增加约 150–190 ms；VM 多数小任务在 0.5–1 秒量级。10/1,000/100,000 文件 apply 分别约 15 ms、0.84 秒、5.5 分钟。数字对应下列本机配置，不是对所有项目的保证。

### 环境与身份

| 项目 | 本轮记录 |
|---|---|
| 日期 / 平台 | 2026-10-04，Linux x86_64 |
| CPU / 内存 | Ryzen 7 9700X，8 核/16 线程，MemTotal 31,980,420 KiB（约 30.5 GiB） |
| 系统 | Fedora 44，Linux 7.2.8-200.fc44.x86_64 |
| 文件系统 | home btrfs；Linux FUSE；默认 /tmp 为带用户配额的 16 GiB tmpfs |
| pVisor | 0.3.0；初始工作树基于 8c63f4f0，存在并行开发和未提交改动，原始 source_status 保留 |
| 测量 CLI SHA256 | `592b01a0683b8eeedc4750d04b7400820034fa84e630c0dcecc9eb742b800383` |
| libkrunfw | 5.5.0；SHA256 `6df51f65d7f99fc22215e69a4236c770b1588ceb6777eca014f92b366517d237` |
| 修复后 replay SHA256 | `a69f7a6e40e4bbc71fb1e5af6c957fd7c59c194c3e719c4e497c492044b33f32` |
| 容器 / Agent | rootless Podman + crun 1.28；Claude 2.1.128、Codex 0.160.0；各轮版本与镜像 ID 在报告 |
| 后台负载 | 共享桌面、编辑器、并行开发；不是专用空闲性能机。报告记录轮前/轮后 load；补充批次有一个大文件 apply 同时运行 |

### 比较配置

| 名称 | 配置与可比较范围 |
|---|---|
| native | 直接执行同一工具/输入 |
| host | 宿主进程，OverlayNet off；允许访问宿主 |
| staged | host + staged workspace；本身不限制视图外宿主访问 |
| safe | rootless 文件沙箱 + stage + proxy；只读 Rust 工具链 share |
| vm | libkrun/KVM，2 vCPU；工具主批次 1 GiB、host rootfs `/` + stage；隔离另用准备好的 rootfs；idle probe 128 MiB |
| podman | 预制本机 OCI 工具镜像，工作区 bind mount，crun，普通任务 network none、网络实验 host network |
| container | pVisor OCI + crun，工作区 writable mount，network none/host；每 Job 私有 rootfs 复制计入 wall |

不能把全部组别称为同样的隔离级别。每个 pVisor 成功样本要求 completed/零退出 Bundle、实际执行器身份与请求一致；staged/safe/VM 还检查暂存生效。文件数据、工具输出或网络摘要正确才进入 rows。

### 采样与解释

热宿主缓存，不主动驱逐。镜像构建/导入、输入准备、代码编译的构建阶段不进入启动计时；cargo 工作负载自己的编译当然计入工具时间。filesystem/network 每格 3 次预热/30 次采样，随机组别顺序。apply 为 30/10/3 次（10/1,000/100,000 文件）；前两组 1 次预热、最大组无预热。density 每格 5 个批次，agent/replay 每任务 3 次，supervision 30 次，这些套件无预热。性能表使用线性插值 P50/P95/P99；嵌套请求/并发 Job 可能相关，未给独立样本置信区间。

wall 是一项命令从启动到退出的总成本，worker 是内部工具运行与校验成本；两者分别呈现。失败 batch 保留原因和尝试/成功数，内存 guard 不算成功或零成本。不可用的 Docker daemon、缺少工具的镜像、VM 地址空间失败分别解释，不能当作竞争方案慢或产品兼容失败。

### 原始数据

[批次清单](../../assets/benchmarks/product-v1-20261004/manifest.json)列出每个报告及摘要；[逐样本 CSV](../../assets/benchmarks/product-v1-20261004/samples.csv)便于重算分布；[证据归档](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz)保留完整 JSON、脚本快照、Bundle、错误日志和 SIGKILL ledger。payload 与 rootfs 可按脚本重建，未在 docs 复制数 GB 环境。已有 macOS 与 Linux VM 批次完整保留；新产品工作负载只在 Linux 测，不合并跨平台样本。

本轮 `just lint` 通过；`just test` 为 Rust 1,075 项通过 / 8 项跳过、Python 84 项通过 / 16 项跳过。新增 benchmark 测试单独运行 19 项通过；STAGE 语义规格 14/14 PASS，人工审阅状态仍为 UNREVIEWED，不等同于人类批准。大规模 syscall trace 与非崩溃 apply ledger 保留在本机 target 原始目录，公开归档保留报告、Bundle、诊断和崩溃 ledger。

用 `python3 benchmark/pvisor/summarize_product_v1.py <batch>/report.json --output-csv /tmp/summary.csv` 重算；公开[分布汇总 CSV](../../assets/benchmarks/product-v1-20261004/summary.csv)按批次分组。

本轮性能针对固定制品，不能代表之后的并行改动或其他发行构建；源码并非干净提交。全仓 lint/test 校验的是当时工作树，固定 CLI 的功能另由 benchmark 与 STAGE 规格验证。

## 熟悉基线与完整 Agent Env：Linux 同机对照 {#reference-env}

这轮回答三个不同问题：环境首条输出有多快、工具能否正常运行、客户端能否完成修复闭环。启动、文件操作、完整工具任务与真实 CLI 分开计时，旧 macOS/Linux 数据原样保留；新的 Docker/Firecracker/QEMU 对照只在 Linux 采集。

| Item | Recorded value |
|---|---|
| Host | Fedora 44 / Linux 7.2.8-200.fc44.x86_64; Ryzen 7 9700X, 8C/16T; 30.5 GiB |
| Tools | Python 3.14.7; Node 24.18.0; Rust/Cargo 1.98.1; Claude 2.1.128; Codex 0.160.0 |
| References | Docker Engine 29.7.2 rootless; Firecracker 1.13.1 PCI; QEMU 10.2.2 q35 / microvm |
| pVisor SHA256 | `1a2db5ad015c5ace40b3c96c7dd0dc94a08b8893de35150bd909554286e2cd3c` |
| Guest kernel | Linux 6.12.109; reference kernel/config shared by Firecracker/QEMU |
| VM RAM / CPU | 128 MiB shell probe; 16 GiB complete environment; 2 vCPU |
| Storage | btrfs host; Docker bind mount; pVisor staged virtio-fs; reference private ext4 |
| Sampling | 30 samples + 3 warmups per available case; random order; hot host caches |
| Effective samples | 1,410 passed; 47/48 mode/backend configurations available |
| Identity/source | Frozen binary and harness hashes; parallel dirty development recorded, not a clean commit |


Docker/native 不设置硬内存上限；所有 payload 与 VMM 使用宿主物理核心 0、1，Docker 私有 daemon 和容器内工具单独绑定，Rust 编译 `-j2`。Docker 的启动 daemon 已运行，其一次性启动不在任务时间内。VM RAM 由 Run Bundle 或 VMM 配置核对；配置容量与实际 RSS 分开报告。宿主是共享桌面，仍有其他核心上的并行开发，缓存/磁盘竞争不完全隔离，未锁定频率，因此不把个位数百分比当作可靠排名。

QEMU q35 和 microvm 同时给出，避免以 PC 设备默认成本代替其最低路径。microvm 关闭可选传统设备，KVM/host CPU、virtio-blk；Firecracker 启用 PCI、无 API、无 jailer。三者直接启动裁剪内核与静态 init，没有 systemd/SSH/cloud-init。[QEMU microvm 官方说明](https://www.qemu.org/docs/master/system/i386/microvm.html)、[Firecracker 1.13.1 配置](https://github.com/firecracker-microvm/firecracker/blob/v1.13.1/docs/getting-started.md)、[Docker rootless](https://docs.docker.com/engine/security/rootless/)是参数依据。这是开发性能基线，不是生产安全部署评测；Firecracker/QEMU 没有成为 pVisor executor。

pVisor 内置 firmware 与参考内核版本相同，配置并非相同；它还有 staged 视图、执行观察与 Run Bundle。block/ext4 与 virtio-fs 的文件路径、客户端初始化和退出协议都不同，因此完整命令差值不能解释为单纯 VMM 开销。

### 计时、正确性与失败

`ready_ms` 是首条输出或工具自检结果，`result_ms` 是通过校验的任务结果；两者都包含对应运行时启动。`completion_ms` 是完整退出，`prepare_ms` 单独记录工作区/私有磁盘准备。主矩阵退出采样存在最高约 50 ms 的轮询粒度，主要结论使用 stdout 事件计时；精确退出补测单独归档，不混入主批次。文件操作表使用任务内部 `worker_ms`，不含新环境启动。

每组先检查一次能力，3 次预热不入分布，30 轮按固定 seed 随机后端顺序。正确性要求零退出、恰好一个结果、正确 mode、真实 grade 返回、文件内容及原目录状态、VM 无 panic；pVisor 还核对 completed Bundle、实际 VM/host executor 和 staged 观察。失败保留日志，排除性能分布；Claude/VM 90 秒初始化超时令正式 N=0，runner 在完成其他组后以非零状态退出。Codex 统一内层 `danger-full-access`，默认内部沙箱兼容性不在这张表的通过条件中。

真实 Claude/Codex 连接同一环境内的本地受控响应服务。无真实推理、公网时延、真实 tokens 或账单。每组 30 次反映一个任务的重复性，不是 30 种独立缺陷；P95/P99 是本批次的描述统计，不是长期尾延迟或成功率置信区间。

### 资源审计 {#reference-resources}

20 ms 抽样 RSS 求和，不是 PSS，也不是严格 cgroup 峰值，可能漏掉短峰和重复统计共享页。主矩阵最初只追踪 Docker CLI/daemon 后代，审计发现容器 shim 被 systemd 接管，漏掉容器工具 RSS。公开主报告已标为部分范围，不能拿其约 102 MiB 与 VM 排名。单独补测用确切 container ID 追踪 shim 与任务，并保留 Docker 进程亲缘和 affinity 证据；资源表在补测报告中独立呈现。

#### 完整工具任务的资源补测

同配置、10 次/组、1 次预热，按容器 ID 跟踪 Docker shim，RSS 峰值中位数如下。Docker 包含专属 daemon 的固定占用，本批次开始时约 75 MiB；这不是单个容器的增量内存，不能直接做除法估算 Agent 密度。VM 内存包含 VMM/实际已触及 guest 页，不等于配置的 16 GiB。任务期间没有新建环境或导入镜像与测量并发。

| Backend | N | Peak RSS P50/P95 MiB |
|---|---|---|
| Native | 10 | 168.8 / 171.0 |
| pVisor host | 10 | 178.5 / 180.7 |
| pVisor staged | 10 | 226.8 / 242.5 |
| pVisor VM | 10 | 722.1 / 734.9 |
| Docker rootless | 10 | 280.3 / 320.5 |
| Firecracker PCI | 10 | 762.7 / 771.4 |
| QEMU q35 | 10 | 812.3 / 818.9 |
| QEMU microvm | 10 | 803.0 / 818.6 |


native/staged 为约 0.17/0.22 GiB，Docker 含 daemon 约 0.27 GiB，VM 为约 0.71 GiB。pVisor VM 的实际占用与本轮参考 VM 同属不到 1 GiB 的量级；这个短任务不覆盖长时间大仓库峰值，也不测共享页节省。RSS 范围不同，不能据此排出严格物理内存效率排名。

[资源报告](../../assets/benchmarks/reference-env-20261004/followups/reference-resources-20261004/report.json) · [逐样本](../../assets/benchmarks/reference-env-20261004/followups/reference-resources-20261004/samples.csv) · [进程与 affinity 审计](../../assets/benchmarks/reference-env-20261004/followups/reference-resources-20261004/docker-process-audit.json) · [证据](../../assets/benchmarks/reference-env-20261004/followups/reference-resources-20261004/evidence.tar.gz)


### 环境部署与复现

从已安装工具构建 rootfs，再导入相同 Docker 镜像、制作 ext4。Fedora 44 自动工具布局为 Python 3.14/Node 24；其他系统可传 `--tools-rootfs` 等价预制 base。需要 KVM、FUSE、Docker rootless、Firecracker、QEMU、Claude/Codex、Rust musl target、GCC/e2fsprogs；内核构建需要干净 Linux 6.12.109 源码。只复制工具与假凭据，不复制登录认证。

先按 rootless Docker 官方方式启动本用户的私有 daemon，使用短 socket 路径；将下例 `12345` 替换成该 daemon 的实际宿主 PID，选择允许的两个物理核心。脚本核对 owner/socket 后只绑定该 daemon，不修改系统 Docker。所有输出目录须为新目录；先用 `--samples 1 --warmups 0` 试跑。内核/工具下载、内核编译、镜像准备不计入任务时间，离线准备和每次克隆成本另有记录。

```bash
bash benchmark/pvisor/prepare_reference_kernel.sh \
  /absolute/path/to/clean/linux-6.12.109 target/reference-kernel-new
python3 benchmark/pvisor/prepare_reference_env.py \
  --binary /absolute/path/to/pinned/pvisor --output target/reference-env-new \
  --kernel-elf target/reference-kernel-new/vmlinux \
  --kernel-bzimage target/reference-kernel-new/arch/x86/boot/bzImage \
  --kernel-config target/reference-kernel-new/.config \
  --docker-host unix:///tmp/pvisor-reference-docker/docker.sock
python3 benchmark/pvisor/reference_baselines.py \
  --assets target/reference-env-new --binary /absolute/path/to/pinned/pvisor \
  --output target/reference-results-new \
  --docker-host unix:///tmp/pvisor-reference-docker/docker.sock \
  --docker-root-pid 12345 --cpu-affinity 0,1 --memory-mib 16384 \
  --samples 30 --warmups 3
uv run --no-project --with matplotlib python benchmark/pvisor/render_reference_baselines.py \
  --report target/reference-results-new/report.json \
  --assets target/reference-env-new --output /tmp/reference-report-new
```

归档 schema 为 `pvisor-reference-environment/v1`，不冒充旧 smoke schema。含逐样本、命令、固定脚本、guest/tool 版本、失败、最小化实际工具回传及 Run Bundle 隔离/资源证明；不复制数 GB rootfs 或继承宿主环境的凭据。固定 rootfs、工具、内核与 pVisor 摘要在报告中。自动部署脚本与复现入口见[benchmark 目录](https://github.com/DeepLink-org/pvisor/tree/main/benchmark/pvisor)。

[逐样本 CSV](../../assets/benchmarks/reference-env-20261004/samples.csv) · [分布与阶段计时](../../assets/benchmarks/reference-env-20261004/summary.json) · [运行证据](../../assets/benchmarks/reference-env-20261004/evidence.tar.gz) · [兼容性矩阵](../../assets/benchmarks/reference-env-20261004/compatibility.json)
