# 基准与对比

pVisor 无镜像 VM 启动约 **110 ms**；同机 Firecracker 启动完整 Ubuntu 为已准备 **5.64 秒**、首次 **9.25 秒**。这减少了短任务的开机等待，工具执行成本与 CLI 兼容性仍需分别看。Docker/QEMU 的历史对照、macOS 数据以及大文件 apply 等短板全部保留。

测量公开复现脚本、样本与失败情况；方案对比标明官方来源和未测范围。方法、环境与样本数见[方法](methodology.md)。

## 已有测量

macOS 与 Linux 的测试数据同时保留，按平台、测量日期和制品分别归档。下表为此前 2026-10-03 的 VM 测量；新增结果追加记录，保留此前批次及原始证据。

| 宿主平台 / 后端 | 测量 | 已观察到的结果 | 条件与完整证据 |
|---|---|---|---|
| macOS ARM64 / HVF | CLI → VM 工作负载就绪 | P50 84.35 ms；P95 112.14 ms | Apple M4，两轮 P0 受控制品、裁剪 firmware，2 vCPU / 128 MiB，N=100；已准备 rootfs、热宿主缓存；[启动延迟与历史批次](startup.md) |
| macOS ARM64 / HVF | VM 冷 RAM 回收 | 2 GiB 配置的 RAM 代理约降低 60% | Apple M4，两台 VM、每台 64 MiB 重复冷数据、ready 后 60–90 秒；首次读取变慢且 footprint 增加；[内存收益与使用代价](vm-memory/index.md) |
| Linux x86_64 / KVM | CLI → VM 工作负载就绪 | P50 172.69 ms；P95 180.37 ms | Ryzen 7 9700X，libkrunfw 5.5.0，Fedora rootfs，2 vCPU / 128 MiB，N=100；已准备 rootfs、热宿主缓存；[启动延迟](startup.md#linux-results) |
| Linux x86_64 / KVM | VM 生命周期 | pause P50 0.24 ms；offload 22.98 ms | Ryzen 7 9700X，256 MiB / 2 vCPU / raw，N=30；[正确性与完整分布](vm-memory/index.md#linux-lifecycle) |
| Linux x86_64 / KVM | 完整 VM 快照 | raw snapshot 保存 P50 712 ms、恢复至 heartbeat 933 ms | Ryzen 7 9700X，256 MiB / 2 vCPU，N=10、每次恢复两个 fork；[保存、恢复与压缩数据](vm-memory/index.md#linux-snapshot) |

这些数据覆盖不同负载与阶段，不构成 macOS 与 Linux 的速度排名。共享冷页 pager 当前仅支持 macOS/ARM64；Linux 的 offload 与完整快照是独立测量。

每个数字对应明确的阶段和负载；启动表包含完整 CLI 到标记路径，冷 RAM 代理不是整机物理内存。新产品基准见下方首版结果，未测指标单独标明。

## 默认部署方式：无镜像与完整发行版 {#full-ubuntu}

最新 Linux 对照使用 pVisor `--rootfs host` 与官方 Ubuntu 26.04.1 LTS，启动 N=30、完整任务 N=10；工具、内核与系统服务差异写入方法。Mac 的约 84 ms 启动结果继续独立保留，本轮未测 Mac 完整 Agent Env。

| 问题 | pVisor | 完整 Ubuntu / Firecracker |
|---|---|---|
| [新建环境](startup.md#full-ubuntu) | VM P50 110 ms / P95 122 ms | 已准备 P50 5.64 s / P95 6.01 s; 首次启动 P50 9.25 s |
| [修复测试：启动到结果](agent-tasks.md#full-ubuntu) | VM 4.61 / 5.09 s; staged 0.72 / 0.78 s | 8.51 / 9.11 s |
| [修复测试：工具内部](agent-tasks.md#full-ubuntu) | VM 4.02 / 4.49 s | 2.30 / 2.69 s |
| [Codex 工具闭环](agent-tasks.md#full-ubuntu) | VM 11.39 / 13.73 s | 10.81 / 13.49 s |

启动差距反映是否需要开机进入完整发行版，不是纯 VMM 优劣。频繁新建短任务环境可以减少秒级等待；长期复用环境则应关注工具内部性能、文件成本和客户端兼容性。上述任务单元格为 P50/P95，10 次样本仅说明本轮水位，不是尾延迟保证。首次启动的 Ubuntu 尚未安装完整 Agent 工具，下载与安装不含在启动时间中。

[方法与证据](methodology.md#full-ubuntu)

同一完整 Ubuntu 的 QEMU 补测也已完成：q35 / microvm 启动 P50 为 **5.43 / 7.67 秒**，完整修复测试为 **8.12 / 10.33 秒**。每格 N=10、共 80/80 通过，独立于上表批次；[完整结果](agent-tasks.md#full-ubuntu)同时给出内部工具时间和真实 CLI 闭环，避免只用启动数字判断性能。

## 历史受控数据水位：Docker 与最小 VM {#reference-position}

下表使用相同工具制品；Firecracker/QEMU 为裁剪内核、直接 init，pVisor 使用目录。适合观察工具与 Docker 的受控差距；默认无镜像 / 完整 Ubuntu 对照以[上节](#full-ubuntu)为准。旧失败及旧性能数值保留在原批次，不代表新配置的结果。

下表将不同问题分别放在熟悉的尺度上；不把较小的启动数字替代完整任务。新环境 Linux 1,410 个有效样本、另有 320 个资源/退出补测，旧 macOS/Linux 与首版 3,375 个产品样本继续独立保留。

| 问题 | 熟悉基线 | pVisor 水位 | 对用户的含义 |
|---|---|---|---|
| [新 VM 首条输出](startup.md#reference-startup) | Firecracker 74 ms；QEMU microvm 88 ms；Docker 90 ms | VM 86 ms | microVM 百毫秒量级，适合已准备环境的短任务；不含下载 |
| [完整环境修复/测试](agent-tasks.md#reference-env) | 原生 0.50 s；Docker 0.90 s；QEMU microvm 1.85 s | staged 0.70 s；VM 3.97 s | stage 增量约 0.20 s；VM 约为 Docker 4.4 倍，仍需优化 |
| [文件读取/遍历](filesystem.md#reference-fs) | Docker 64 MiB 读 33 ms、2,048 文件遍历 5 ms | staged 49 ms / 180 ms | 读取成本较小；频繁扫描小文件会积累等待 |
| [完整任务内存](methodology.md#reference-resources) | 原生 RSS 169 MiB；Docker 含 daemon 280 MiB | staged 227 MiB；VM 722 MiB | 任务实际驻留量级，不是 Agent 并发容量；RSS 有共享页重复统计 |
| [真实客户端工具闭环](agent-tasks.md#reference-env) | Claude/Docker 1.23 s；Codex/Docker 6.26 s | staged 1.07 s / 2.25 s；Codex/VM 10.93 s；Claude/VM 失败 | 必须按客户端选环境，初始化与兼容性会压过启动差距 |
| [改动合入](apply.md) | Git patch apply：10 文件 0.73 ms，1,000 文件 13.61 ms | 约 15 ms / 836 ms；100,000 文件约 5.5 min | 小批量可交互；额外协议有成本，大批量是短板；Git 未提供相同事务语义 |
| [本地请求](network.md) | native HTTP 0.95 ms | proxy 1.24 ms；VM 3.83 ms | +0.29 / +2.88 ms 本地预算，不等于公网模型时延 |
| [审查与监督](supervision-cost.md) | 常见 Git/diff 审查流程；非同机性能对照 | 20 项机器审查/合入/drop 约 25 ms | 机器操作成本已测；人类节省分钟数未测 |
| [并发](density.md) | native/Podman 同样 128 路 idle 全通过 | staged idle 128 路全通过；safe 有失败 | 空闲探针通过不等于能跑 128 个完整 Agent；需要实任务容量测量 |

数字均为对应批次 P50。新对照共用相同工具与两核预算，VM 2 vCPU、完整任务 16 GiB；Docker 为 rootless Engine + bind mount，VM 文件路径不同。它们提供实际成本定位，并非相同安全边界、文件系统或纯 VMM 的排名。macOS 新完整环境和 Docker/Firecracker/QEMU 对照未测，已有 HVF 数据继续在独立 macOS 章节保留。

[完整方法、固定制品与失败证据](methodology.md#reference-env)


## 基准

[VM 内存回收、offload 与完整快照](vm-memory/index.md) · [启动延迟](startup.md) · [文件系统开销](filesystem.md) · [网络开销](network.md) · [apply/drop 成本](apply.md) · [端到端任务](agent-tasks.md) · [监督成本](supervision-cost.md) · [并发密度](density.md) · [隔离有效性](isolation-tests.md) · [回放保真度](replay-fidelity.md)

## 对比

[Agent 自带沙箱](compare-agent-sandboxes.md) · [Docker/devcontainer](compare-containers.md) · [云端沙箱](compare-cloud-sandboxes.md) · [隔离基座](compare-runtimes.md) · [RL 基础设施](compare-rl-infra.md)

## 2026-10-04：产品性能首版

首版给出 Linux 同机原生、暂存、safe、VM 与 OCI 对照。host 工具耗时接近原生；暂存对连续读/离线 npm 增加数十毫秒，小文件访问代价更大。apply 从十文件约 15 ms 增长到十万文件约 5.5 分钟。性能与失败情况一起公开，便于按工作负载选择。

当前数据支持小批量改动审查和本机工具执行：连续读取与离线安装的暂存成本较小，千文件提交已接近秒级。目录遍历、密集小文件写入、大规模提交，以及 safe 高并发稳定性仍有明显优化空间。并发数据来自空闲探针，不能直接当成完整 Agent 的运行容量。

| 主题 | 结果与含义 |
|---|---|
| [文件系统](filesystem.md) | 64 MiB 读取 worker P50：原生 32.66 ms、staged 40.76 ms；2,048 文件 metadata：4.84 → 149.60 ms |
| [网络](network.md) | 本机 HTTP 小请求 P50：原生 0.95、host proxy 1.24、VM 3.83 ms；不是公网 API 时延 |
| [apply/drop](apply.md) | 10/1,000/100,000 文件 apply P50 约 15 ms / 0.84 s / 5.5 min；公开冲突拒绝和 SIGKILL 恢复 |
| [并发密度](density.md) | 128 路 idle probe：native/host/staged/Podman 全完成；safe 638/640，OCI 大工具 rootfs 触发 tmpfs 配额 |
| [Agent 工具闭环](agent-tasks.md) | 真实 Claude/Codex CLI，72/72 受控修复通过；真实模型成功率未测 |
| [监督流程](supervision-cost.md) | 审查 20 项、合入 10 项/drop 10 项，机器耗时约 25 ms；人类分钟数未测 |
| [隔离](isolation-tests.md) | 5 配置、视图外路径/socket/别名探针，核对真实宿主影响 |
| [回放](replay-fidelity.md) | 6 adapter、360/360 合成前缀；发现并修复 prepare-only 副作用 |

各页分别解释 macOS 和 Linux；本轮未采集 macOS 新工作负载。真实模型、人工实验、云端时延与价格实测仍未测，不用代理指标补造数据。方法、固定制品摘要、逐样本证据与复现入口见[首版协议](methodology.md#product-v1)。
