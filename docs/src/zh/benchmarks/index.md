# 基准与对比

每份测量与对比都可复现，作为「可核对」承诺的证据来源；方法、环境与样本数见[方法](methodology.md)。

## 首屏数字

| 指标 | 值 | 来源 |
| --- | --- | --- |
| VM 执行器 guest 就绪延迟（仅 guest init 阶段，Apple M4/HVF） | p50 ≈ 114 ms（Rust init） | [启动延迟](startup.md) |
| pVisor 端到端冷启动（host／container／VM） | 建设中 | [启动延迟](startup.md) |
| 文件系统开销 | 建设中 | [文件系统开销（规划中）](filesystem.md) |
| 端到端 Agent 任务开销 | 建设中 | [端到端任务（规划中）](agent-tasks.md) |

## 基准

[启动延迟](startup.md) · [文件系统开销（规划中）](filesystem.md) · [网络开销（规划中）](network.md) · [apply/drop 成本（规划中）](apply.md) · [端到端任务（规划中）](agent-tasks.md) · [监督成本（规划中）](supervision-cost.md) · [并发密度（规划中）](density.md) · [隔离有效性（规划中）](isolation-tests.md) · [回放保真度](replay-fidelity.md)

## 对比

[Agent 自带沙箱（规划中）](compare-agent-sandboxes.md) · [Docker/devcontainer（规划中）](compare-containers.md) · [云端沙箱（规划中）](compare-cloud-sandboxes.md) · [隔离基座（规划中）](compare-runtimes.md) · [RL 基础设施（规划中）](compare-rl-infra.md)

!!! note "建设中"
    首屏的「端到端冷启动」「文件系统开销」「端到端 Agent 任务开销」尚无数据；范围与验收标准见[启动延迟](startup.md)、[文件系统开销（规划中）](filesystem.md)与[端到端任务（规划中）](agent-tasks.md)。

