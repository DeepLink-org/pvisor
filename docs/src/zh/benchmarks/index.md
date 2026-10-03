# 基准与对比

每份测量与对比都可复现，作为「可核对」承诺的证据来源；方法、环境与样本数见[方法](methodology.md)。

## 已有测量

macOS 与 Linux 的测试数据同时保留，按平台、测量日期和制品分别归档。以下均为 2026-10-03 的测量；新增结果追加记录，保留此前批次及原始证据。

| 宿主平台 / 后端 | 测量 | 已观察到的结果 | 条件与完整证据 |
|---|---|---|---|
| macOS ARM64 / HVF | CLI → VM 工作负载就绪 | P50 84.35 ms；P95 112.14 ms | Apple M4，两轮 P0 受控制品、裁剪 firmware，2 vCPU / 128 MiB，N=100；已准备 rootfs、热宿主缓存；[启动延迟与历史批次](startup.md) |
| macOS ARM64 / HVF | VM 冷 RAM 回收 | 2 GiB 配置的 RAM 代理约降低 60% | Apple M4，两台 VM、每台 64 MiB 重复冷数据、ready 后 60–90 秒；首次读取变慢且 footprint 增加；[内存收益与使用代价](vm-memory/index.md) |
| Linux x86_64 / KVM | CLI → VM 工作负载就绪 | P50 172.69 ms；P95 180.37 ms | Ryzen 7 9700X，libkrunfw 5.5.0，Fedora rootfs，2 vCPU / 128 MiB，N=100；已准备 rootfs、热宿主缓存；[启动延迟](startup.md#linux-results) |
| Linux x86_64 / KVM | VM 生命周期 | pause P50 0.24 ms；offload 22.98 ms | Ryzen 7 9700X，256 MiB / 2 vCPU / raw，N=30；[正确性与完整分布](vm-memory/index.md#linux-lifecycle) |
| Linux x86_64 / KVM | 完整 VM 快照 | raw snapshot 保存 P50 712 ms、恢复至 heartbeat 933 ms | Ryzen 7 9700X，256 MiB / 2 vCPU，N=10、每次恢复两个 fork；[保存、恢复与压缩数据](vm-memory/index.md#linux-snapshot) |

这些数据覆盖不同负载与阶段，不构成 macOS 与 Linux 的速度排名。共享冷页 pager 当前仅支持 macOS/ARM64；Linux 的 offload 与完整快照是独立测量。

每个数字对应明确的阶段和负载；启动表包含完整 CLI 到标记路径，冷 RAM 代理不是整机物理内存。其余基准按下方标注推进，尚无数据的项目不提供性能结论。

## 基准

[VM 内存回收、offload 与完整快照](vm-memory/index.md) · [启动延迟](startup.md) · [文件系统开销（规划中）](filesystem.md) · [网络开销（规划中）](network.md) · [apply/drop 成本（规划中）](apply.md) · [端到端任务（规划中）](agent-tasks.md) · [监督成本（规划中）](supervision-cost.md) · [并发密度（规划中）](density.md) · [隔离有效性（规划中）](isolation-tests.md) · [回放保真度](replay-fidelity.md)

## 对比

[Agent 自带沙箱（规划中）](compare-agent-sandboxes.md) · [Docker/devcontainer（规划中）](compare-containers.md) · [云端沙箱（规划中）](compare-cloud-sandboxes.md) · [隔离基座（规划中）](compare-runtimes.md) · [RL 基础设施（规划中）](compare-rl-infra.md)

!!! note "建设中"
    首屏的「端到端冷启动」「文件系统开销」「端到端 Agent 任务开销」尚无数据；范围与验收标准见[启动延迟](startup.md)、[文件系统开销（规划中）](filesystem.md)与[端到端任务（规划中）](agent-tasks.md)。

