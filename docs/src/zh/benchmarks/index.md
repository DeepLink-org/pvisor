# 基准与对比

首版显示：pVisor 宿主执行的工具耗时接近原生，暂存小文件操作增加约百毫秒，VM 小任务多数在半秒到一秒。小批量提交可交互使用，十万文件 apply 约 5.5 分钟，是当前明确的短板。

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
