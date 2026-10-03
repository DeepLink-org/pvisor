# 基准与对比

每份测量与对比都可复现，作为「可核对」承诺的证据来源；方法、环境与样本数见[方法](methodology.md)。

## 已有测量

| 测量 | 已观察到的结果 | 条件与完整证据 |
|---|---|---|
| VM guest 就绪阶段 | p50 约 114 ms | 仅 Rust guest init，Apple M4/HVF；[启动延迟](startup.md) |
| VM 冷 RAM 回收 | 2 GiB 配置的 RAM 代理约降低 60% | 两台 VM、每台 64 MiB 重复冷数据、ready 后 60–90 秒；首次读取变慢且 footprint 增加，见[内存收益与使用代价](vm-memory/index.md) |

每个数字对应明确的阶段和负载；guest init 不是端到端启动，冷 RAM 代理不是整机物理内存。其余基准按下方标注推进，尚无数据的项目不提供性能结论。

## 基准

[VM 内存节约、物理压力与性能开销](vm-memory/index.md) · [启动延迟](startup.md) · [文件系统开销（规划中）](filesystem.md) · [网络开销（规划中）](network.md) · [apply/drop 成本（规划中）](apply.md) · [端到端任务（规划中）](agent-tasks.md) · [监督成本（规划中）](supervision-cost.md) · [并发密度（规划中）](density.md) · [隔离有效性（规划中）](isolation-tests.md) · [回放保真度](replay-fidelity.md)

## 对比

[Agent 自带沙箱（规划中）](compare-agent-sandboxes.md) · [Docker/devcontainer（规划中）](compare-containers.md) · [云端沙箱（规划中）](compare-cloud-sandboxes.md) · [隔离基座（规划中）](compare-runtimes.md) · [RL 基础设施（规划中）](compare-rl-infra.md)

!!! note "建设中"
    首屏的「端到端冷启动」「文件系统开销」「端到端 Agent 任务开销」尚无数据；范围与验收标准见[启动延迟](startup.md)、[文件系统开销（规划中）](filesystem.md)与[端到端任务（规划中）](agent-tasks.md)。

